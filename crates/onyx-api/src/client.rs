//! Client-side check of an API response (M7-F2, docs/spec/delivery.md
//! §Verification). The client trusts only its own chain node: it recomputes
//! the shard id and the binding from the inputs in the response's `verify`
//! bundle, checks the rows against the root, and compares root, binding and
//! finalization height with what `getFinalizedRoot(round, shard)` returned at
//! the `finalized` block of the chain and contract it expects. It also checks
//! that the answer is the one it asked for ([`Expect`], M7-L39-F2): a valid
//! proof of another row, subject, metric or market is refused.
//!
//! Complete by construction (M7-L40-F1): the client never trusts a field the
//! server writes. What a metric *is* comes from the client's own definition
//! ([`MetricDef`]: source, recipe, schema, params), compared with the binding
//! inputs; `asOf` comes from the snapshot inside the source reference, whose
//! hash is in the binding; license and attribution come from the rights
//! record, whose hash is in the binding; a market is pinned by the client to
//! (round, shard, subject). Every response field is either checked
//! ([`BOUND_FIELDS`]) or listed as informational ([`INFORMATIONAL_FIELDS`]),
//! and each endpoint's body may carry only its own keys (M7-L41-F4): a field
//! the client did not expect is refused, not ignored.
//!
//! **The chain read is on by default** (IDX-R5-3, M7 loop 3; ADR-0023
//! Amendment 2). The operator's own per-request root read (`ChainRoots`)
//! uses the operator's node, so it adds nothing against a lying operator
//! node: only the client's own read does. So:
//!
//! - [`ClientNode::verify`] reads `getFinalizedRoot(round, shard)` on the
//!   client's own node (at the `finalized` block, after checking its chain
//!   id) and checks everything: [`Verified::chain_checked`] = true.
//! - [`verify_response`] takes that node's answer ([`ChainAnswer`]) and
//!   requires it: `None` (the node says the root is not final, or there is
//!   no answer) is refused with [`VerifyError::NotFinalOnChain`].
//! - The opt-out is explicit: [`verify_response_unchecked`] checks
//!   everything but the root's finality and returns `chain_checked` = false;
//!   its text leads with `chain_checked=false (NOT VERIFIED ON CHAIN`.
//!
//! Backward compatibility: before M7 loop 3, `verify_response(…, None)`
//! passed with `chain_checked: false`. The same call now fails closed
//! (`NotFinalOnChain`); a caller that meant to skip the chain read calls
//! [`verify_response_unchecked`] instead. The signature and every other
//! check are unchanged.
//!
//! Not proven by one response (PLAN q14, delivery.md §Verification): that a
//! metric answer comes from the *newest* finalized shard, or that a subject
//! is absent. Anything that settles money pins (round, shard).

use crate::catalog::{snapshot_end, ShardBinding};
use crate::observations::{self, Checked, CheckedAttempt};
use crate::roots::{decode_finalized_root, get_finalized_root_calldata, JsonRpc};
use borsh::BorshDeserialize;
use onyx_coordinator::observe::ObservationOutcome;
use onyx_core::{
    keccak256, Answer, DataRights, FeedTemplate, Hash32, Snapshot, SourceRef, Subject,
};
use onyx_merkle::{MerkleTree, Proof};
use onyx_recipe_sdk::OutputRow;
use serde_json::Value;

/// Response fields [`verify_response`] checks: against the Merkle proof, the
/// binding (recomputed from its inputs and preimages), the client's chain
/// answer (`chainId`, `contract`, `root`, `binding`, `finalizedAt`), or the
/// client's own question ([`Expect`]). Top level and inside `verify`.
pub const BOUND_FIELDS: &[&str] = &[
    // top level
    "round",
    "shard",
    "index",
    "row",
    "rows",
    "leafCount",
    "proof",
    "siblings",
    "root",
    "finalizedAt",
    "recipeId",
    "labelSet",
    "license",
    "attribution",
    "subject",
    "asOf",
    "value",
    "verify",
    // inside `verify`
    "chainId",
    "contract",
    "binding",
    "bindingInputs",
    "shardInputs",
    "sourceRef",
    "rights",
];

/// Response fields a client must not rely on: labels and hints the server
/// writes that nothing binds. `metric` and `market` are names only (what they
/// mean is bound through [`MetricDef`] and the market's pinned shard), `tx`
/// and `finalizedUnix` are hints (the chain is the authority), and `ledger`,
/// `call`, `verifyCall` describe how to check, and `snapshotUnix` is the
/// data time the price clock starts from.
pub const INFORMATIONAL_FIELDS: &[&str] = &[
    "metric",
    "market",
    "tx",
    "verifyCall",
    "ledger",
    "call",
    "finalizedUnix",
    // The data's end time the price clock starts from (AUD-2R-F2): the end
    // header's time for a block shard (a client with its own node checks the
    // header the tip hash commits), a document's bound `published_unix`.
    "snapshotUnix",
    // The server's serving check (ADR-0034 Amendment 1): the operator's claim
    // that the row's base is cross-checked, not bound to the root (THREATS
    // RES-10); read into `Verified::serving`.
    "servingCheck",
    // ADR-0048: the shard's level as the operator's catalog has it (floored
    // by the registry when one is pinned) and the row's one-step ladder. Not
    // bound to the root: a client binds the level with its own
    // `OnyxRegistry.verifyRowLeveled` call and [`check_row_ladder`].
    "proofLevel",
    "ladder",
];

/// What a metric is, as the client knows it from the metric registry: rows of
/// this source and chain over this kind of window, computed by this recipe
/// and runtime with these parameters and this label set, in this schema,
/// under this registry. A row from another metric's shard fails here even if
/// its key matches (M7-L40-F1); so does one over another label set (a
/// `test.*` set included), chain or window, or under another runtime or
/// registry (M7-L41-F2). Every field is required.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetricDef<'a> {
    /// Source id (e.g. `btc.node`).
    pub source: &'a str,
    /// Chain of the snapshot (`ChainId`, e.g. `btc.mainnet`); `None` for
    /// offchain documents.
    pub chain: Option<&'a str>,
    /// Which windows of that chain the metric covers.
    pub window: Window,
    /// Recipe id.
    pub recipe_id: Hash32,
    /// Runtime profile hash (`RuntimeProfile::hash`).
    pub runtime_hash: Hash32,
    /// Row schema id.
    pub schema_id: Hash32,
    /// `keccak256(params)`.
    pub params_hash: Hash32,
    /// The label set (`LabelSetRef::hash`); `Hash32::ZERO` for a metric
    /// that uses no labels.
    pub label_set: Hash32,
    /// Registry root the metric is defined under.
    pub registry_root: Hash32,
}

/// The snapshot window a metric covers (M7-L41-F2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    /// A block range of exactly this many blocks (a single block is 1).
    Blocks(u64),
    /// A block range (or block) starting at exactly this height.
    FromHeight(u64),
    /// An offchain document (no chain window).
    Document,
}

/// What the client asked for. The response must answer exactly this.
// A short-lived argument, never stored in bulk: the large `Metric` variant
// costs nothing worth a `Box` at every call site.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expect<'a> {
    /// `/v1/proofs/{round}/{shard}/{index}`.
    Proof {
        /// Round.
        round: u64,
        /// Shard.
        shard: Hash32,
        /// Row index.
        index: u64,
    },
    /// `/v1/export/{round}/{shard}`.
    Export {
        /// Round.
        round: u64,
        /// Shard.
        shard: Hash32,
    },
    /// `/v1/metrics/{metric}?subject=..[&as_of=..]`.
    Metric {
        /// Metric name (informational; `def` is what is checked).
        metric: &'a str,
        /// The metric's definition.
        def: MetricDef<'a>,
        /// Row key asked for.
        subject: &'a str,
        /// Latest `as_of` asked for, if any: the shard's snapshot must end at
        /// or before it.
        as_of: Option<u64>,
        /// Oldest acceptable snapshot end (PLAN q14 a): the client's own
        /// freshness floor. One response cannot prove it is the newest
        /// shard, so the client says how old an answer it will accept; a
        /// shard whose snapshot ends before this is refused. Pass 0 to accept
        /// any age (only for data where staleness costs nothing).
        min_as_of: u64,
    },
    /// `/v1/settlement/{market}`. The client knows the market's definition
    /// (from the market contract): its round, shard and subject.
    Settlement {
        /// Market id (informational).
        market: &'a str,
        /// The market's round.
        round: u64,
        /// The market's shard.
        shard: Hash32,
        /// The market's row key.
        subject: &'a str,
    },
}

/// What the client's own node returned for `getFinalizedRoot(round, shard)`,
/// and where it asked. It names the (round, shard) it answers, so it cannot be
/// reused for another shard's response (M7-L41-F4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainAnswer {
    /// Round the client asked about.
    pub round: u64,
    /// Shard the client asked about.
    pub shard: Hash32,
    /// Chain the client queried.
    pub chain_id: u64,
    /// Settlement contract the client queried.
    pub contract: [u8; 20],
    /// Returned root.
    pub root: Hash32,
    /// Returned binding.
    pub binding: Hash32,
    /// Returned finalization height.
    pub finalized_at: u64,
}

impl ChainAnswer {
    /// Decode raw `eth_call` return data of `getFinalizedRoot(round, shard)`;
    /// `Ok(None)` when not finalized.
    pub fn from_return(
        round: u64,
        shard: Hash32,
        chain_id: u64,
        contract: [u8; 20],
        ret: &[u8],
    ) -> Result<Option<Self>, VerifyError> {
        let r = decode_finalized_root(ret).map_err(|_| VerifyError::Malformed("return data"))?;
        Ok(r.map(|(root, binding, finalized_at)| ChainAnswer {
            round,
            shard,
            chain_id,
            contract,
            root,
            binding,
            finalized_at,
        }))
    }
}

/// Why a response was rejected.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// A field is missing or not well formed.
    #[error("malformed response: {0}")]
    Malformed(&'static str),
    /// The binding inputs do not derive the shard id.
    #[error("shard id does not match its inputs")]
    ShardMismatch,
    /// The binding does not match its inputs, root and row count.
    #[error("binding does not match its inputs")]
    BindingMismatch,
    /// The row's proof, or the rows, do not hash to the root.
    #[error("rows do not verify against the root")]
    ProofInvalid,
    /// The chain answer differs from the response.
    #[error("chain disagrees: {0}")]
    ChainMismatch(&'static str),
    /// A valid answer, but not to the question asked.
    #[error("not what was asked for: {0}")]
    NotRequested(&'static str),
    /// A field this endpoint's answer never carries (M7-L41-F4).
    #[error("unexpected field: {0}")]
    UnexpectedField(String),
    /// The answer's `ladder` is refused (ADR-0048, [`crate::ladder::check_served`]):
    /// a forged level, a step no check backs, or a fall.
    #[error("ladder refused: {0}")]
    Ladder(String),
    /// A level-2 transcript does not rebuild into the served value under the
    /// client's own template ([`verify_observation`]).
    #[error("observation does not verify: {0}")]
    Observation(String),
    /// No chain answer: the client's node says the root is not final (or
    /// none was given). Can be transient (M7L3-R1-3): a node behind the
    /// operator's, or a root finalized moments ago; retry later.
    /// [`verify_response_unchecked`] is the explicit opt-out.
    #[error("the root is not final on your node (getFinalizedRoot returned nothing): NOT VERIFIED ON CHAIN; if your node lags or the root was finalized moments ago, retry later")]
    NotFinalOnChain,
    /// The client's own node could not be read (nothing was decided).
    #[error("your node could not be read: {0}")]
    ChainUnavailable(String),
}

/// Keys of the `verify` bundle, and of its two input objects.
const VERIFY_KEYS: &[&str] = &[
    "ledger",
    "chainId",
    "contract",
    "call",
    "round",
    "shard",
    "root",
    "leafCount",
    "binding",
    "bindingInputs",
    "shardInputs",
    "sourceRef",
    "rights",
    "finalizedAt",
    "finalizedUnix",
    "tx",
];
const BINDING_INPUT_KEYS: &[&str] = &[
    "recipeId",
    "runtimeHash",
    "labelSetHash",
    "sourceHash",
    "schemaId",
    "rightsHash",
];
const SHARD_INPUT_KEYS: &[&str] = &["paramsHash", "registryRoot"];
const PROOF_KEYS: &[&str] = &["index", "leafCount", "siblings"];
/// Top-level keys of a `/v1/proofs` answer.
const ROW_KEYS: &[&str] = &[
    "round",
    "shard",
    "index",
    "row",
    "leafCount",
    "proof",
    "root",
    "finalizedAt",
    "recipeId",
    "labelSet",
    "tx",
    "license",
    "attribution",
    "snapshotUnix",
    "proofLevel",
    "ladder",
    "verify",
    "servingCheck",
];
/// Extra top-level keys of a `/v1/metrics` answer (on top of [`ROW_KEYS`]).
const METRIC_KEYS: &[&str] = &["metric", "subject", "asOf"];
/// Top-level keys of a `/v1/export` answer.
const EXPORT_KEYS: &[&str] = &[
    "round",
    "shard",
    "root",
    "leafCount",
    "rows",
    "finalizedAt",
    "recipeId",
    "labelSet",
    "tx",
    "license",
    "attribution",
    "proofLevel",
    "verify",
    "servingCheck",
];
/// Top-level keys of a `/v1/settlement` answer.
const SETTLEMENT_KEYS: &[&str] = &[
    "market",
    "subject",
    "value",
    "round",
    "shard",
    "row",
    "index",
    "leafCount",
    "siblings",
    "root",
    "finalizedAt",
    "recipeId",
    "labelSet",
    "tx",
    "verifyCall",
    "proofLevel",
    "verify",
];

/// The top-level keys an answer to `expect` may carry.
pub fn allowed_keys(expect: &Expect<'_>) -> Vec<&'static str> {
    match expect {
        Expect::Proof { .. } => ROW_KEYS.to_vec(),
        Expect::Metric { .. } => ROW_KEYS.iter().chain(METRIC_KEYS).copied().collect(),
        Expect::Export { .. } => EXPORT_KEYS.to_vec(),
        Expect::Settlement { .. } => SETTLEMENT_KEYS.to_vec(),
    }
}

/// A level-2 answer's `onchain` hint: the keys of `base`, and
/// `transcriptComplete` naming exactly
/// [`observations::TRANSCRIPT_COMPLETE_CALL`] (review REVLOG LOW-1). The key
/// is optional, so an answer from a server that predates it still verifies;
/// the client asks `transcriptComplete` for every served round either way
/// (`verify_observation_onchain`, `verify_attempts_onchain`), never because
/// the hint says so.
fn onchain_hint(p: &Value, base: &[&str]) -> Result<(), VerifyError> {
    let mut allowed = base.to_vec();
    // ROADMAP C11: the sealed-reveal calls are named too (optional, exact).
    let named: [(&str, &str, &'static str); 5] = [
        (
            "transcriptComplete",
            observations::TRANSCRIPT_COMPLETE_CALL,
            "onchain.transcriptComplete",
        ),
        (
            "drawRevealKey",
            observations::DRAW_REVEAL_KEY_CALL,
            "onchain.drawRevealKey",
        ),
        (
            "revealLogComplete",
            observations::REVEAL_LOG_COMPLETE_CALL,
            "onchain.revealLogComplete",
        ),
        (
            "attemptMatches",
            observations::ATTEMPT_MATCHES_CALL,
            "onchain.attemptMatches",
        ),
        (
            "drawTimeLocked",
            observations::DRAW_TIME_LOCKED_CALL,
            "onchain.drawTimeLocked",
        ),
    ];
    allowed.extend(named.iter().map(|(k, _, _)| *k));
    only_keys(p, &allowed, "onchain")?;
    for (k, want, what) in named {
        match p.get(k) {
            None => {}
            Some(v) if v.as_str() == Some(want) => {}
            Some(_) => return Err(VerifyError::Malformed(what)),
        }
    }
    Ok(())
}

/// Every key of `obj` is in `allowed` (M7-L41-F4).
fn only_keys(obj: &Value, allowed: &[&str], what: &'static str) -> Result<(), VerifyError> {
    let map = obj.as_object().ok_or(VerifyError::Malformed(what))?;
    match map.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(VerifyError::UnexpectedField(format!("{what}.{k}"))),
        None => Ok(()),
    }
}

/// The answer carries only the keys its endpoint writes.
fn check_keys(body: &Value, expect: &Expect<'_>) -> Result<(), VerifyError> {
    // The wrong kind of answer gets the clearer error, not "unexpected rows".
    match (body.get("rows").is_some(), expect) {
        (true, Expect::Export { .. })
        | (false, Expect::Proof { .. })
        | (false, Expect::Metric { .. })
        | (false, Expect::Settlement { .. }) => {}
        (true, _) => return Err(VerifyError::NotRequested("an export")),
        (false, Expect::Export { .. }) => {
            return Err(VerifyError::NotRequested("one row, not an export"))
        }
    }
    only_keys(body, &allowed_keys(expect), "body")?;
    let v = &body["verify"];
    only_keys(v, VERIFY_KEYS, "verify")?;
    only_keys(
        &v["bindingInputs"],
        BINDING_INPUT_KEYS,
        "verify.bindingInputs",
    )?;
    only_keys(&v["shardInputs"], SHARD_INPUT_KEYS, "verify.shardInputs")?;
    if matches!(expect, Expect::Proof { .. } | Expect::Metric { .. }) {
        only_keys(&body["proof"], PROOF_KEYS, "proof")?;
    }
    Ok(())
}

/// A response that passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    /// Round.
    pub round: u64,
    /// Shard.
    pub shard: Hash32,
    /// Root the rows verify against.
    pub root: Hash32,
    /// Binding recomputed from the inputs.
    pub binding: Hash32,
    /// The server's serving check (ADR-0034 Amendment 1), when the metric
    /// has one: which gate cleared the rows and each served row's
    /// cross-check. **The operator's claim, not a proof**: nothing here is
    /// bound to the root; a client that needs it proven runs the same check
    /// on the public inputs. `None` for metrics without a serving check.
    pub serving: Option<ServingClaim>,
    /// True when the root was checked against the client's own node
    /// ([`ClientNode::verify`], [`verify_response`]). False only after the
    /// explicit opt-out [`verify_response_unchecked`]: nothing shows the
    /// root is final on chain; the caller must still call
    /// `getFinalizedRoot` (see [`Verified::chain_call`]) before trusting it
    /// (IDX-R5-3).
    pub chain_checked: bool,
}

/// What a server said about its serving check of the answer's rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServingClaim {
    /// The gate that cleared the rows.
    pub gate: String,
    /// True: every served row's entity has an independent cross-check
    /// (a server never serves a row without one, so false never verifies).
    pub cross_checked: bool,
    /// Per served row: the cross-check's basis and source.
    pub bases: Vec<(String, String)>,
}

fn serving_row(v: &Value) -> Result<(String, String), VerifyError> {
    only_keys(
        v,
        &["gate", "crossChecked", "basis", "source"],
        "servingCheck",
    )?;
    if v["crossChecked"].as_bool() != Some(true) {
        return Err(VerifyError::Malformed("servingCheck.crossChecked"));
    }
    let s = |k: &'static str| {
        v[k].as_str()
            .map(str::to_string)
            .ok_or(VerifyError::Malformed("servingCheck"))
    };
    Ok((s("basis")?, s("source")?))
}

/// The `servingCheck` field of an answer, checked for shape only.
fn serving_claim(body: &Value) -> Result<Option<ServingClaim>, VerifyError> {
    let Some(v) = body.get("servingCheck") else {
        return Ok(None);
    };
    let gate = v["gate"]
        .as_str()
        .ok_or(VerifyError::Malformed("servingCheck.gate"))?
        .to_string();
    let bases = match v.get("rows") {
        Some(rows) => {
            only_keys(v, &["gate", "rows"], "servingCheck")?;
            let rows = rows
                .as_array()
                .ok_or(VerifyError::Malformed("servingCheck.rows"))?;
            if body["rows"].as_array().map(Vec::len) != Some(rows.len()) {
                return Err(VerifyError::Malformed("servingCheck.rows"));
            }
            rows.iter().map(serving_row).collect::<Result<_, _>>()?
        }
        None => vec![serving_row(v)?],
    };
    Ok(Some(ServingClaim {
        gate,
        cross_checked: true,
        bases,
    }))
}

/// One line that leads with whether the root was checked on the client's own
/// node (IDX-R5-3), so a log or a CLI cannot show a pass without it.
impl std::fmt::Display for Verified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.chain_checked {
            write!(f, "chain_checked=true: ")?;
        } else {
            write!(
                f,
                "chain_checked=false (NOT VERIFIED ON CHAIN: call getFinalizedRoot on your own node): "
            )?;
        }
        write!(
            f,
            "round {} shard {} root {} binding {}",
            self.round, self.shard, self.root, self.binding
        )?;
        if let Some(c) = &self.serving {
            write!(
                f,
                "; serving check {}: cross_checked={} (the server's claim: {})",
                c.gate,
                c.cross_checked,
                c.bases
                    .iter()
                    .map(|(b, _)| b.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )?;
        }
        Ok(())
    }
}

impl Verified {
    /// Refuse an answer that does not say every served row is
    /// independently cross-checked (for a metric with a serving check, such
    /// as `btc.entity-reserve`). This checks the server's claim is there,
    /// not that it is true ([`Verified::serving`]).
    pub fn require_cross_checked(&self) -> Result<&ServingClaim, VerifyError> {
        self.serving
            .as_ref()
            .filter(|c| c.cross_checked && !c.bases.is_empty())
            .ok_or(VerifyError::NotRequested("a cross-checked row"))
    }

    /// Calldata for the `eth_call` that completes the check.
    pub fn chain_call(&self) -> Vec<u8> {
        get_finalized_root_calldata(self.round, &self.shard)
    }
}

/// ADR-0048: a `/v1/proofs` or `/v1/metrics` answer's `proofLevel` and
/// `ladder` against `chain_level`, the level the client's own
/// `OnyxRegistry.verifyRowLeveled` call returned for this row (a shard's
/// level is attested once). Call it after [`verify_response`] passed for
/// `body` (it reads the verified round, shard, index, row and root again).
/// Refused: a stated level other than the chain's (a forged level), and a
/// ladder that is not the row's one step at that level on the root. The
/// step's time is the server's `finalizedUnix` hint, not bound.
pub fn check_row_ladder(
    body: &Value,
    v: &Verified,
    chain_level: onyx_core::ProofLevel,
) -> Result<onyx_core::Ladder, VerifyError> {
    let stated = body["proofLevel"]
        .as_u64()
        .and_then(|n| u8::try_from(n).ok())
        .and_then(onyx_core::ProofLevel::from_number)
        .ok_or(VerifyError::Malformed("proofLevel"))?;
    let index = u64_of(&body["index"], "index")?;
    let row = bytes(&body["row"], "row")?;
    let at = body["ladder"]["steps"][0]["at"]
        .as_u64()
        .ok_or(VerifyError::Malformed("ladder"))?;
    let own = onyx_core::Ladder::new(
        onyx_core::ladder::value_key(
            &onyx_core::ladder::row_key(v.round, &v.shard, index),
            &keccak256(&row),
        ),
        onyx_core::ladder::Step {
            level: chain_level,
            evidence: v.root,
            at,
        },
    )
    .map_err(|e| VerifyError::Ladder(e.to_string()))?;
    crate::ladder::check_served(
        Some(&body["ladder"]),
        &own,
        stated,
        crate::ladder::ROW_RECORD,
        None,
    )
    .map_err(VerifyError::Ladder)
}

fn h32(v: &Value, what: &'static str) -> Result<Hash32, VerifyError> {
    let s = v.as_str().ok_or(VerifyError::Malformed(what))?;
    let mut a = [0u8; 32];
    hex::decode_to_slice(
        s.strip_prefix("0x").ok_or(VerifyError::Malformed(what))?,
        &mut a,
    )
    .map_err(|_| VerifyError::Malformed(what))?;
    Ok(Hash32(a))
}

fn bytes(v: &Value, what: &'static str) -> Result<Vec<u8>, VerifyError> {
    let s = v.as_str().ok_or(VerifyError::Malformed(what))?;
    hex::decode(s.strip_prefix("0x").ok_or(VerifyError::Malformed(what))?)
        .map_err(|_| VerifyError::Malformed(what))
}

fn u64_of(v: &Value, what: &'static str) -> Result<u64, VerifyError> {
    v.as_u64().ok_or(VerifyError::Malformed(what))
}

fn hashes(v: &Value, what: &'static str) -> Result<Vec<Hash32>, VerifyError> {
    v.as_array()
        .ok_or(VerifyError::Malformed(what))?
        .iter()
        .map(|s| h32(s, what))
        .collect()
}

fn decode_row(row: &[u8]) -> Result<OutputRow, VerifyError> {
    OutputRow::try_from_slice(row).map_err(|_| VerifyError::Malformed("row does not decode"))
}

/// Check a `/v1/proofs`, `/v1/metrics`, `/v1/export` or `/v1/settlement`
/// response body against what was asked (`expect`) and against `chain`, the
/// client's own node's `getFinalizedRoot` answer (as
/// [`ChainAnswer::from_return`] decodes it, or as [`ClientNode::answer`]
/// reads it). The chain answer is required (M7 loop 3, IDX-R5-3): `None`
/// means the root is not final on the client's node and is refused
/// ([`VerifyError::NotFinalOnChain`]); the rest is checked first, so a
/// malformed or tampered answer still gets its own error. The explicit
/// opt-out is [`verify_response_unchecked`].
pub fn verify_response(
    body: &Value,
    expect: &Expect<'_>,
    chain: Option<&ChainAnswer>,
) -> Result<Verified, VerifyError> {
    let v = check_response(body, expect, chain)?;
    if chain.is_none() {
        return Err(VerifyError::NotFinalOnChain);
    }
    Ok(v)
}

/// The explicit opt-out of the chain read: every check of
/// [`verify_response`] except the root's finality on the client's own node.
/// The result says `chain_checked` = false and its text leads with
/// `chain_checked=false (NOT VERIFIED ON CHAIN`; a lying operator node is
/// caught only by the chain call this skips (IDX-R5-3).
pub fn verify_response_unchecked(
    body: &Value,
    expect: &Expect<'_>,
) -> Result<Verified, VerifyError> {
    check_response(body, expect, None)
}

/// The client's own chain node (M7 loop 3): reads
/// `getFinalizedRoot(round, shard)` from the settlement contract the client
/// trusts, at the `finalized` block, after checking the node's chain id.
/// Only the client's own node protects against a lying operator node
/// (IDX-R5-3).
pub struct ClientNode<T: JsonRpc> {
    rpc: T,
    chain_id: u64,
    contract: [u8; 20],
}

impl<T: JsonRpc> ClientNode<T> {
    /// The settlement contract `contract` on chain `chain_id`, read through
    /// `rpc` (the client's own node).
    pub fn new(rpc: T, chain_id: u64, contract: [u8; 20]) -> Self {
        ClientNode {
            rpc,
            chain_id,
            contract,
        }
    }

    /// The node's `getFinalizedRoot(round, shard)` at the `finalized` block;
    /// `Ok(None)` when the root is not final there. A node on another chain
    /// is refused.
    pub async fn answer(
        &self,
        round: u64,
        shard: Hash32,
    ) -> Result<Option<ChainAnswer>, VerifyError> {
        let id = self
            .rpc
            .call("eth_chainId", serde_json::json!([]))
            .await
            .map_err(VerifyError::ChainUnavailable)?;
        let id = id
            .as_str()
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .ok_or(VerifyError::ChainUnavailable(
                "eth_chainId: not a quantity".into(),
            ))?;
        if id != self.chain_id {
            return Err(VerifyError::ChainMismatch("chainId"));
        }
        let ret = self
            .rpc
            .call(
                "eth_call",
                serde_json::json!([
                    {
                        "to": format!("0x{}", hex::encode(self.contract)),
                        "data": format!("0x{}", hex::encode(get_finalized_root_calldata(round, &shard))),
                    },
                    "finalized"
                ]),
            )
            .await
            .map_err(VerifyError::ChainUnavailable)?;
        let ret = ret
            .as_str()
            .and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())
            .ok_or(VerifyError::ChainUnavailable("eth_call: not hex".into()))?;
        ChainAnswer::from_return(round, shard, self.chain_id, self.contract, &ret)
    }

    /// The default check: read the response's (round, shard) on this node
    /// and run [`verify_response`] with the answer, so `chain_checked` is
    /// true on success, and a root not final on this node is refused.
    pub async fn verify(&self, body: &Value, expect: &Expect<'_>) -> Result<Verified, VerifyError> {
        let v = &body["verify"];
        let round = u64_of(&v["round"], "round")?;
        let shard = h32(&v["shard"], "shard")?;
        let answer = self.answer(round, shard).await?;
        verify_response(body, expect, answer.as_ref())
    }

    /// [`ClientNode::verify`] for a one-row answer, returning the checked
    /// facts ([`verify_row`]).
    pub async fn verify_row(
        &self,
        body: &Value,
        expect: &Expect<'_>,
    ) -> Result<ProvenRow, VerifyError> {
        let v = &body["verify"];
        let round = u64_of(&v["round"], "round")?;
        let shard = h32(&v["shard"], "shard")?;
        let answer = self.answer(round, shard).await?;
        verify_row(body, expect, answer.as_ref())
    }
}

fn check_response(
    body: &Value,
    expect: &Expect<'_>,
    chain: Option<&ChainAnswer>,
) -> Result<Verified, VerifyError> {
    check_parts(body, expect, chain).map(|p| p.verified)
}

/// [`check_response`] with the facts it checked: the binding inputs, the
/// source reference and, for a one-row answer, the proven row.
fn check_parts(
    body: &Value,
    expect: &Expect<'_>,
    chain: Option<&ChainAnswer>,
) -> Result<Parts, VerifyError> {
    check_keys(body, expect)?;
    let v = &body["verify"];
    let i = &v["bindingInputs"];
    let s = &v["shardInputs"];
    let inputs = ShardBinding {
        recipe_id: h32(&i["recipeId"], "recipeId")?,
        runtime_hash: h32(&i["runtimeHash"], "runtimeHash")?,
        label_set_hash: h32(&i["labelSetHash"], "labelSetHash")?,
        source_hash: h32(&i["sourceHash"], "sourceHash")?,
        schema_id: h32(&i["schemaId"], "schemaId")?,
        rights_hash: h32(&i["rightsHash"], "rightsHash")?,
        params_hash: h32(&s["paramsHash"], "paramsHash")?,
        registry_root: h32(&s["registryRoot"], "registryRoot")?,
    };
    let round = u64_of(&v["round"], "round")?;
    let shard = h32(&v["shard"], "shard")?;
    let root = h32(&v["root"], "root")?;
    let leaf_count = u64_of(&v["leafCount"], "leafCount")?;
    // The outer fields must name the same shard and root as the bundle.
    if body["shard"] != v["shard"] || body["root"] != v["root"] || body["round"] != v["round"] {
        return Err(VerifyError::Malformed("body and bundle differ"));
    }
    match expect {
        Expect::Proof {
            round: r,
            shard: sh,
            ..
        }
        | Expect::Export {
            round: r,
            shard: sh,
        }
        | Expect::Settlement {
            round: r,
            shard: sh,
            ..
        } => {
            if round != *r {
                return Err(VerifyError::NotRequested("round"));
            }
            if shard != *sh {
                return Err(VerifyError::NotRequested("shard"));
            }
        }
        Expect::Metric { .. } => {}
    }
    if inputs.shard_id() != shard {
        return Err(VerifyError::ShardMismatch);
    }
    let binding = inputs.binding(root, leaf_count);
    if h32(&v["binding"], "binding")? != binding {
        return Err(VerifyError::BindingMismatch);
    }
    // Preimages inside the binding (M7-L40-F1): the source reference pins the
    // source and snapshot (so `asOf`), the rights record pins the license.
    let source_bytes = bytes(&v["sourceRef"], "sourceRef")?;
    if keccak256(&source_bytes) != inputs.source_hash {
        return Err(VerifyError::BindingMismatch);
    }
    let source = SourceRef::try_from_slice(&source_bytes)
        .map_err(|_| VerifyError::Malformed("sourceRef"))?;
    let rights_bytes = bytes(&v["rights"], "rights")?;
    if keccak256(&rights_bytes) != inputs.rights_hash {
        return Err(VerifyError::BindingMismatch);
    }
    let rights =
        DataRights::try_from_slice(&rights_bytes).map_err(|_| VerifyError::Malformed("rights"))?;
    check_echoes(body, v, &inputs, &rights)?;
    if let Expect::Metric {
        def,
        as_of,
        min_as_of,
        ..
    } = expect
    {
        check_metric(body, def, *as_of, *min_as_of, &inputs, &source)?;
    }
    // The data itself: one row with its proof, or every row.
    let mut proven: Option<(u64, Vec<u8>)> = None;
    if let Some(rows) = body["rows"].as_array() {
        if !matches!(expect, Expect::Export { .. }) {
            return Err(VerifyError::NotRequested("an export"));
        }
        let rows = rows
            .iter()
            .map(|r| bytes(r, "rows"))
            .collect::<Result<Vec<_>, _>>()?;
        if rows.is_empty() {
            return Err(VerifyError::Malformed("rows"));
        }
        let tree = MerkleTree::from_rows(&rows);
        if tree.root() != root || tree.leaf_count() != leaf_count {
            return Err(VerifyError::ProofInvalid);
        }
    } else {
        let row = bytes(&body["row"], "row")?;
        // Row endpoints nest the proof; the settlement body is flat (it is the
        // `verifyRow` argument list).
        let p = if body["proof"].is_object() {
            &body["proof"]
        } else {
            body
        };
        let proof = Proof {
            index: u64_of(&p["index"], "index")?,
            leaf_count: u64_of(&p["leafCount"], "leafCount")?,
            siblings: hashes(&p["siblings"], "siblings")?,
        };
        if proof.leaf_count != leaf_count || !onyx_merkle::verify(&root, &row, &proof) {
            return Err(VerifyError::ProofInvalid);
        }
        // The outer index must be the proven one.
        if body["index"].as_u64() != Some(proof.index) {
            return Err(VerifyError::Malformed("index differs from the proof"));
        }
        check_row(body, expect, &row, proof.index)?;
        proven = Some((proof.index, row));
    }
    if let Some(c) = chain {
        // The answer must be for this (round, shard) (M7-L41-F4).
        if c.round != round {
            return Err(VerifyError::ChainMismatch("round"));
        }
        if c.shard != shard {
            return Err(VerifyError::ChainMismatch("shard"));
        }
        // The client's own node is the authority. A bundle naming another
        // chain or contract is refused even if the root happens to match.
        if v["chainId"].as_u64().is_some_and(|id| id != c.chain_id) {
            return Err(VerifyError::ChainMismatch("chainId"));
        }
        if v["contract"].is_string() && bytes(&v["contract"], "contract")? != c.contract {
            return Err(VerifyError::ChainMismatch("contract"));
        }
        if c.root != root {
            return Err(VerifyError::ChainMismatch("root"));
        }
        if c.binding != binding {
            return Err(VerifyError::ChainMismatch("binding"));
        }
        if v["finalizedAt"].as_u64() != Some(c.finalized_at) {
            return Err(VerifyError::ChainMismatch("finalizedAt"));
        }
    }
    Ok(Parts {
        verified: Verified {
            round,
            shard,
            root,
            binding,
            serving: serving_claim(body)?,
            chain_checked: chain.is_some(),
        },
        inputs,
        source,
        proven,
    })
}

/// What [`check_parts`] checked.
struct Parts {
    verified: Verified,
    inputs: ShardBinding,
    source: SourceRef,
    proven: Option<(u64, Vec<u8>)>,
}

/// A one-row answer that passed every check of [`verify_response`],
/// including the chain read, with the facts a sector's own check needs
/// (ROADMAP D3): the binding inputs the client recomputed the shard id and
/// binding from, the source reference inside the binding (so the snapshot's
/// range and tip hash), and the row proven under the finalized root. A
/// client re-running a serving check builds on these, never on a field the
/// server writes outside the binding.
///
/// Only [`verify_row`] makes one (the fields are private, review D3-R1-4), so
/// holding a `ProvenRow` means the checks ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenRow {
    verified: Verified,
    inputs: ShardBinding,
    source: SourceRef,
    index: u64,
    row: Vec<u8>,
}

impl ProvenRow {
    /// The verified answer (`chain_checked` is always true).
    pub fn verified(&self) -> &Verified {
        &self.verified
    }

    /// The binding inputs (they derive the shard id; with the root and row
    /// count, the binding the chain finalized).
    pub fn inputs(&self) -> &ShardBinding {
        &self.inputs
    }

    /// The source reference whose hash is in the binding.
    pub fn source(&self) -> &SourceRef {
        &self.source
    }

    /// Leaf index of the row.
    pub fn index(&self) -> u64 {
        self.index
    }

    /// The row (Borsh `OutputRow`), proven under the verified root.
    pub fn row(&self) -> &[u8] {
        &self.row
    }
}

/// [`verify_response`] for a one-row answer (`/v1/proofs`, `/v1/metrics`,
/// `/v1/settlement`), returning the checked facts as a [`ProvenRow`]. The
/// chain answer is required, as in [`verify_response`]: `None` is
/// [`VerifyError::NotFinalOnChain`]. An export is refused: it has no single
/// proven row.
pub fn verify_row(
    body: &Value,
    expect: &Expect<'_>,
    chain: Option<&ChainAnswer>,
) -> Result<ProvenRow, VerifyError> {
    if matches!(expect, Expect::Export { .. }) {
        return Err(VerifyError::NotRequested("one row, not an export"));
    }
    let p = check_parts(body, expect, chain)?;
    if chain.is_none() {
        return Err(VerifyError::NotFinalOnChain);
    }
    let (index, row) = p.proven.ok_or(VerifyError::NotRequested("one row"))?;
    Ok(ProvenRow {
        verified: p.verified,
        inputs: p.inputs,
        source: p.source,
        index,
        row,
    })
}

/// Fields the server repeats outside the bundle must equal what the bundle
/// binds: `leafCount`, `finalizedAt`, `recipeId`, `labelSet`, `license`,
/// `attribution` (M7-L40-F1).
fn check_echoes(
    body: &Value,
    v: &Value,
    inputs: &ShardBinding,
    rights: &DataRights,
) -> Result<(), VerifyError> {
    let nonzero = |h: Hash32| {
        if h == Hash32::ZERO {
            Value::Null
        } else {
            Value::String(h.to_string())
        }
    };
    if !body["leafCount"].is_null() && body["leafCount"] != v["leafCount"] {
        return Err(VerifyError::Malformed("leafCount differs from the bundle"));
    }
    if !body["finalizedAt"].is_null() && body["finalizedAt"] != v["finalizedAt"] {
        return Err(VerifyError::Malformed(
            "finalizedAt differs from the bundle",
        ));
    }
    if body.get("recipeId").is_some() && body["recipeId"] != nonzero(inputs.recipe_id) {
        return Err(VerifyError::Malformed("recipeId differs from the binding"));
    }
    if body.get("labelSet").is_some() && body["labelSet"] != nonzero(inputs.label_set_hash) {
        return Err(VerifyError::Malformed("labelSet differs from the binding"));
    }
    if body.get("license").is_some() && body["license"].as_str() != Some(&rights.license) {
        return Err(VerifyError::Malformed(
            "license differs from the rights record",
        ));
    }
    if body.get("attribution").is_some()
        && body["attribution"].as_str() != rights.attribution.as_deref()
    {
        return Err(VerifyError::Malformed(
            "attribution differs from the rights record",
        ));
    }
    Ok(())
}

/// The shard computes the metric the client means, over data no newer than
/// `as_of` (M7-L40-F1) and no older than `min_as_of` (PLAN q14 a,
/// M7-L41-F1). `asOf` must be the end of the pinned snapshot.
fn check_metric(
    body: &Value,
    def: &MetricDef<'_>,
    as_of: Option<u64>,
    min_as_of: u64,
    inputs: &ShardBinding,
    source: &SourceRef,
) -> Result<(), VerifyError> {
    if source.source.0 != def.source {
        return Err(VerifyError::NotRequested("source"));
    }
    if inputs.recipe_id != def.recipe_id {
        return Err(VerifyError::NotRequested("recipe"));
    }
    if inputs.schema_id != def.schema_id {
        return Err(VerifyError::NotRequested("schema"));
    }
    if inputs.params_hash != def.params_hash {
        return Err(VerifyError::NotRequested("params"));
    }
    if inputs.label_set_hash != def.label_set {
        return Err(VerifyError::NotRequested("label set"));
    }
    if inputs.runtime_hash != def.runtime_hash {
        return Err(VerifyError::NotRequested("runtime"));
    }
    if inputs.registry_root != def.registry_root {
        return Err(VerifyError::NotRequested("registry"));
    }
    check_window(&source.snapshot, def)?;
    let end = snapshot_end(&source.snapshot);
    if body["asOf"].as_u64() != Some(end) {
        return Err(VerifyError::Malformed("asOf differs from the snapshot"));
    }
    if as_of.is_some_and(|max| end > max) {
        return Err(VerifyError::NotRequested("asOf"));
    }
    if end < min_as_of {
        return Err(VerifyError::NotRequested("asOf older than min_as_of"));
    }
    Ok(())
}

/// The snapshot is on the metric's chain and covers its kind of window
/// (M7-L41-F2): a shorter or shifted range with the same recipe is another
/// metric.
fn check_window(snapshot: &Snapshot, def: &MetricDef<'_>) -> Result<(), VerifyError> {
    let (chain, from, to) = match snapshot {
        Snapshot::Block { chain, height, .. } => (Some(chain.0.as_str()), *height, *height),
        Snapshot::BlockRange {
            chain,
            from_height,
            to_height,
            ..
        } => (Some(chain.0.as_str()), *from_height, *to_height),
        Snapshot::Document { .. } => (None, 0, 0),
    };
    if chain != def.chain {
        return Err(VerifyError::NotRequested("chain"));
    }
    let ok = match (def.window, chain) {
        (Window::Document, None) => true,
        (Window::Blocks(n), Some(_)) => {
            to.checked_sub(from).and_then(|d| d.checked_add(1)) == Some(n)
        }
        (Window::FromHeight(h), Some(_)) => from == h,
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(VerifyError::NotRequested("window"))
    }
}

/// The proven row answers the question: the right index, the right key, and
/// the outer fields say what the row says.
fn check_row(body: &Value, expect: &Expect<'_>, row: &[u8], index: u64) -> Result<(), VerifyError> {
    match expect {
        Expect::Export { .. } => Err(VerifyError::NotRequested("one row, not an export")),
        Expect::Proof { index: want, .. } => {
            if index != *want {
                return Err(VerifyError::NotRequested("index"));
            }
            Ok(())
        }
        Expect::Metric {
            metric, subject, ..
        } => {
            if body["metric"].as_str() != Some(*metric) {
                return Err(VerifyError::NotRequested("metric"));
            }
            if body["subject"].as_str() != Some(*subject) {
                return Err(VerifyError::NotRequested("subject"));
            }
            if decode_row(row)?.key != subject.as_bytes() {
                return Err(VerifyError::NotRequested("row key"));
            }
            Ok(())
        }
        Expect::Settlement {
            market, subject, ..
        } => {
            if body["market"].as_str() != Some(*market) {
                return Err(VerifyError::NotRequested("market"));
            }
            if body["subject"].as_str() != Some(*subject) {
                return Err(VerifyError::NotRequested("subject"));
            }
            let o = decode_row(row)?;
            if o.key != subject.as_bytes() {
                return Err(VerifyError::NotRequested("row key"));
            }
            if bytes(&body["value"], "value")? != o.value {
                return Err(VerifyError::Malformed("value differs from the row"));
            }
            Ok(())
        }
    }
}

// ------------------------------------------------------------ level 2

/// Top-level keys of a `/v1/observations` answer.
const OBSERVATION_KEYS: &[&str] = &[
    "metric",
    "subject",
    "subjectText",
    "windowStart",
    "windowEnd",
    "config",
    "pool",
    "draw",
    "observers",
    "commits",
    "reveals",
    "sealed",
    "answer",
    "value",
    "proofLevel",
    "ladder",
    "templateId",
    "specId",
    "roundId",
    "resultHash",
    "factKey",
    "registryFactKey",
    "settledUnix",
    "rights",
    "license",
    "attribution",
    "onchain",
    "attempts",
    "limits",
    // CL7 (ADR-0057 item 6): the paired recheck's note, checked by shape
    // here and against the recheck's own verified answer by [`verify_recheck`].
    "recheck",
];

/// A served `limits` (ROADMAP B4) must be exactly the stated level-2 limits
/// for how the answer's rounds were served ([`observations::limits_json`]):
/// a server may not drop one or say REVLOG-5 does not apply to a sealed
/// round. Absent (a server from before B4) is accepted; a client from before
/// B4 refuses the extra key (fail closed, as `transcriptComplete`).
fn check_limits(body: &Value, sealed: bool) -> Result<(), VerifyError> {
    match body.get("limits") {
        None => Ok(()),
        Some(l) if *l == observations::limits_json(sealed) => Ok(()),
        Some(_) => Err(VerifyError::Malformed(
            "limits: not the stated level-2 limits for this answer",
        )),
    }
}

/// Top-level keys of a `/v1/observations/{metric}/attempts` answer.
const ATTEMPTS_KEYS: &[&str] = &[
    "metric",
    "subjectText",
    "templateId",
    "factKey",
    "registryFactKey",
    "attempts",
    "onchain",
    "limits",
];

/// What a client asked `/v1/observations/{metric}` for, from its own
/// knowledge: the metric's registered feed template (its own copy, reviewed
/// with the source's rights; never one the server sends), the subject, a
/// fixed feed's window end if it asked for one, the oldest window it accepts,
/// and its own clock.
#[derive(Clone, Copy, Debug)]
pub struct ObservationExpect<'a> {
    /// The client's copy of the registered template.
    pub template: &'a FeedTemplate,
    /// The subject asked for.
    pub subject: &'a Subject,
    /// The window end asked for (fixed feeds), if any.
    pub window_end: Option<u64>,
    /// Freshness floor: the served round's window must end at or after this
    /// (unix seconds). Required, like `min_as_of` for shard metrics
    /// (M7-L41-F1): without it a server answering "newest" may serve any old
    /// round (L2API-R1-F3).
    pub min_window_end: u64,
    /// The client's clock (unix seconds): a round that settles after it (plus
    /// `MAX_CLOCK_SKEW_SECS`) is refused (L2API-R1-F2).
    pub now: u64,
}

/// A level-2 answer that passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedObservation {
    /// The settled value.
    pub answer: Answer,
    /// The value as text (decimal at the feed's scale, or hex bytes).
    pub value: String,
    /// What the round proves (spec, round id, result, fact keys).
    pub checked: Checked,
    /// The value's ladder as served and checked (ADR-0048); keep it and
    /// pass it to [`onyx_core::Ladder::check_extends`] for the next answer
    /// about the same value, so a later fall is refused.
    pub ladder: onyx_core::Ladder,
    /// With `?outcome=all`: the fact's published rounds that settled no
    /// value, checked off chain; [`verify_attempts_onchain`] checks them
    /// against the fact's draws on chain. `None` when not asked for.
    pub attempts: Option<VerifiedAttempts>,
}

/// A fact's published rounds without a value (SEC-R2-F1), each rebuilt from
/// its transcript with the client's own template: draw, commits, reveals and
/// the outcome they aggregate to (`SourceUnavailable` or `NoResult`), all of
/// one fact, by strictly ascending draw attempt, each below the value round's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedAttempts {
    /// The checked rounds, by ascending attempt.
    pub attempts: Vec<CheckedAttempt>,
    /// The on-chain fact key (`OnyxRegistry.factKeyOf`).
    pub registry_fact: Hash32,
    /// `onchain::metric_key`.
    pub metric_key: Hash32,
    /// `onchain::subject_key`.
    pub subject_key: Hash32,
    /// A dated feed: one fact per subject, so the subject's strikes are this
    /// fact's draws without a value, exactly.
    pub dated: bool,
    /// The value round's draw attempt, when the fact settled a value.
    pub value_attempt: Option<u32>,
}

/// What [`verify_attempts_onchain`] established about a fact's draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrawTally {
    /// Draws the chain announced for the fact (`drawCount`).
    pub draws: u32,
    /// Of those, the draws that did not end with the recorded value.
    pub without_value: u32,
    /// Of those, the ones with a published round (the rest never had their
    /// beacon fixed, so no committee existed).
    pub published: u32,
}

impl VerifiedAttempts {
    /// Calldata of `OnyxRegistry.drawCount(fact)`.
    pub fn draw_count_calldata(&self) -> Vec<u8> {
        let mut d = keccak256(b"drawCount(bytes32)").0[..4].to_vec();
        d.extend_from_slice(&self.registry_fact.0);
        d
    }

    /// Calldata of `OnyxRegistry.drawOf(fact, attempt)`.
    pub fn draw_of_calldata(&self, attempt: u32) -> Vec<u8> {
        let mut d = keccak256(b"drawOf(bytes32,uint32)").0[..4].to_vec();
        d.extend_from_slice(&self.registry_fact.0);
        let mut w = [0u8; 32];
        w[28..].copy_from_slice(&attempt.to_be_bytes());
        d.extend_from_slice(&w);
        d
    }

    /// Calldata of `OnyxRegistry.drawBudget(metric, subjectKey)`.
    pub fn draw_budget_calldata(&self) -> Vec<u8> {
        let mut d = keccak256(b"drawBudget(bytes32,bytes32)").0[..4].to_vec();
        d.extend_from_slice(&self.metric_key.0);
        d.extend_from_slice(&self.subject_key.0);
        d
    }

    /// Calldata of `OnyxRegistry.transcriptComplete` for every published
    /// round, in order: [`verify_attempts_onchain`] takes one return each.
    pub fn transcript_complete_calldatas(&self) -> Vec<Vec<u8>> {
        self.attempts.iter().map(|c| c.log.calldata()).collect()
    }

    /// The sealed-reveal calls of every published round, in order
    /// (ROADMAP C11): [`verify_attempts_onchain`] takes one [`SealedRets`]
    /// each.
    pub fn sealed_calls(&self) -> Vec<SealedCalls> {
        self.attempts
            .iter()
            .map(|c| sealed_calls(&c.reveal_log))
            .collect()
    }

    /// How many `drawOf` returns [`verify_attempts_onchain`] needs, given
    /// the `drawCount` the chain returned: one per draw without a value.
    pub fn draws_without_value(&self, draw_count: u32) -> u32 {
        draw_count.saturating_sub(u32::from(self.value_attempt.is_some()))
    }
}

/// One `drawOf` return: (commit, beacon height, announced at, beacon).
fn draw_of_from_return(ret: &[u8]) -> Result<(Hash32, u64, u64, Hash32), VerifyError> {
    let bad = || VerifyError::Malformed("drawOf return");
    if ret.len() != 128 {
        return Err(bad());
    }
    let u64w = |w: &[u8]| -> Result<u64, VerifyError> {
        if w[..24].iter().any(|b| *b != 0) {
            return Err(bad());
        }
        Ok(u64::from_be_bytes(w[24..32].try_into().map_err(|_| bad())?))
    };
    let h =
        |w: &[u8]| -> Result<Hash32, VerifyError> { Ok(Hash32(w.try_into().map_err(|_| bad())?)) };
    Ok((
        h(&ret[..32])?,
        u64w(&ret[32..64])?,
        u64w(&ret[64..96])?,
        h(&ret[96..])?,
    ))
}

/// What `drawCount` returned (one ABI word, a `uint32`).
pub fn draw_count_from_return(ret: &[u8]) -> Result<u32, VerifyError> {
    let w: [u8; 32] = ret
        .try_into()
        .map_err(|_| VerifyError::Malformed("drawCount return"))?;
    if w[..28].iter().any(|b| *b != 0) {
        return Err(VerifyError::Malformed("drawCount return"));
    }
    Ok(u32::from_be_bytes([w[28], w[29], w[30], w[31]]))
}

/// The on-chain half of checking a fact's rounds without a value
/// (SEC-R2-F1), from `drawCount(fact)`, then `drawOf(fact, i)` for every
/// draw without a value (`i` in `0..draws_without_value(drawCount)`), and
/// `drawBudget(metric, subject)`, read from the registry the client trusts,
/// at one block. Requires all of:
/// - with a value round: `drawCount` is its attempt + 1 (it is the latest
///   draw, as `drawMatches` also checks);
/// - every published round's draw is the chain's draw at its attempt (same
///   commitment to pool, spec and config, beacon height and beacon,
///   announced before the beacon height);
/// - every other draw without a value has no beacon on chain. Such a draw had
///   no committee only because honest observers commit after `drawOf` shows
///   the beacon fixed (`observe::check_commit_allowed`, ATT-R1-F4);
/// - the subject's strikes cover these draws (exactly, for a dated feed:
///   one fact per subject) and stay within the allowance.
///
/// So the served value round is the only value round, and every draw the fact
/// used before it is accounted for. Each published round is one its drawn
/// observers played: `verify_observation` / `verify_attempts` (which built
/// `a`) require every commit and reveal to carry its observer's signature
/// (ATT-R1-F3), so a coordinator cannot publish a made-up round for a fixed
/// draw in place of a dissenting one. A server can still leave out the
/// reveal of an observer that did not post its receipt (the observer then
/// shows as committed, not revealed), and a sealed round's outcome is its
/// word until the fact is closed (ATT-R1-F2).
pub fn verify_attempts_onchain(
    a: &VerifiedAttempts,
    count_ret: &[u8],
    draw_of_rets: &[Vec<u8>],
    budget_ret: &[u8],
    complete_rets: &[Vec<u8>],
    sealed_rets: &[SealedRets],
) -> Result<DrawTally, VerifyError> {
    if complete_rets.len() != a.attempts.len() || sealed_rets.len() != a.attempts.len() {
        return Err(VerifyError::Malformed(
            "transcriptComplete and sealed returns: one per published round",
        ));
    }
    for ret in complete_rets {
        if !transcript_complete_from_return(ret)? {
            return Err(VerifyError::ChainMismatch(
                "transcriptComplete: a published round leaves out a logged commit or receipted reveal",
            ));
        }
    }
    // ROADMAP C11: each round's sealed posts, dismissals and record.
    for (c, r) in a.attempts.iter().zip(sealed_rets) {
        verify_sealed_onchain(&c.reveal_log, r)?;
    }
    let draws = draw_count_from_return(count_ret)?;
    if let Some(v) = a.value_attempt {
        if draws != v.saturating_add(1) {
            return Err(VerifyError::ChainMismatch("drawCount"));
        }
    }
    let without_value = a.draws_without_value(draws);
    if draw_of_rets.len() != without_value as usize {
        return Err(VerifyError::Malformed(
            "drawOf returns: one per draw without a value",
        ));
    }
    if a.attempts.iter().any(|c| c.draw.attempt >= without_value) {
        return Err(VerifyError::ChainMismatch(
            "a published round the chain never announced",
        ));
    }
    for (i, ret) in (0u32..).zip(draw_of_rets) {
        let (commit, beacon_height, announced_at, beacon) = draw_of_from_return(ret)?;
        match a.attempts.iter().find(|c| c.draw.attempt == i) {
            Some(c) => {
                if commit != c.onchain.draw_commit()
                    || beacon_height != c.draw.beacon_height
                    || beacon != c.draw.beacon
                    || announced_at >= beacon_height
                {
                    return Err(VerifyError::ChainMismatch("drawOf"));
                }
            }
            None => {
                if commit == Hash32::ZERO || beacon != Hash32::ZERO {
                    return Err(VerifyError::ChainMismatch(
                        "a draw with a fixed beacon and no published round",
                    ));
                }
            }
        }
    }
    let (strikes, allowance) = draw_budget_from_return(budget_ret)?;
    let covered = if a.dated {
        strikes == without_value
    } else {
        strikes >= without_value
    };
    if strikes > allowance || !covered {
        return Err(VerifyError::ChainMismatch("drawBudget"));
    }
    Ok(DrawTally {
        draws,
        without_value,
        published: a.attempts.len() as u32,
    })
}

/// Check the `attempts` array of an answer under the client's template: every
/// round rebuilds (draw, commits, reveals) to an outcome without a value and
/// is for `subject`, all of one fact (`fact`, if known), strictly ascending
/// by draw attempt and below `before` (the value round's attempt), and each
/// served outcome, round id, result hash and window end is the rebuilt one.
/// With `sealed`, a round may come without its reveals (ATT-R1-F2,
/// [`observations::check_sealed_attempt`]): everything but the aggregation
/// is checked, and its outcome is the server's word.
fn check_attempts(
    arr: &Value,
    template: &FeedTemplate,
    subject: &Subject,
    fact: Option<Hash32>,
    now: u64,
    before: Option<u32>,
    sealed: bool,
) -> Result<Vec<CheckedAttempt>, VerifyError> {
    let list = arr.as_array().ok_or(VerifyError::Malformed("attempts"))?;
    if list.len() > observations::MAX_DRAW_ATTEMPTS as usize {
        return Err(VerifyError::Malformed(
            "attempts: more than a fact may draw",
        ));
    }
    let obs = |e: observations::ObservationError| VerifyError::Observation(e.to_string());
    let mut out: Vec<CheckedAttempt> = Vec::with_capacity(list.len());
    for a in list {
        let c = if sealed && a.get("revealed").is_some() {
            only_keys(a, observations::SEALED_ATTEMPT_KEYS, "attempts[]")?;
            let (at, revealed) = observations::sealed_attempt_of(a).map_err(obs)?;
            if &at.subject != subject {
                return Err(VerifyError::NotRequested("an attempt for another subject"));
            }
            let outcome = match a["outcome"].as_str() {
                Some("sourceUnavailable") => ObservationOutcome::SourceUnavailable,
                Some("noResult") => ObservationOutcome::NoResult,
                _ => return Err(VerifyError::Malformed("attempts[].outcome")),
            };
            observations::check_sealed_attempt(template, &at, &revealed, outcome, now)
                .map_err(obs)?
        } else {
            only_keys(a, observations::ATTEMPT_RESPONSE_KEYS, "attempts[]")?;
            let at = observations::attempt_of(a).map_err(obs)?;
            if &at.subject != subject {
                return Err(VerifyError::NotRequested("an attempt for another subject"));
            }
            observations::check_attempt(template, &at, now).map_err(obs)?
        };
        if fact
            .or(out.first().map(|f| f.fact))
            .is_some_and(|f| f != c.fact)
        {
            return Err(VerifyError::Observation(
                "an attempt for another fact".into(),
            ));
        }
        if out.last().is_some_and(|p| p.draw.attempt >= c.draw.attempt)
            || before.is_some_and(|v| c.draw.attempt >= v)
        {
            return Err(VerifyError::Observation(
                "attempts are not strictly ascending below the value round's draw".into(),
            ));
        }
        if a["outcome"].as_str() != Some(observations::outcome_text(&c.outcome))
            || h32(&a["roundId"], "attempts[].roundId")? != c.round_id
            || h32(&a["resultHash"], "attempts[].resultHash")? != c.result
            || u64_of(&a["windowEnd"], "attempts[].windowEnd")? != c.window_end
        {
            return Err(VerifyError::Observation(
                "an attempt's outcome, round id, result or window is not what it rebuilds to"
                    .into(),
            ));
        }
        out.push(c);
    }
    Ok(out)
}

/// Check a `/v1/observations/{metric}/attempts` answer (a fact that settled
/// no value) against what was asked: `expect`'s template, subject, window
/// end (fixed feeds) and clock; `min_window_end` is not used. Every round is
/// rebuilt as [`check_attempts`] does; the fact keys and names must be the
/// rebuilt ones. Then [`verify_attempts_onchain`] checks the draws on chain.
pub fn verify_attempts(
    body: &Value,
    expect: &ObservationExpect<'_>,
) -> Result<VerifiedAttempts, VerifyError> {
    let map = body.as_object().ok_or(VerifyError::Malformed("body"))?;
    if let Some(k) = map.keys().find(|k| !ATTEMPTS_KEYS.contains(&k.as_str())) {
        return Err(VerifyError::UnexpectedField(format!("body.{k}")));
    }
    let template = expect.template;
    let attempts = check_attempts(
        &body["attempts"],
        template,
        expect.subject,
        None,
        expect.now,
        None,
        // A fixed fact closes on its first round, so its rounds always come
        // with their reveals; a dated or period one may come sealed.
        template.is_dated(),
    )?;
    let first = attempts
        .first()
        .ok_or(VerifyError::Malformed("attempts: none"))?;
    check_limits(body, attempts.iter().any(|c| c.sealed))?;
    if !template.is_dated() && expect.window_end.is_some_and(|w| w != first.window_end) {
        return Err(VerifyError::NotRequested("another window"));
    }
    if body["metric"].as_str() != Some(template.metric.0.as_str())
        || body["subjectText"].as_str() != Some(observations::subject_text(expect.subject))
        || h32(&body["templateId"], "templateId")? != template.id()
        || h32(&body["factKey"], "factKey")? != first.fact
        || h32(&body["registryFactKey"], "registryFactKey")? != first.registry_fact
    {
        return Err(VerifyError::Observation(
            "metric, subject text, template or fact keys are not the rebuilt ones".into(),
        ));
    }
    match &body["onchain"] {
        Value::Null => {}
        p => onchain_hint(
            p,
            &["chainId", "registry", "drawCount", "drawOf", "drawBudget"],
        )?,
    }
    Ok(VerifiedAttempts {
        registry_fact: first.registry_fact,
        metric_key: first.onchain.metric_key,
        subject_key: first.onchain.subject_key,
        dated: template.is_dated(),
        value_attempt: None,
        attempts,
    })
}

impl VerifiedObservation {
    /// Calldata of `OnyxRegistry.observationLevel(metric, subjectKey,
    /// windowEnd, spec, result)` built from the client's own recomputation:
    /// the chain answers 2 only if it recorded exactly this result for this
    /// fact under this spec (ADR-0030).
    pub fn observation_level_calldata(&self) -> Vec<u8> {
        let o = &self.checked.onchain;
        let mut d =
            keccak256(b"observationLevel(bytes32,bytes32,uint64,bytes32,bytes32)").0[..4].to_vec();
        d.extend_from_slice(&o.metric_key.0);
        d.extend_from_slice(&o.subject_key.0);
        let mut w = [0u8; 32];
        w[24..].copy_from_slice(&o.window_end.to_be_bytes());
        d.extend_from_slice(&w);
        d.extend_from_slice(&o.spec_id.0);
        d.extend_from_slice(&o.result.0);
        d
    }
}

impl VerifiedObservation {
    /// Calldata of `OnyxRegistry.drawMatches(metric, subjectKey, windowEnd,
    /// attempt, commit, beaconHeight, beacon)` (L2RND-R1): the chain answers
    /// true only if the coordinator announced this draw on chain before the
    /// beacon height, committed to this pool, spec and config (the client's
    /// own `draw_commit`, L2RND-R3), as the fact's latest attempt, and the
    /// beacon is the block hash `fixBeacon` read at that height. A client
    /// requires both this and level 2 from `observationLevel`.
    pub fn draw_matches_calldata(&self) -> Vec<u8> {
        let o = &self.checked.onchain;
        onyx_coordinator::observe::draw_matches_calldata(
            &onyx_coordinator::observe::OnchainFact {
                metric_key: o.metric_key,
                subject_key: o.subject_key,
                window_end: o.window_end,
                // `drawMatches` takes neither (the registry knows both).
                dated: false,
                template: Hash32::ZERO,
            },
            &self.checked.draw,
            &o.draw_commit(),
        )
    }
}

impl VerifiedObservation {
    /// Calldata of `OnyxRegistry.drawBudget(metric, subjectKey)`
    /// (L2RND-R2-F1): the subject's announced draws that did not end with a
    /// recorded value, and how many the owner's reopens allow.
    pub fn draw_budget_calldata(&self) -> Vec<u8> {
        let o = &self.checked.onchain;
        let mut d = keccak256(b"drawBudget(bytes32,bytes32)").0[..4].to_vec();
        d.extend_from_slice(&o.metric_key.0);
        d.extend_from_slice(&o.subject_key.0);
        d
    }

    /// The sealed-reveal calls [`verify_observation_onchain`] needs as its
    /// last argument (ROADMAP C11).
    pub fn sealed_calls(&self) -> SealedCalls {
        sealed_calls(&self.checked.reveal_log)
    }

    /// The four `eth_call`s [`verify_observation_onchain`] needs, in its
    /// argument order: `observationLevel`, `drawMatches`, `drawBudget`,
    /// `transcriptComplete`.
    pub fn onchain_calls(&self) -> [Vec<u8>; 4] {
        [
            self.observation_level_calldata(),
            self.draw_matches_calldata(),
            self.draw_budget_calldata(),
            self.checked.log.calldata(),
        ]
    }
}

// ------------------------------------------------- sealed reveals (ROADMAP C11)

/// The sealed-reveal calls of one served round (ROADMAP C11, ADR-0027
/// amendment 10), all built from the client's own recomputation
/// ([`observations::SealedChecked`]: every drawn observer, the round's config
/// window, the recomputed round id; review C4-R1-2), and, as [`SealedRets`],
/// their returns in the same shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedCalls {
    /// `drawRevealKey(fact, attempt)`.
    pub reveal_key: Vec<u8>,
    /// `revealLogComplete(fact, attempt, round, window, entries)`.
    pub reveal_log: Vec<u8>,
    /// `attemptMatches(..., result, entries)`, for a round without a value.
    pub matches: Option<Vec<u8>>,
    /// `sealedRevealOf` and `sealedKeyProofOf` of each served post, in order.
    pub posts: Vec<(Vec<u8>, Vec<u8>)>,
    /// `drawTimeLocked(fact, attempt)` (ROADMAP C51).
    pub time_locked: Vec<u8>,
}

/// The returns of a round's [`SealedCalls`], in the same shape.
pub type SealedRets = SealedCalls;

/// The sealed-reveal calls of the checked round `c` ([`verify_sealed_onchain`]
/// takes one return each).
pub fn sealed_calls(c: &observations::SealedChecked) -> SealedCalls {
    use onyx_coordinator::sealed::{
        draw_reveal_key_calldata, draw_time_locked_calldata, sealed_key_proof_of_calldata,
        sealed_reveal_of_calldata,
    };
    let q = &c.query;
    SealedCalls {
        time_locked: draw_time_locked_calldata(&q.registry_fact, q.attempt),
        reveal_key: draw_reveal_key_calldata(&q.registry_fact, q.attempt),
        reveal_log: q.calldata(),
        matches: (!c.value).then(|| q.matches_calldata(&c.result)),
        posts: c
            .posts
            .iter()
            .map(|(a, _)| {
                (
                    sealed_reveal_of_calldata(&q.registry_fact, q.attempt, a, &q.round_id),
                    sealed_key_proof_of_calldata(&q.registry_fact, q.attempt, a, &q.round_id),
                )
            })
            .collect(),
    }
}

/// What an honest registry answers to [`sealed_calls`] of `c` when the draw's
/// key is `chain_key`, `revealLogComplete` is `log_ok` and `attemptMatches`
/// is `matches`, every served post being the chain's: for tests and test
/// chains.
pub fn honest_sealed_returns(
    c: &observations::SealedChecked,
    chain_key: Option<onyx_coordinator::sealed::Point>,
    log_ok: bool,
    matches: bool,
) -> SealedRets {
    let word = |b: bool| {
        let mut w = vec![0u8; 32];
        w[31] = u8::from(b);
        w
    };
    SealedRets {
        time_locked: word(c.time_locked),
        reveal_key: chain_key.map_or(vec![0u8; 64], |k| k.to_vec()),
        reveal_log: word(log_ok),
        matches: (!c.value).then(|| word(matches)),
        posts: c
            .posts
            .iter()
            .map(|(_, p)| onyx_coordinator::sealed::posted_returns(Some(p)))
            .collect(),
    }
}

/// The on-chain half of a served round's sealed reveals (ROADMAP C11), from
/// the returns of [`sealed_calls`] against the registry the client trusts.
/// Requires all of:
/// - `drawTimeLocked` is exactly whether the round is served time-locked
///   (ROADMAP C51): its dismissals were then checked against the lock round's
///   beacon signature and openings off chain;
/// - `drawRevealKey` is exactly the reveal key the round names: a draw with a
///   key never comes without its sealed posts (a server cannot drop them by
///   leaving the section out), nor a draw without one with posts;
/// - `revealLogComplete` is true for the client's own query: no posted commit,
///   receipted reveal or in-phase sealed post left out (REVLOG-1, C4-1);
/// - every served post is the chain's, byte for byte (`sealedRevealOf`,
///   `sealedKeyProofOf`): a server that serves its own garbage under an
///   observer's name, with an honest dismissal of it, is refused (C11-4);
/// - for a round served sealed, and for every round without a value of a draw
///   with a reveal key, `attemptMatches` is true: the coordinator recorded
///   exactly this round (its listed observers, the window, the round id and
///   the result) before serving it (REVLOG-5, C4-8): a record for every
///   sealed round.
///
/// The dismissals were checked off chain ([`observations::check`],
/// [`observations::check_attempt`]); this function never sees a dismissal it
/// has not checked, since only a checked round has a
/// [`observations::SealedChecked`].
pub fn verify_sealed_onchain(
    c: &observations::SealedChecked,
    rets: &SealedRets,
) -> Result<(), VerifyError> {
    use onyx_coordinator::sealed::{bool_from_return, posted_from_returns, reveal_key_from_return};
    if rets.posts.len() != c.posts.len() || rets.matches.is_some() == c.value {
        return Err(VerifyError::Malformed(
            "sealed returns: one per call of sealed_calls",
        ));
    }
    let key = reveal_key_from_return(&rets.reveal_key)
        .map_err(|_| VerifyError::Malformed("drawRevealKey return"))?;
    if key != c.reveal_key {
        return Err(VerifyError::ChainMismatch(
            "drawRevealKey: the round's reveal key is not the draw's (a sealed draw served without its posts, or posts for a draw without a key)",
        ));
    }
    if !bool_from_return(&rets.reveal_log, "revealLogComplete")
        .map_err(|_| VerifyError::Malformed("revealLogComplete return"))?
    {
        return Err(VerifyError::ChainMismatch(
            "revealLogComplete: the round leaves out a posted commit, a receipted reveal or a sealed post",
        ));
    }
    for ((_, served), (r, p)) in c.posts.iter().zip(&rets.posts) {
        let chain = posted_from_returns(r, p)
            .map_err(|_| VerifyError::Malformed("sealedRevealOf/sealedKeyProofOf return"))?;
        if chain.as_ref() != Some(served) {
            return Err(VerifyError::ChainMismatch(
                "sealedRevealOf: a served sealed post is not the chain's",
            ));
        }
    }
    // ROADMAP C51 (THREATS C51-*): the round is served as time-locked exactly
    // when the draw is: a server cannot hide the time-lock of a time-locked
    // draw (its posts would be "opened" without the lock round), nor claim one
    // for a draw announced without it.
    if bool_from_return(&rets.time_locked, "drawTimeLocked")
        .map_err(|_| VerifyError::Malformed("drawTimeLocked return"))?
        != c.time_locked
    {
        return Err(VerifyError::ChainMismatch(
            "drawTimeLocked: the round is served as time-locked or not, and the draw is the other",
        ));
    }
    let needs_record = c.served_sealed || (key.is_some() && !c.value);
    if needs_record {
        let ok = match &rets.matches {
            Some(m) => bool_from_return(m, "attemptMatches")
                .map_err(|_| VerifyError::Malformed("attemptMatches return"))?,
            None => false,
        };
        if !ok {
            return Err(VerifyError::ChainMismatch(
                "attemptMatches: no record of exactly this round (its observers, window, round id and result)",
            ));
        }
    }
    Ok(())
}

/// What `drawBudget` returned (two ABI words, `uint32` strikes and
/// allowance).
pub fn draw_budget_from_return(ret: &[u8]) -> Result<(u32, u32), VerifyError> {
    let bad = || VerifyError::Malformed("drawBudget return");
    if ret.len() != 64 {
        return Err(bad());
    }
    let word = |w: &[u8]| -> Result<u32, VerifyError> {
        if w[..28].iter().any(|b| *b != 0) {
            return Err(bad());
        }
        Ok(u32::from_be_bytes(w[28..32].try_into().map_err(|_| bad())?))
    };
    Ok((word(&ret[..32])?, word(&ret[32..])?))
}

/// The on-chain half of checking a level-2 answer, from the returns of
/// [`VerifiedObservation::onchain_calls`] made against the registry the
/// client trusts (L2RND-R2-F6). Requires all of:
/// - `observationLevel` is 2: the chain recorded exactly this result for
///   this fact under this spec;
/// - `drawMatches` is true: the round's draw is the fact's latest announced
///   draw, announced before its beacon block with a commitment to exactly
///   this pool, spec (window) and config (`min_bond`), beacon from the chain
///   (L2RND-R3);
/// - the draw bound holds: the round's attempt is below the subject's
///   allowance (`MAX_DRAWS` per owner reopen), and the subject's draws
///   without a recorded value never exceed it.
///
/// - `transcriptComplete` is true (ADR-0027 amendment 8, SIG-R1-F1): the
///   served round leaves out no commit a drawn observer posted on chain in
///   the commit phase, and no reveal the coordinator's receipt key said it
///   holds. A server that drops an observer to steer the value or force a
///   re-draw is caught whenever that observer posted.
/// - the sealed-reveal checks of [`verify_sealed_onchain`] (ROADMAP C11), from
///   the returns of [`VerifiedObservation::sealed_calls`] (`sealed_ret`):
///   the draw's reveal key, `revealLogComplete`, every served post the chain's.
///
/// What this does not prove: which of the at most `MAX_DRAWS` public draws
/// the coordinator let reach a value. The bound caps that choice; it does
/// not remove it (THREATS L2-F2). Ask with `?outcome=all` and run
/// [`verify_attempts_onchain`] to see every earlier draw of the fact and the
/// rounds that settled no value (SEC-R2-F1).
pub fn verify_observation_onchain(
    v: &VerifiedObservation,
    level_ret: &[u8],
    draw_ret: &[u8],
    budget_ret: &[u8],
    complete_ret: &[u8],
    sealed_ret: &SealedRets,
) -> Result<(), VerifyError> {
    if observation_level_from_return(level_ret)? != 2 {
        return Err(VerifyError::ChainMismatch("observationLevel"));
    }
    if !draw_matches_from_return(draw_ret)? {
        return Err(VerifyError::ChainMismatch("drawMatches"));
    }
    let (strikes, allowance) = draw_budget_from_return(budget_ret)?;
    if strikes > allowance || v.checked.draw.attempt >= allowance {
        return Err(VerifyError::ChainMismatch("drawBudget"));
    }
    if !transcript_complete_from_return(complete_ret)? {
        return Err(VerifyError::ChainMismatch(
            "transcriptComplete: a logged commit or receipted reveal is left out",
        ));
    }
    // ROADMAP C11: the round's sealed posts, dismissals and reveal key.
    verify_sealed_onchain(&v.checked.reveal_log, sealed_ret)
}

/// What `transcriptComplete` returned (one ABI word, a `bool`).
pub fn transcript_complete_from_return(ret: &[u8]) -> Result<bool, VerifyError> {
    let w: [u8; 32] = ret
        .try_into()
        .map_err(|_| VerifyError::Malformed("transcriptComplete return"))?;
    if w[..31].iter().any(|b| *b != 0) || w[31] > 1 {
        return Err(VerifyError::Malformed("transcriptComplete return"));
    }
    Ok(w[31] == 1)
}

/// What `drawMatches` returned (one ABI word, a `bool`).
pub fn draw_matches_from_return(ret: &[u8]) -> Result<bool, VerifyError> {
    let w: [u8; 32] = ret
        .try_into()
        .map_err(|_| VerifyError::Malformed("drawMatches return"))?;
    if w[..31].iter().any(|b| *b != 0) || w[31] > 1 {
        return Err(VerifyError::Malformed("drawMatches return"));
    }
    Ok(w[31] == 1)
}

/// The level `observationLevel` returned (one ABI word, a `uint8`).
pub fn observation_level_from_return(ret: &[u8]) -> Result<u8, VerifyError> {
    let w: [u8; 32] = ret
        .try_into()
        .map_err(|_| VerifyError::Malformed("observationLevel return"))?;
    if w[..31].iter().any(|b| *b != 0) {
        return Err(VerifyError::Malformed("observationLevel return"));
    }
    Ok(w[31])
}

/// Check a `/v1/observations` answer against what was asked (`expect`):
/// rebuild the round from the transcript with the client's own template
/// (apply the rules a round must meet to open: config, observer count and
/// order, bonds; refuse a round settling after the client's clock or a window
/// older than `min_window_end`; instantiate the spec, recompute the round id,
/// open every commit,
/// re-aggregate the reveals into the result hash), require the served answer
/// to be the value that hash commits to, and require every other field to be
/// what that recomputation yields. Nothing the server writes is trusted
/// except as the transcript the checks consume; `metric` and `onchain` are a
/// name and a pointer (the client uses its own node and its own arguments).
pub fn verify_observation(
    body: &Value,
    expect: &ObservationExpect<'_>,
) -> Result<VerifiedObservation, VerifyError> {
    let map = body.as_object().ok_or(VerifyError::Malformed("body"))?;
    if let Some(k) = map.keys().find(|k| !OBSERVATION_KEYS.contains(&k.as_str())) {
        return Err(VerifyError::UnexpectedField(format!("body.{k}")));
    }
    if body.get("rows").is_some() || body.get("verify").is_some() {
        return Err(VerifyError::NotRequested("a shard answer"));
    }
    // A value's fact is closed: no round of this answer is sealed.
    check_limits(body, false)?;
    let t =
        observations::transcript_of(body).map_err(|e| VerifyError::Observation(e.to_string()))?;
    if &t.subject != expect.subject {
        return Err(VerifyError::NotRequested("another subject"));
    }
    let template = expect.template;
    let checked = observations::check(template, &t, expect.now)
        .map_err(|e| VerifyError::Observation(e.to_string()))?;
    let window_end = checked.spec.window_end;
    if expect.window_end.is_some_and(|w| w != window_end) {
        return Err(VerifyError::NotRequested("another window"));
    }
    if window_end < expect.min_window_end {
        return Err(VerifyError::NotRequested(
            "windowEnd older than min_window_end",
        ));
    }
    let eq_h = |key: &'static str, want: Hash32| -> Result<(), VerifyError> {
        if h32(&body[key], key)? != want {
            return Err(VerifyError::Observation(format!(
                "{key} is not what the transcript yields"
            )));
        }
        Ok(())
    };
    eq_h("templateId", template.id())?;
    eq_h("specId", checked.spec.id())?;
    eq_h("roundId", checked.round_id)?;
    eq_h("resultHash", checked.result)?;
    eq_h("factKey", checked.fact)?;
    eq_h("registryFactKey", checked.registry_fact)?;
    let eq_u = |key: &'static str, want: u64| -> Result<(), VerifyError> {
        if u64_of(&body[key], key)? != want {
            return Err(VerifyError::Observation(format!(
                "{key} is not what the transcript yields"
            )));
        }
        Ok(())
    };
    eq_u("windowEnd", window_end)?;
    eq_u("settledUnix", checked.settled_unix)?;
    eq_u("proofLevel", u64::from(observations::OBSERVED_LEVEL))?;
    // ADR-0048: the ladder holds this round's own step and nothing this
    // client cannot check; its top is the stated level.
    let own = observations::own_ladder(
        &checked.spec,
        &t.answer,
        checked.result,
        checked.settled_unix,
    )
    .ok_or_else(|| VerifyError::Observation("the answer is no value of the feed's kind".into()))?;
    let ladder = crate::ladder::check_served(
        body.get("ladder"),
        &own,
        onyx_core::ProofLevel::ReObserved,
        crate::ladder::OBSERVATION_RECORD,
        None,
    )
    .map_err(VerifyError::Ladder)?;
    let rights = &template.rights;
    if bytes(&body["rights"], "rights")? != borsh::to_vec(rights).unwrap_or_default()
        || body["license"].as_str() != Some(rights.license.as_str())
        || body["attribution"].as_str() != rights.attribution.as_deref()
    {
        return Err(VerifyError::Observation(
            "rights are not the template's".into(),
        ));
    }
    let value = observations::answer_text(template.kind, &t.answer);
    if body["value"].as_str() != Some(value.as_str())
        || body["subjectText"].as_str() != Some(observations::subject_text(&t.subject))
        || body["metric"].as_str() != Some(template.metric.0.as_str())
    {
        return Err(VerifyError::Observation(
            "value, subject text or metric name is not the transcript's".into(),
        ));
    }
    match &body["onchain"] {
        Value::Null => {}
        p => onchain_hint(
            p,
            &["chainId", "registry", "call", "drawMatches", "drawBudget"],
        )?,
    }
    if let Some(n) = body.get("recheck") {
        recheck_shape(n)?;
    }
    let attempts = match body.get("attempts") {
        None => None,
        Some(arr) => Some(VerifiedAttempts {
            attempts: check_attempts(
                arr,
                template,
                &t.subject,
                Some(checked.fact),
                expect.now,
                Some(checked.draw.attempt),
                // The fact has a value: it is closed, so nothing is sealed.
                false,
            )?,
            registry_fact: checked.registry_fact,
            metric_key: checked.onchain.metric_key,
            subject_key: checked.onchain.subject_key,
            dated: template.is_dated(),
            value_attempt: Some(checked.draw.attempt),
        }),
    };
    Ok(VerifiedObservation {
        answer: t.answer,
        value,
        checked,
        ladder,
        attempts,
    })
}

/// A served `recheck` note's shape: exactly [`observations::RECHECK_KEYS`],
/// two hashes, a time and a flag. What it states is only a claim until
/// [`verify_recheck`] recomputes it from the recheck's own answer.
fn recheck_shape(n: &Value) -> Result<(), VerifyError> {
    let m = n.as_object().ok_or(VerifyError::Malformed("recheck"))?;
    if let Some(k) = m
        .keys()
        .find(|k| !observations::RECHECK_KEYS.contains(&k.as_str()))
    {
        return Err(VerifyError::UnexpectedField(format!("recheck.{k}")));
    }
    h32(&n["result"], "recheck.result")?;
    h32(&n["valueHash"], "recheck.valueHash")?;
    u64_of(&n["recordedAt"], "recheck.recordedAt")?;
    if !n["matches"].is_boolean() {
        return Err(VerifyError::Malformed("recheck.matches"));
    }
    Ok(())
}

/// CL7 (ADR-0057 item 6): the `recheck` note served with `value_body`,
/// recomputed from the value's verified answer and the paired `.recheck`
/// metric's verified answer of the same subject (both from
/// [`verify_observation`], the recheck one under the client's own
/// `onyx_core::recheck_of` template). Refused when the served note is
/// missing, differs in any field, or the two answers are of other subjects.
/// A "No" (`matches: false`) is a note next to the value; the value's level
/// is unchanged.
pub fn verify_recheck(
    value_body: &Value,
    value: &VerifiedObservation,
    recheck_template: &FeedTemplate,
    recheck: &VerifiedObservation,
) -> Result<onyx_coordinator::corrections::RecheckNote, VerifyError> {
    let served = value_body
        .get("recheck")
        .ok_or(VerifyError::Malformed("recheck: missing"))?;
    recheck_shape(served)?;
    let want = observations::recheck_note_of(
        &value.answer,
        &value.checked.spec.subject,
        recheck_template,
        &recheck.answer,
        &recheck.checked,
    )
    .ok_or_else(|| {
        VerifyError::Observation("the recheck is no number of the same subject".into())
    })?;
    if *served != observations::recheck_json(&want) {
        return Err(VerifyError::Observation(
            "recheck: not what the recheck's own answer yields".into(),
        ));
    }
    Ok(want)
}

// ------------------------------------------------------------- payTo pin

/// The `OnyxRevenue` a client pays, pinned before it signs (REV-4): a server
/// whose `PAYMENT-REQUIRED` names another `payTo` (the operator's own wallet,
/// say) gets no signature, so contributors cannot be cut out of a sale by a
/// changed address. Built from a deployment manifest the client trusts (the
/// deploy run's output, published by the project), then checked against the
/// client's own node with [`PayToPin::check_code`]: the node must be on the
/// pinned chain and hold exactly the pinned runtime code at the address.
/// Only the [`VerifiedPin`] that check returns can choose a requirement, so a
/// client cannot skip it (REV-L3-F9).
///
/// Manifest (JSON): `{"chainId": 84532, "revenue": "0x…", "usdc": "0x…",
/// "revenueCodeHash": "0x…"}`, all required: `revenueCodeHash` is keccak-256
/// of the runtime code. The code hash binds the contract's immutables (token,
/// ledger, owner, treasury); only the reporter, set by the owner, is not
/// bound by it (REV-12).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayToPin {
    /// EIP-155 chain id.
    pub chain_id: u64,
    /// CAIP-2 network (`eip155:<chainId>`).
    pub network: String,
    /// The token payments are made in.
    pub asset: [u8; 20],
    /// The pinned `OnyxRevenue` (the only acceptable `payTo`).
    pub pay_to: [u8; 20],
    /// keccak-256 of its runtime code.
    pub code_hash: [u8; 32],
}

/// A [`PayToPin`] the client's own node confirmed: the only way to choose a
/// requirement to sign.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedPin {
    pin: PayToPin,
}

/// Why a client refuses to pay.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    /// No offered requirement pays the pinned contract in the pinned token on
    /// the pinned network.
    #[error("no payment option pays the pinned OnyxRevenue {pinned} on {network}; offered payTo {offered:?}")]
    NotPinned {
        /// The pinned address.
        pinned: String,
        /// The pinned network.
        network: String,
        /// Every `payTo` the server offered.
        offered: Vec<String>,
    },
    /// The pinned address does not hold the pinned code.
    #[error("the code at {0} is not the pinned OnyxRevenue")]
    CodeMismatch(String),
    /// The client's node is on another chain than the pin.
    #[error("the node is on chain {got}, the pin is for chain {want}")]
    WrongChain {
        /// The node's `eth_chainId`.
        got: u64,
        /// The pinned chain.
        want: u64,
    },
    /// The manifest is not usable.
    #[error("deployment manifest: {0}")]
    Manifest(&'static str),
    /// The client's node could not be asked.
    #[error("rpc: {0}")]
    Rpc(String),
}

fn addr20(s: &str) -> Option<[u8; 20]> {
    let s = s.trim();
    let h = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    hex::decode(h).ok()?.try_into().ok()
}

fn hash32(s: &str) -> Option<[u8; 32]> {
    hex::decode(s.trim().strip_prefix("0x")?)
        .ok()?
        .try_into()
        .ok()
}

impl PayToPin {
    /// A pin from a deployment manifest (see the type docs). Every field is
    /// required: a manifest without the code hash is refused, never read as
    /// "any code" (REV-L3-F9).
    pub fn from_manifest(v: &Value) -> Result<Self, PinError> {
        let chain_id = v["chainId"]
            .as_u64()
            .ok_or(PinError::Manifest("chainId must be a number"))?;
        let pay_to = v["revenue"]
            .as_str()
            .and_then(addr20)
            .ok_or(PinError::Manifest("revenue must be a 20-byte hex address"))?;
        let asset = v["usdc"]
            .as_str()
            .and_then(addr20)
            .ok_or(PinError::Manifest("usdc must be a 20-byte hex address"))?;
        let code_hash =
            v["revenueCodeHash"]
                .as_str()
                .and_then(hash32)
                .ok_or(PinError::Manifest(
                    "revenueCodeHash (32 bytes of hex) is required",
                ))?;
        if pay_to == [0u8; 20] || asset == [0u8; 20] {
            return Err(PinError::Manifest("zero address"));
        }
        Ok(PayToPin {
            chain_id,
            network: format!("eip155:{chain_id}"),
            asset,
            pay_to,
            code_hash,
        })
    }

    /// Check the pin on the client's own node: `eth_chainId` must be the
    /// pinned chain and `eth_getCode` at `latest` must hash to the pinned
    /// code. Returns the [`VerifiedPin`] that can choose a requirement.
    pub async fn check_code<T: crate::roots::JsonRpc + ?Sized>(
        &self,
        rpc: &T,
    ) -> Result<VerifiedPin, PinError> {
        let id = rpc
            .call("eth_chainId", serde_json::json!([]))
            .await
            .map_err(PinError::Rpc)?;
        let got = id
            .as_str()
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .ok_or_else(|| PinError::Rpc(format!("eth_chainId: not a quantity: {id}")))?;
        if got != self.chain_id {
            return Err(PinError::WrongChain {
                got,
                want: self.chain_id,
            });
        }
        let addr = format!("0x{}", hex::encode(self.pay_to));
        let code = rpc
            .call("eth_getCode", serde_json::json!([addr, "latest"]))
            .await
            .map_err(PinError::Rpc)?;
        let code = hex::decode(code.as_str().unwrap_or_default().trim_start_matches("0x"))
            .map_err(|e| PinError::Rpc(format!("eth_getCode: {e}")))?;
        if code.is_empty() || keccak256(&code).0 != self.code_hash {
            return Err(PinError::CodeMismatch(addr));
        }
        Ok(VerifiedPin { pin: self.clone() })
    }
}

impl VerifiedPin {
    /// The pin that was checked.
    pub fn pin(&self) -> &PayToPin {
        &self.pin
    }

    /// The `exact` requirement that pays the pinned contract, or a refusal.
    /// Addresses compare as bytes, so checksum case never matters.
    pub fn select<'a>(
        &self,
        pr: &'a crate::x402::PaymentRequired,
    ) -> Result<&'a crate::x402::PaymentRequirements, PinError> {
        let p = &self.pin;
        pr.accepts
            .iter()
            .find(|r| {
                r.scheme == "exact"
                    && r.network == p.network
                    && addr20(&r.asset) == Some(p.asset)
                    && addr20(&r.pay_to) == Some(p.pay_to)
            })
            .ok_or_else(|| PinError::NotPinned {
                pinned: format!("0x{}", hex::encode(p.pay_to)),
                network: p.network.clone(),
                offered: pr.accepts.iter().map(|r| r.pay_to.clone()).collect(),
            })
    }
}

#[cfg(test)]
mod pin_tests {
    use super::*;
    use crate::roots::JsonRpc;
    use crate::x402::{BoxFuture, PaymentRequired, PaymentTerms, ResourceInfo};
    use serde_json::json;

    const REVENUE: &str = "0x5E5E5e5e5E5e5e5E5e5E5e5e5e5E5e5e5e5e5E5e";
    const USDC: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";
    const CODE: &[u8] = b"\x60\x80\x60\x40";

    fn manifest(hash: [u8; 32]) -> Value {
        json!({"chainId": 84_532, "revenue": REVENUE, "usdc": USDC,
               "revenueCodeHash": format!("0x{}", hex::encode(hash))})
    }

    fn challenge(pay_to: &str) -> PaymentRequired {
        let terms = PaymentTerms {
            network: "eip155:84532".into(),
            asset: USDC.to_lowercase(),
            pay_to: pay_to.into(),
            max_timeout_seconds: 60,
            extra: None,
        };
        PaymentRequired {
            x402_version: 2,
            error: None,
            resource: ResourceInfo {
                url: "https://api.example/v1/metrics/x".into(),
                description: None,
                mime_type: None,
            },
            accepts: vec![terms.requirements(2_500)],
            extensions: None,
        }
    }

    /// A node on `chain` with `code` at every address.
    struct Code {
        chain: u64,
        code: Vec<u8>,
    }

    impl JsonRpc for Code {
        fn call<'a>(&'a self, method: &'a str, _p: Value) -> BoxFuture<'a, Result<Value, String>> {
            let r = match method {
                "eth_chainId" => Ok(json!(format!("0x{:x}", self.chain))),
                "eth_getCode" => Ok(json!(format!("0x{}", hex::encode(&self.code)))),
                _ => Err(method.to_string()),
            };
            Box::pin(async move { r })
        }
    }

    fn node() -> Code {
        Code {
            chain: 84_532,
            code: CODE.to_vec(),
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
    }

    /// REV-4: the pinned contract is paid whatever the address case; any
    /// other `payTo`, network or token is refused before signing.
    #[test]
    fn a_client_pays_only_the_pinned_revenue_contract() {
        let pin = rt()
            .block_on(
                PayToPin::from_manifest(&manifest(keccak256(CODE).0))
                    .expect("manifest")
                    .check_code(&node()),
            )
            .expect("checked");
        let ok = challenge(&REVENUE.to_lowercase());
        assert_eq!(pin.select(&ok).expect("pinned").amount, "2500");
        // The operator's own wallet as payTo: refused.
        let swapped = challenge("0x1111111111111111111111111111111111111111");
        let e = pin.select(&swapped).expect_err("not pinned");
        assert!(
            matches!(&e, PinError::NotPinned { offered, .. } if offered.len() == 1),
            "{e}"
        );
        let mut other_net = ok.clone();
        other_net.accepts[0].network = "eip155:8453".into();
        assert!(
            pin.select(&other_net).is_err(),
            "mainnet Base is not the pin"
        );
        let mut other_token = ok.clone();
        other_token.accepts[0].asset = "0x2222222222222222222222222222222222222222".into();
        assert!(pin.select(&other_token).is_err());
        let mut other_scheme = ok;
        other_scheme.accepts[0].scheme = "upto".into();
        assert!(pin.select(&other_scheme).is_err());
    }

    /// REV-L3-F9: a manifest must name real addresses and the code hash (no
    /// "any code" downgrade); the client's own node must be on the pinned
    /// chain and hold exactly the pinned code. Only a checked pin selects.
    #[test]
    fn the_pin_needs_a_code_hash_the_right_chain_and_the_right_code() {
        assert!(PayToPin::from_manifest(
            &json!({"chainId": 84_532, "revenue": "0x12", "usdc": USDC,
                    "revenueCodeHash": format!("0x{}", hex::encode([1u8; 32]))})
        )
        .is_err());
        let e =
            PayToPin::from_manifest(&json!({"chainId": 84_532, "revenue": REVENUE, "usdc": USDC}))
                .expect_err("no code hash");
        assert!(matches!(e, PinError::Manifest(_)), "{e}");
        assert!(PayToPin::from_manifest(&json!({"revenue": REVENUE, "usdc": USDC})).is_err());
        rt().block_on(async {
            let pin = PayToPin::from_manifest(&manifest(keccak256(CODE).0)).expect("pin");
            assert!(pin.check_code(&node()).await.is_ok());
            let other_code = Code {
                chain: 84_532,
                code: b"\x60\x00".to_vec(),
            };
            assert!(matches!(
                pin.check_code(&other_code).await,
                Err(PinError::CodeMismatch(_))
            ));
            let wallet = Code {
                chain: 84_532,
                code: Vec::new(),
            };
            assert!(matches!(
                pin.check_code(&wallet).await,
                Err(PinError::CodeMismatch(_))
            ));
            let mainnet = Code {
                chain: 8_453,
                code: CODE.to_vec(),
            };
            assert_eq!(
                pin.check_code(&mainnet).await,
                Err(PinError::WrongChain {
                    got: 8_453,
                    want: 84_532
                })
            );
        });
    }
}
