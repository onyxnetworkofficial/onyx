//! Chain indexer (ADR-0033, docs/spec/delivery.md "Indexer"): discovers the
//! shards to serve from `OnyxSettlement`'s `Finalized(uint64,bytes32,bytes32,bytes32)`
//! events instead of a hand-written list.
//!
//! - **What it reads.** Logs of the one settlement contract with the pinned
//!   [`FINALIZED_TOPIC`], from the deployment's block up to the node's
//!   `finalized` block only (a block a reorg can still remove is never read),
//!   in ranges of at most [`IndexerConfig::span`] blocks. A range the node
//!   refuses (public nodes cap `eth_getLogs` spans; sepolia.base.org answers
//!   HTTP 413 "limited to a 200 range") is split in halves down to one
//!   block (and stays that size for the rest of the poll); a one-block range
//!   that still fails is read again for the held shards only (their ids as a
//!   topic filter, after the block's header shows the node is up), so one
//!   block with more logs than an answer may carry never stops the scan
//!   (IDX-R1-2); if that fails too the poll ends and resumes there next time.
//!   Each poll also reads again the last
//!   [`IndexerConfig::overlap`] blocks, so a log a lagging backend of a
//!   load-balanced node left out is found on a later poll; logs are keyed by
//!   (block, log index), so a repeat is ignored. Two different logs at one
//!   position are both kept as candidates and counted on `/v1/health`: an
//!   event is a hint, and the contract view decides (IDX-R1-1). A position
//!   holds at most [`MAX_CANDIDATES_PER_LOG`]; when it is full, a candidate
//!   whose shard is not held, then one whose (round, shard) another kept log
//!   also names makes room; never the last log naming a held (round, shard),
//!   whatever its chain status reads said (IDX-R2-1, IDX-R4-1, see
//!   `add_log`).
//! - **Threat model (ADR-0033 Amendments 4 and 5).** The indexer trusts the
//!   operator's node for the operator's liveness. A lying node can delay
//!   serving a held shard, and the held-shard rescan undoes that once it
//!   answers honestly. It can also list a wrong root: `ChainRoots` reads
//!   roots through the same node, so correctness against a lying operator
//!   node rests on clients that check roots on their own node
//!   (`client::verify_response` with a chain answer, IDX-R5-3). From the
//!   [`STARTUP_RESCAN_POLL`]th poll after a start and every
//!   [`IndexerConfig::rescan_every`] polls after the previous one ended, the
//!   held shards' logs are read again from the deploy block (their ids as a
//!   topic-2 filter, in the same capped ranges, at most
//!   [`IndexerConfig::rescan_budget`] calls per poll) and merged as
//!   candidates, every confirmed end is read again, and so is every served
//!   shard's final view (two reads on two polls refuting it take it out of
//!   the catalog, IDX-R5-2). A log the rescan found may replace, at its
//!   position, a held log that answer did not return and whose end two
//!   polls confirmed.
//! - **Cursor.** The next block to read and every event seen are written to a
//!   state file (atomically: temp file, fsync, rename) bound to the chain id,
//!   contract and deploy block, once per poll and every
//!   [`SAVE_EVERY_RANGES`] ranges of a long scan, on a blocking thread (not
//!   the async runtime's); a restart resumes there and judges the saved
//!   events again. A node reporting an older `finalized` block than seen
//!   before (a lagging backend) moves nothing backwards. A poll reads at most
//!   [`IndexerConfig::max_jump`] blocks past the head this poll and the
//!   previous one both reported, so one lying answer far ahead cannot move
//!   the cursor past blocks the overlap reads again (IDX-R5-5). Judging
//!   reads the contract at this poll's head, never a remembered one
//!   (IDX-R5-1).
//! - **The event is never trusted.** A (round, shard) becomes servable only
//!   if (1) the operator holds the shard's spec ([`Holdings`]) and a kept log
//!   names the (round, shard), (2) the contract's own view
//!   `getFinalizedRoot(round, shard)`, read at the indexed finalized block,
//!   names a final root and binding (an event's root and binding are not
//!   required to match: the contract decides, IDX-R2-1), (3)
//!   the rows the operator holds hash to that root, their count and the spec
//!   give exactly that binding, and (4) a pinned label set passes the
//!   evidence half of `LabelSet::check_servable`: the set is held, every
//!   label cites evidence, and every cited hash resolves in the operator's
//!   store ([`check_held`]). A `test.*` set is refused unless the server runs
//!   in dev mode (where it is served, as before, without that check).
//!   Then the shard is floored at the level the registry attested for it,
//!   when a registry is pinned, and added to the catalog. The API still reads
//!   the root from the chain on every lookup (`ChainRoots`, through the same
//!   node) and checks the rows again before serving them.
//! - **Known but not served.** Every (round, shard) seen is listed with its
//!   status on `/v1/health` (`not_held`, `pending`, or the refusal), and an
//!   event whose root the contract does not hold is listed as superseded.
//!   A shard whose case ended without a root (`NoValidRoot`, `Unresolved`),
//!   whose round was voided or empty, or that the contract never opened (a
//!   forged event) is marked ended and no longer polled (IDX-R1-3) once a
//!   second read on a later poll agrees (IDX-R3-2).
//! - **No single answer is final (IDX-R3).** An end, or a final view that
//!   refuses the held data, is acted on for good only when two reads on
//!   different polls agree; a read that disagrees starts over. A final root
//!   the held rows match admits when `shardState` also reads Finalized, or
//!   when two reads on different polls agree (IDX-R4-2); the API reads the
//!   root from the chain again on every request (through the same node).
//! - **Deploy block.** [`Indexer::check_deploy_block`] refuses a `deployBlock`
//!   that is not the block the contract's code appeared in (IDX-R1-4), after
//!   retrying node errors, and says which of four causes it was (IDX-R2-2).

use crate::catalog::{Catalog, ShardMeta};
use crate::roots::{decode_finalized_root, get_finalized_root_calldata, JsonRpc, FINALIZED_TOPIC};
use onyx_coordinator::cas::{Cas, CasExt, Kind};
use onyx_core::{DataRights, EvidenceRef, Hash32, LabelSetRef, ProofLevel, ShardSpec};
use onyx_merkle::MerkleTree;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

/// Version of the state file this code reads and writes.
pub const STATE_VERSION: u64 = 1;
/// Default blocks per `eth_getLogs` call (sepolia.base.org's cap, 2026-10-12).
pub const DEFAULT_SPAN: u64 = 200;
/// Largest configurable span.
pub const MAX_SPAN: u64 = 10_000;
/// Default blocks read again on every poll (half the default span, so a
/// caught-up poll is one call).
pub const DEFAULT_OVERLAP: u64 = 100;
/// A range answering more logs than this is split (bounded memory per call).
pub const MAX_LOGS_PER_RANGE: usize = 10_000;
/// `eth_getLogs` calls per poll at most for the scan; the next poll
/// continues.
pub const MAX_RANGES_PER_POLL: u64 = 2_000;
/// Default `eth_getLogs` calls per poll for the held-shard rescan, apart from
/// the scan's budget (IDX-R5-4); the next poll continues where it stopped.
pub const DEFAULT_RESCAN_BUDGET: u64 = 100;
/// Default blocks the scan may read past the head two polls agree on
/// (IDX-R5-5): the default overlap, so the blocks one lying `finalized`
/// answer let the scan pass are all read again by the overlap.
pub const DEFAULT_MAX_JUMP: u64 = DEFAULT_OVERLAP;
/// Largest configurable jump.
pub const MAX_MAX_JUMP: u64 = 1_000_000;
/// Shards listed on `/v1/health` at most (the counts cover all).
pub const HEALTH_LIST: usize = 64;
/// Different logs kept for one (block, log index) when re-reads disagree
/// (IDX-R1-1); a full position makes room by eviction or refuses the newcomer
/// (IDX-R2-1, `add_log`).
pub const MAX_CANDIDATES_PER_LOG: usize = 4;
/// Waits between attempts of one read in [`Indexer::check_deploy_block`]
/// (IDX-R2-2): four attempts over about six seconds.
pub const DEPLOY_CHECK_BACKOFF_MS: [u64; 3] = [500, 1_500, 4_000];
/// Held shard ids per topic-filtered `eth_getLogs` call when one block is
/// read for the held shards only (IDX-R1-2).
pub const HELD_TOPICS_PER_CALL: usize = 64;
/// `eth_getLogs` ranges between state-file writes in a long scan (IDX-R1-5).
pub const SAVE_EVERY_RANGES: u64 = 50;
/// `shardState(uint64,bytes32)` selector, pinned (`cast sig`).
pub const SHARD_STATE_SELECTOR: [u8; 4] = [0x64, 0x21, 0xbf, 0x40];
/// `OnyxLedger.ShardStatus.Finalized`.
const STATUS_FINALIZED: u8 = 3;
/// Default polls between held-shard rescans from `deployBlock` (ADR-0033
/// Amendment 4): at a 15 s poll, one an hour.
pub const DEFAULT_RESCAN_EVERY: u64 = 240;
/// Largest configurable rescan interval, in polls.
pub const MAX_RESCAN_EVERY: u64 = 1_000_000;
/// The poll (counted from 0 after a start) on which the held-shard rescan
/// of a resumed state file starts.
pub const STARTUP_RESCAN_POLL: u64 = 2;

/// Where and how to index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexerConfig {
    /// EIP-155 chain id the node must report.
    pub chain_id: u64,
    /// `OnyxSettlement`.
    pub contract: [u8; 20],
    /// The block the contract was deployed in (no event can be older).
    pub deploy_block: u64,
    /// Blocks per `eth_getLogs` call, 1..=[`MAX_SPAN`].
    pub span: u64,
    /// Blocks before the cursor read again each poll, 0..=[`MAX_SPAN`].
    pub overlap: u64,
    /// Dev mode: a shard pinned to a `test.*` label set may be served.
    pub allow_test_labels: bool,
    /// `OnyxRegistry`, when pinned: each admitted shard is floored at its
    /// attested level (AUD-2R-F3). Its code is checked by the caller.
    pub registry: Option<[u8; 20]>,
    /// `/v1/health` lists every known shard and the last error's text
    /// (default). Off, it shows counts only: no shard ids (which say what the
    /// operator holds), no error or conflict text (IDX-R1-5).
    pub health_detail: bool,
    /// Polls between held-shard rescans from `deployBlock` (ADR-0033
    /// Amendment 4), counted from the end of the previous one (IDX-R5-4); a
    /// rescan also runs after every start. 0: at start only.
    pub rescan_every: u64,
    /// `eth_getLogs` calls per poll for the held-shard rescan,
    /// 1..=[`MAX_RANGES_PER_POLL`], apart from the scan's own budget
    /// (IDX-R5-4).
    pub rescan_budget: u64,
    /// Blocks the scan may read past the `finalized` head that this poll and
    /// the previous one both reported (or past the cursor),
    /// 1..=[`MAX_MAX_JUMP`] (IDX-R5-5).
    pub max_jump: u64,
}

impl IndexerConfig {
    /// Defaults: [`DEFAULT_SPAN`], [`DEFAULT_OVERLAP`], no test labels, no
    /// registry, full health detail, a rescan every [`DEFAULT_RESCAN_EVERY`]
    /// polls with [`DEFAULT_RESCAN_BUDGET`] calls per poll, a jump of
    /// [`DEFAULT_MAX_JUMP`] blocks.
    pub fn new(chain_id: u64, contract: [u8; 20], deploy_block: u64) -> Self {
        IndexerConfig {
            chain_id,
            contract,
            deploy_block,
            span: DEFAULT_SPAN,
            overlap: DEFAULT_OVERLAP,
            allow_test_labels: false,
            registry: None,
            health_detail: true,
            rescan_every: DEFAULT_RESCAN_EVERY,
            rescan_budget: DEFAULT_RESCAN_BUDGET,
            max_jump: DEFAULT_MAX_JUMP,
        }
    }

    fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_SPAN).contains(&self.span) {
            return Err(format!("indexer span must be 1..={MAX_SPAN} blocks"));
        }
        if self.overlap > MAX_SPAN {
            return Err(format!("indexer overlap must be 0..={MAX_SPAN} blocks"));
        }
        if self.rescan_every > MAX_RESCAN_EVERY {
            return Err(format!(
                "indexer rescan interval must be 0..={MAX_RESCAN_EVERY} polls"
            ));
        }
        if !(1..=MAX_RANGES_PER_POLL).contains(&self.rescan_budget) {
            return Err(format!(
                "indexer rescan budget must be 1..={MAX_RANGES_PER_POLL} calls per poll"
            ));
        }
        if !(1..=MAX_MAX_JUMP).contains(&self.max_jump) {
            return Err(format!(
                "indexer max jump must be 1..={MAX_MAX_JUMP} blocks"
            ));
        }
        Ok(())
    }
}

/// One `Finalized(round, shard, root, binding)` log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedLog {
    /// Round (topic 1).
    pub round: u64,
    /// Shard id (topic 2).
    pub shard: Hash32,
    /// Root the event names (data word 0). Never trusted alone.
    pub root: Hash32,
    /// Binding the event names (data word 1).
    pub binding: Hash32,
    /// Block number.
    pub block: u64,
    /// Log index in the block.
    pub log_index: u64,
    /// Transaction hash, when the node gave a well-formed one.
    pub tx: Option<String>,
}

fn hex_u64(v: &Value, what: &str) -> Result<u64, String> {
    let s = v
        .as_str()
        .ok_or_else(|| format!("{what}: not a hex quantity"))?;
    let digits = s
        .strip_prefix("0x")
        .ok_or_else(|| format!("{what}: no 0x"))?;
    if digits.is_empty() || digits.len() > 16 {
        return Err(format!("{what}: bad quantity {s}"));
    }
    u64::from_str_radix(digits, 16).map_err(|e| format!("{what}: {e}"))
}

fn hex_bytes(v: &Value, what: &str) -> Result<Vec<u8>, String> {
    let s = v.as_str().ok_or_else(|| format!("{what}: not a string"))?;
    hex::decode(
        s.strip_prefix("0x")
            .ok_or_else(|| format!("{what}: no 0x"))?,
    )
    .map_err(|e| format!("{what}: {e}"))
}

fn word(v: &Value, what: &str) -> Result<[u8; 32], String> {
    hex_bytes(v, what)?
        .try_into()
        .map_err(|_| format!("{what}: not 32 bytes"))
}

fn h32(s: &str, what: &str) -> Result<Hash32, String> {
    word(&json!(s), what).map(Hash32)
}

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// Why a retried read failed (IDX-R2-2).
#[derive(Debug)]
enum RpcFailure {
    /// The node answered that it does not keep the state asked for.
    StateMissing(String),
    /// Every attempt failed (down, rate-limited, timing out).
    Node { tries: usize, last: String },
}

impl std::fmt::Display for RpcFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            RpcFailure::StateMissing(e) => write!(f, "the node lacks that state: {e}"),
            RpcFailure::Node { tries, last } => write!(
                f,
                "the node failed {tries} times (last: {last}); it is down, rate-limiting or timing out: retry later or use another node"
            ),
        }
    }
}

/// A node error that says the node does not keep old state (a pruned, full
/// node), as geth-style and other clients word it. Not retried.
fn state_missing(e: &str) -> bool {
    let e = e.to_ascii_lowercase();
    [
        "missing trie node",
        "pruned",
        "historical state",
        "state not available",
        "state is not available",
        "state histories",
        "archive",
    ]
    .iter()
    .any(|p| e.contains(p))
}

/// Parse one log of an `eth_getLogs` answer for blocks `from..=to`: it must
/// come from `contract`, carry exactly the pinned topic and two indexed
/// words (a round that fits u64 and a shard), two data words, a block in the
/// asked range, and not be marked removed. Anything else is an error, so a
/// malformed answer is retried, never half read.
pub fn parse_log(
    v: &Value,
    contract: &[u8; 20],
    from: u64,
    to: u64,
) -> Result<FinalizedLog, String> {
    let addr = hex_bytes(&v["address"], "log address")?;
    if addr != contract {
        return Err(format!(
            "log from {}, not the settlement contract",
            hx(&addr)
        ));
    }
    if v["removed"] == json!(true) {
        return Err("log marked removed at a finalized block".into());
    }
    let topics = v["topics"].as_array().ok_or("log has no topics")?;
    if topics.len() != 3 {
        return Err(format!("log has {} topics, want 3", topics.len()));
    }
    if topics[0].as_str().map(str::to_ascii_lowercase).as_deref() != Some(FINALIZED_TOPIC) {
        return Err("log topic is not Finalized(uint64,bytes32,bytes32,bytes32)".into());
    }
    let r = word(&topics[1], "round topic")?;
    if r[..24].iter().any(|b| *b != 0) {
        return Err("round topic does not fit u64".into());
    }
    let mut rb = [0u8; 8];
    rb.copy_from_slice(&r[24..]);
    let shard = Hash32(word(&topics[2], "shard topic")?);
    let data = hex_bytes(&v["data"], "log data")?;
    if data.len() != 64 {
        return Err(format!("log data is {} bytes, want 64", data.len()));
    }
    let mut root = [0u8; 32];
    root.copy_from_slice(&data[..32]);
    let mut binding = [0u8; 32];
    binding.copy_from_slice(&data[32..]);
    let block = hex_u64(&v["blockNumber"], "log blockNumber")?;
    if block < from || block > to {
        return Err(format!(
            "log at block {block} outside the asked range {from}..={to}"
        ));
    }
    let log_index = hex_u64(&v["logIndex"], "log logIndex")?;
    let tx = v["transactionHash"]
        .as_str()
        .filter(|s| word(&json!(s), "tx").is_ok())
        .map(str::to_ascii_lowercase);
    Ok(FinalizedLog {
        round: u64::from_be_bytes(rb),
        shard,
        root: Hash32(root),
        binding: Hash32(binding),
        block,
        log_index,
        tx,
    })
}

/// What the operator holds for one shard: its metadata from the spec (the
/// round is set when the indexer finds it) and the pinned label set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldShard {
    /// Metadata; `round` is ignored until admission, `leaf_count` (when set)
    /// must equal the held rows' count.
    pub meta: ShardMeta,
    /// The spec's label set, for the evidence check.
    pub label_set: Option<LabelSetRef>,
}

impl HeldShard {
    /// From the spec and the source's rights (whose hash must be the spec's).
    pub fn from_spec(
        spec: &ShardSpec,
        metric: impl Into<String>,
        rights: DataRights,
    ) -> Option<Self> {
        Some(HeldShard {
            meta: ShardMeta::from_spec(0, spec, metric, rights)?,
            label_set: spec.label_set.clone(),
        })
    }
}

/// The shards the operator holds specs for, by shard id. A shard id is
/// finalized at most once on chain (`everFinalized`), so the round comes
/// from the event.
#[derive(Clone, Debug, Default)]
pub struct Holdings {
    by_shard: BTreeMap<Hash32, HeldShard>,
}

impl Holdings {
    /// None held.
    pub fn new() -> Self {
        Self::default()
    }

    /// Hold `h`; refuses a second spec for one shard id.
    pub fn insert(&mut self, h: HeldShard) -> Result<(), String> {
        let id = h.meta.shard;
        if self.by_shard.contains_key(&id) {
            return Err(format!("shard {id} is held twice"));
        }
        self.by_shard.insert(id, h);
        Ok(())
    }

    /// The held spec of `shard`.
    pub fn get(&self, shard: &Hash32) -> Option<&HeldShard> {
        self.by_shard.get(shard)
    }

    /// Held shard ids.
    pub fn ids(&self) -> impl Iterator<Item = &Hash32> {
        self.by_shard.keys()
    }

    /// How many.
    pub fn len(&self) -> usize {
        self.by_shard.len()
    }

    /// None held.
    pub fn is_empty(&self) -> bool {
        self.by_shard.is_empty()
    }

    /// Keccak-256 of the held shard ids in order (M7L3-R1-2): a rescan
    /// cursor saved with it is resumed only for the same held set.
    pub fn ids_hash(&self) -> Hash32 {
        let mut b = Vec::with_capacity(self.by_shard.len() * 32);
        for id in self.by_shard.keys() {
            b.extend_from_slice(&id.0);
        }
        onyx_core::keccak256(&b)
    }
}

/// Why a held, final shard is not served.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The operator's store has no rows for the root.
    RowsMissing,
    /// The held row count differs from the stated `leafCount`.
    LeafCountMismatch,
    /// The spec and the rows give another binding than the contract's.
    BindingMismatch,
    /// The pinned label set is not in the store (or not the pinned one).
    LabelSetMissing,
    /// A label cites no evidence, or evidence the store cannot resolve.
    EvidenceMissing(String),
    /// A `test.*` label set outside dev mode.
    TestLabels,
    /// The metric name is already a level-2 metric.
    MetricConflict,
}

impl Refusal {
    /// Stable code for `/v1/health`.
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::RowsMissing => "rows_missing",
            Refusal::LeafCountMismatch => "leaf_count_mismatch",
            Refusal::BindingMismatch => "binding_mismatch",
            Refusal::LabelSetMissing => "label_set_missing",
            Refusal::EvidenceMissing(_) => "evidence_missing",
            Refusal::TestLabels => "test_labels",
            Refusal::MetricConflict => "metric_conflict",
        }
    }
}

/// How the contract says a (round, shard) ended without a final root
/// (`shardState`, IDX-R1-3). None of these can become final later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// A dispute case ended `NoValidRoot`: no revealed root was right.
    NoValidRoot,
    /// A dispute case ended `Unresolved`.
    Unresolved,
    /// The round was voided.
    Voided,
    /// Nobody revealed.
    Empty,
    /// The contract has not opened this (round, shard) at a block after the
    /// event's: the event was forged. Judged again if another event naming
    /// it is kept.
    NotOnChain,
}

impl Ended {
    /// Stable code for `/v1/health`.
    pub fn code(&self) -> &'static str {
        match self {
            Ended::NoValidRoot => "no_valid_root",
            Ended::Unresolved => "unresolved",
            Ended::Voided => "voided",
            Ended::Empty => "empty",
            Ended::NotOnChain => "not_on_chain",
        }
    }

    /// From `OnyxLedger.ShardStatus` (None, Open, Pending, Finalized, Empty,
    /// Voided, InCase, NoValidRoot, Unresolved): `Some` for a state no root
    /// can come out of, `None` while one still can. Above 8 is an error.
    pub fn from_status(status: u8) -> Result<Option<Ended>, String> {
        Ok(match status {
            0 => Some(Ended::NotOnChain),
            1 | 2 | 3 | 6 => None,
            4 => Some(Ended::Empty),
            5 => Some(Ended::Voided),
            7 => Some(Ended::NoValidRoot),
            8 => Some(Ended::Unresolved),
            s => return Err(format!("shardState status {s} is not a ShardStatus")),
        })
    }
}

/// ABI calldata for `shardState(round, shard)`.
pub fn shard_state_calldata(round: u64, shard: &Hash32) -> Vec<u8> {
    let mut d = SHARD_STATE_SELECTOR.to_vec();
    let mut word = [0u8; 32];
    word[24..].copy_from_slice(&round.to_be_bytes());
    d.extend_from_slice(&word);
    d.extend_from_slice(&shard.0);
    d
}

/// The status word of a `shardState` return `(uint8 status, bool closed,
/// uint64 finalizedAt, bytes32 finalRoot, bytes32 finalBinding)`.
pub fn decode_shard_state(ret: &[u8]) -> Result<u8, String> {
    if ret.len() != 160 {
        return Err(format!(
            "shardState return is {} bytes, want 160",
            ret.len()
        ));
    }
    if ret[..31].iter().any(|b| *b != 0) {
        return Err("shardState status does not fit u8".into());
    }
    Ok(ret[31])
}

/// Status of a (round, shard) the indexer has seen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShardStatus {
    /// Not judged yet (the contract view could not be read).
    Unchecked,
    /// The operator holds no spec for it: known, never served.
    NotHeld,
    /// The contract holds no final root at the indexed block yet (the
    /// challenge window is open, or a dispute runs).
    Pending,
    /// Final, held, and refused.
    Unservable(Refusal),
    /// The contract says no root can become final (IDX-R1-3); not polled
    /// once a read on a later poll agrees (IDX-R3-2).
    Ended(Ended),
    /// In the catalog.
    Servable {
        /// The contract's final root.
        root: Hash32,
        /// Block at which it finalized.
        finalized_at: u64,
    },
}

impl ShardStatus {
    fn code(&self) -> &'static str {
        match self {
            ShardStatus::Unchecked => "unchecked",
            ShardStatus::NotHeld => "not_held",
            ShardStatus::Pending => "pending",
            ShardStatus::Unservable(r) => r.code(),
            ShardStatus::Ended(e) => e.code(),
            ShardStatus::Servable { .. } => "servable",
        }
    }
}

/// The evidence half of `check_servable` for evidence in the operator's
/// store: an attestation must be stored as one, any other kind as a blob
/// (both checked against their hash on read).
fn resolves(cas: &dyn Cas, e: &EvidenceRef) -> bool {
    if e.kind == onyx_labels::ATTESTATION_EVIDENCE {
        cas.has(Kind::Attestation, &e.content_hash)
    } else {
        cas.has(Kind::Blob, &e.content_hash)
    }
}

/// Judge a held shard against the contract's final `root` and `binding`:
/// the stored rows must hash to `root`, their count (and the stated
/// `leafCount`) with the spec must give `binding`, and a pinned label set
/// must pass `LabelSet::check_servable` against the store (a `test.*` set
/// only in dev mode, unchecked there as before). Returns the metadata to
/// serve for `round`.
pub fn check_held(
    held: &HeldShard,
    round: u64,
    root: &Hash32,
    binding: &Hash32,
    cas: &dyn Cas,
    allow_test_labels: bool,
) -> Result<ShardMeta, Refusal> {
    let rows = cas.rows(root).ok_or(Refusal::RowsMissing)?;
    let tree = MerkleTree::from_rows(&rows);
    if tree.root() != *root {
        return Err(Refusal::RowsMissing);
    }
    let n = tree.leaf_count();
    if held.meta.leaf_count.is_some_and(|m| m != n) {
        return Err(Refusal::LeafCountMismatch);
    }
    if held.meta.binding.binding(*root, n) != *binding {
        return Err(Refusal::BindingMismatch);
    }
    check_label_set(held.label_set.as_ref(), cas, allow_test_labels)?;
    let mut meta = held.meta.clone();
    meta.round = round;
    meta.leaf_count = Some(n);
    Ok(meta)
}

/// The label-set half of [`check_held`] (R-LBL-FIX-1): a pinned non-test
/// set must be in the store under its pinned name and version and pass
/// `LabelSet::check_servable` (every label cites evidence the store
/// resolves); a `test.*` set passes only with `allow_test_labels`. onyx-node
/// also runs it at start on every shard its catalog file lists (indexer off),
/// so the listed path cannot serve a set the indexer path would refuse.
pub fn check_label_set(
    label_set: Option<&LabelSetRef>,
    cas: &dyn Cas,
    allow_test_labels: bool,
) -> Result<(), Refusal> {
    let Some(l) = label_set else {
        return Ok(());
    };
    if l.name.starts_with(onyx_labels::TEST_SET_PREFIX) {
        return if allow_test_labels {
            Ok(())
        } else {
            Err(Refusal::TestLabels)
        };
    }
    let set = cas
        .label_set(&l.content_hash)
        .filter(|s| s.name == l.name && s.version == l.version)
        .ok_or(Refusal::LabelSetMissing)?;
    set.check_servable(&|e| resolves(cas, e))
        .map_err(|e| Refusal::EvidenceMissing(e.to_string()))
}

/// What one poll did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PollReport {
    /// The `finalized` block the node reported.
    pub head: u64,
    /// Blocks read (inclusive), if any.
    pub scanned: Option<(u64, u64)>,
    /// `eth_getLogs` calls that answered.
    pub ranges: u64,
    /// Ranges split after an error.
    pub splits: u64,
    /// Logs not seen before.
    pub new_events: usize,
    /// Shards added to the catalog by this poll.
    pub admitted: Vec<(u64, Hash32)>,
    /// Logs read at a (block, log index) that already held a different one
    /// (IDX-R1-1).
    pub conflicts: u64,
    /// Blocks read for the held shards only, because the node would not
    /// answer the whole block (IDX-R1-2).
    pub partial_blocks: Vec<u64>,
    /// Candidates removed from a full position to make room (IDX-R2-1).
    pub evicted: u64,
    /// New logs refused at a full position (IDX-R2-1).
    pub dropped: u64,
    /// Blocks read by the held-shard rescan in this poll (inclusive), if any
    /// (ADR-0033 Amendment 4).
    pub rescanned: Option<(u64, u64)>,
    /// The held-shard rescan reached the block it was started for.
    pub rescan_done: bool,
    /// Shards taken out of the catalog by this poll: a served (round, shard)
    /// whose final view two reads on two polls refuted (IDX-R5-2).
    pub removed: Vec<(u64, Hash32)>,
}

/// One (round, shard) as the indexer knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownShard {
    /// Round.
    pub round: u64,
    /// Shard id.
    pub shard: Hash32,
    /// Status.
    pub status: ShardStatus,
    /// Events seen for it (more than one after a dispute replaced a root).
    pub events: usize,
    /// Events whose root or binding is not the contract's final one.
    pub superseded: usize,
}

/// (block, log index).
type Pos = (u64, u64);
/// What a log says: (round, shard, root, binding). Two logs at one position
/// that say different things are both kept as candidates (IDX-R1-1).
type Says = (u64, Hash32, Hash32, Hash32);
const SAYS_MIN: Says = (0, Hash32([0; 32]), Hash32([0; 32]), Hash32([0; 32]));
const SAYS_MAX: Says = (
    u64::MAX,
    Hash32([0xff; 32]),
    Hash32([0xff; 32]),
    Hash32([0xff; 32]),
);

fn says(e: &FinalizedLog) -> Says {
    (e.round, e.shard, e.root, e.binding)
}

/// A final view: (root, binding, finalizedAt).
type View = (Hash32, Hash32, u64);

/// The judging record of one (round, shard) (IDX-R3): no single answer is
/// terminal. An end counts only once a second read on a later poll also
/// says ended (with no live read between), a final view only once a second
/// read on a later poll names the same one; a read that disagrees starts
/// over from the newer read.
#[derive(Clone, Debug, Default)]
struct Track {
    /// The poll of the first of the reads saying ended since the last live
    /// one, while not confirmed (`None`: confirmed, or not ended).
    ending_since: Option<u64>,
    /// A final view read once and not confirmed yet, with its poll.
    view_seen: Option<(View, u64)>,
    /// Read a confirmed end again: a new log names this (round, shard)
    /// judged never opened (the round may have opened since), or a periodic
    /// rescan started (ADR-0033 Amendment 4). The end stays meanwhile. For
    /// a served (round, shard): read its final view again (IDX-R5-2).
    recheck: bool,
    /// A served (round, shard) whose final view a read refuted, with what
    /// that read said and its poll; a second read on a later poll saying the
    /// same takes it out of the catalog (IDX-R5-2).
    refuted: Option<(Option<View>, u64)>,
}

/// What a scan reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Every `Finalized` log of the contract; moves the cursor.
    All,
    /// The held shards' logs only (their ids as a topic-2 filter); moves the
    /// rescan's cursor (ADR-0033 Amendment 4).
    Rescan,
}

/// A held-shard rescan in progress (ADR-0033 Amendment 4): blocks
/// `next..to` are still to read, `to` being the main cursor when it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rescan {
    next: u64,
    to: u64,
}

#[derive(Default)]
struct Inner {
    next: u64,
    /// The deploy block was checked against the chain (IDX-R1-4) by a start
    /// that wrote this state file (SOAK-4): later starts skip the check, so
    /// a restart does not need state from `deployBlock` again.
    deploy_checked: bool,
    /// The `finalized` head the last poll read (IDX-R5-1: shown, and agreed
    /// with by the next poll; never a judging pin).
    head: Option<u64>,
    /// Every event, by position and what it says (a tx hash is not part of
    /// the key: the same log with or without one counts once).
    events: BTreeMap<(Pos, Says), FinalizedLog>,
    /// How many kept logs name each (round, shard) (IDX-R2-1).
    named: BTreeMap<Key, usize>,
    status: BTreeMap<(u64, Hash32), ShardStatus>,
    /// The contract's final (root, binding, finalizedAt) per key, once two
    /// reads on different polls agree, or once it was admitted (a final root
    /// never changes, so a confirmed one is not read again; IDX-R3-2).
    view: BTreeMap<(u64, Hash32), (Hash32, Hash32, u64)>,
    /// What the judging read per key but has not confirmed yet (IDX-R3).
    track: BTreeMap<Key, Track>,
    /// The judging pin (that poll's `finalized` head) each served (round,
    /// shard) was admitted at (IDX-R6-1): a read of its final view below it
    /// comes from a backend behind that block and refutes nothing.
    admitted_at: BTreeMap<Key, u64>,
    polls: u64,
    last_error: Option<String>,
    write_errors: u64,
    /// Conflicting re-reads since start (IDX-R1-1).
    conflicts: u64,
    last_conflict: Option<String>,
    /// Candidates evicted from, and new logs refused at, a full position
    /// since start (IDX-R2-1).
    evicted: u64,
    dropped: u64,
    /// Blocks read for the held shards only since start (IDX-R1-2).
    partial_blocks: u64,
    /// The held-shard rescan in progress, and those finished since start
    /// (ADR-0033 Amendment 4).
    rescan: Option<Rescan>,
    /// Where a rescan cut short by a restart had read up to (from the state
    /// file), and the hash of the held set it was reading for: the startup
    /// rescan resumes there instead of the deploy block (IDX-R6-2), since
    /// the blocks before it were read for those shards already, but only
    /// when the same shards are held now (M7L3-R1-2: a shard newly held
    /// was never read below the cursor). No hash (an older state file): no
    /// resume.
    resume_rescan: Option<(u64, Option<Hash32>)>,
    rescans: u64,
    /// The poll count at which the next periodic rescan starts (set when a
    /// rescan ends, IDX-R5-4).
    next_rescan_at: u64,
    /// Served shards taken out of the catalog since start (IDX-R5-2).
    removed: u64,
    /// Something to write to the state file.
    dirty: bool,
    /// State-file writes since start.
    state_writes: u64,
    /// `/v1/health`'s `indexer` field, rebuilt after each poll so a request
    /// costs a clone, not a walk over every event (IDX-R1-5).
    health: Value,
}

/// Something that reports the indexer's state on `/v1/health`.
pub trait IndexerHealth: Send + Sync {
    /// JSON for the `indexer` field.
    fn indexer_health(&self) -> Value;
}

/// The indexer. Call [`Indexer::poll`] repeatedly (one at a time; a second
/// concurrent call waits).
pub struct Indexer<T: JsonRpc> {
    cfg: IndexerConfig,
    rpc: T,
    held: Holdings,
    cas: Arc<dyn Cas + Send + Sync>,
    state_path: Option<PathBuf>,
    inner: Mutex<Inner>,
    one: tokio::sync::Mutex<()>,
}

/// (round, shard).
type Key = (u64, Hash32);

fn key(e: &FinalizedLog) -> Key {
    (e.round, e.shard)
}

impl<T: JsonRpc> Indexer<T> {
    /// An indexer for `cfg` reading through `rpc`, judging against `held`
    /// and the rows and evidence in `cas`. With `state_path`, the cursor and
    /// events are kept there: an existing file resumes the scan, and one
    /// written for another chain, contract or deploy block is refused.
    pub fn open(
        cfg: IndexerConfig,
        rpc: T,
        held: Holdings,
        cas: Arc<dyn Cas + Send + Sync>,
        state_path: Option<PathBuf>,
    ) -> Result<Self, String> {
        cfg.validate()?;
        let mut inner = Inner {
            next: cfg.deploy_block,
            ..Inner::default()
        };
        if let Some(p) = &state_path {
            match std::fs::read_to_string(p) {
                Ok(text) => load_state(&text, &cfg, &mut inner)
                    .map_err(|e| format!("indexer state {}: {e}", p.display()))?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("indexer state {}: {e}", p.display())),
            }
        }
        let keys: Vec<Key> = inner.events.values().map(key).collect();
        for k in keys {
            *inner.named.entry(k).or_default() += 1;
            inner.status.entry(k).or_insert(ShardStatus::Unchecked);
        }
        // A restart reads the held shards' logs again from the deploy block
        // (ADR-0033 Amendment 4), so a log a lying node withheld or crowded
        // out before it is found once the node answers honestly. It starts
        // on the [`STARTUP_RESCAN_POLL`]th poll, once every saved (round,
        // shard) has been read on two polls (an end it may evict for is then
        // confirmed); a fresh state file has nothing to read again.
        inner.next_rescan_at = if inner.next > cfg.deploy_block {
            STARTUP_RESCAN_POLL
        } else if cfg.rescan_every > 0 {
            cfg.rescan_every
        } else {
            u64::MAX
        };
        let idx = Indexer {
            cfg,
            rpc,
            held,
            cas,
            state_path,
            inner: Mutex::new(inner),
            one: tokio::sync::Mutex::new(()),
        };
        idx.refresh_health();
        Ok(idx)
    }

    /// Check that the configured deploy block is the block the contract's
    /// code appeared in (IDX-R1-4): no code at `deployBlock - 1`, code at
    /// `deployBlock`. A later block would skip events; an earlier one only
    /// costs reads, but is a sign of the wrong manifest. Needs a node that
    /// keeps state that old (an archive node for an old deployment); a node
    /// that cannot answer is an error, never a pass. Node errors (a 429, a
    /// timeout) are retried with [`DEPLOY_CHECK_BACKOFF_MS`], and the error
    /// names the cause: the node failing, a node head below `deployBlock`
    /// (another network, or still syncing), a node without the old state, or
    /// a real mismatch (IDX-R2-2).
    pub async fn check_deploy_block(&self) -> Result<(), String> {
        let backoff = DEPLOY_CHECK_BACKOFF_MS.map(std::time::Duration::from_millis);
        self.check_deploy_block_with(&backoff).await
    }

    /// [`Indexer::check_deploy_block`] with the waits between attempts.
    pub async fn check_deploy_block_with(
        &self,
        backoff: &[std::time::Duration],
    ) -> Result<(), String> {
        // SOAK-4 (L6 soak): the check reads state at `deployBlock`, which a
        // node that keeps only recent state (anvil past a few thousand
        // blocks, a pruned full node past 128) no longer has, so every
        // restart of a deployment older than that failed, for good. A state
        // file bound to this chain, contract and deploy block (`open`
        // refuses any other) that records a passed check is trusted: the
        // operator's own disk, like the journals. The cheap head check still
        // runs (review NB3): a node on another network or still syncing is
        // refused at every start, recorded check or not.
        if self.deploy_checked() {
            self.check_head_reaches_deploy_block(backoff).await?;
            return Ok(());
        }
        self.check_deploy_block_on_chain(backoff).await?;
        if let Ok(mut i) = self.inner.lock() {
            i.deploy_checked = true;
            i.dirty = true;
        }
        self.save().await;
        Ok(())
    }

    /// Whether this indexer's state records a passed deploy-block check.
    pub fn deploy_checked(&self) -> bool {
        self.inner.lock().map(|i| i.deploy_checked).unwrap_or(false)
    }

    /// The node's head must be at or past `deployBlock` (one
    /// `eth_blockNumber`, no old state): otherwise it is on another network
    /// or still syncing.
    async fn check_head_reaches_deploy_block(
        &self,
        backoff: &[std::time::Duration],
    ) -> Result<(), String> {
        let n = self.cfg.deploy_block;
        let head = match self
            .call_retrying(backoff, "eth_blockNumber", json!([]))
            .await
        {
            Ok(v) => hex_u64(&v, "eth_blockNumber").map_err(|e| format!("deployBlock {n}: {e}"))?,
            Err(f) => return Err(format!("deployBlock {n}: cannot read the node's head: {f}")),
        };
        if head < n {
            return Err(format!(
                "deployBlock {n} is above the node's head {head}: the node is on another network or still syncing (check the RPC URL and the manifest)"
            ));
        }
        Ok(())
    }

    async fn check_deploy_block_on_chain(
        &self,
        backoff: &[std::time::Duration],
    ) -> Result<(), String> {
        let n = self.cfg.deploy_block;
        self.check_head_reaches_deploy_block(backoff).await?;
        let code = |block: u64| async move {
            let params = json!([hx(&self.cfg.contract), format!("0x{block:x}")]);
            let v = self
                .call_retrying(backoff, "eth_getCode", params)
                .await
                .map_err(|f| match f {
                    RpcFailure::StateMissing(e) => format!(
                        "deployBlock {n}: the node does not keep state from block {block} ({e}); this check needs one that does (an archive node for an old deployment)"
                    ),
                    f => format!("deployBlock {n}: eth_getCode at block {block}: {f}"),
                })?;
            hex_bytes(&v, "eth_getCode").map_err(|e| format!("deployBlock {n}: {e}"))
        };
        if code(n).await?.is_empty() {
            return Err(format!(
                "deployBlock {n} is before the deployment: the settlement contract {} has no code at block {n}",
                hx(&self.cfg.contract)
            ));
        }
        if n > 0 && !code(n - 1).await?.is_empty() {
            return Err(format!(
                "deployBlock {n} is after the deployment: the settlement contract {} already has code at block {} (events before {n} would be missed); use the block of its deploy transaction",
                hx(&self.cfg.contract),
                n - 1
            ));
        }
        Ok(())
    }

    /// `method` once, then again after each wait in `backoff` while the node
    /// fails; an answer saying the node lacks old state is not retried.
    async fn call_retrying(
        &self,
        backoff: &[std::time::Duration],
        method: &str,
        params: Value,
    ) -> Result<Value, RpcFailure> {
        let mut waits = backoff.iter();
        let mut tries = 0usize;
        loop {
            tries += 1;
            match self.rpc.call(method, params.clone()).await {
                Ok(v) => return Ok(v),
                Err(e) if state_missing(&e) => return Err(RpcFailure::StateMissing(e)),
                Err(e) => match waits.next() {
                    Some(d) => tokio::time::sleep(*d).await,
                    None => return Err(RpcFailure::Node { tries, last: e }),
                },
            }
        }
    }

    /// The configuration.
    pub fn config(&self) -> &IndexerConfig {
        &self.cfg
    }

    /// The next block the scan reads.
    pub fn next_block(&self) -> u64 {
        self.inner.lock().map(|i| i.next).unwrap_or(0)
    }

    /// Every (round, shard) seen, in order.
    pub fn known(&self) -> Vec<KnownShard> {
        let Ok(i) = self.inner.lock() else {
            return Vec::new();
        };
        let mut out: BTreeMap<(u64, Hash32), KnownShard> = BTreeMap::new();
        for e in i.events.values() {
            let k = key(e);
            let view = i.view.get(&k);
            let entry = out.entry(k).or_insert_with(|| KnownShard {
                round: e.round,
                shard: e.shard,
                status: i.status.get(&k).cloned().unwrap_or(ShardStatus::Unchecked),
                events: 0,
                superseded: 0,
            });
            entry.events += 1;
            if view.is_some_and(|(r, b, _)| (*r, *b) != (e.root, e.binding)) {
                entry.superseded += 1;
            }
        }
        out.into_values().collect()
    }

    /// The events seen, by (block, log index).
    pub fn events(&self) -> Vec<FinalizedLog> {
        self.inner
            .lock()
            .map(|i| i.events.values().cloned().collect())
            .unwrap_or_default()
    }

    /// One pass: check the node's chain, read its `finalized` block, scan
    /// the new range (and the overlap), save the cursor, then judge every
    /// held (round, shard) not yet served or ended and add the servable ones
    /// to `catalog`. The judging runs even when the scan failed (IDX-R1-1),
    /// so shards already known keep being admitted while the node misbehaves.
    /// An error is returned after both; the cursor keeps what was read; call
    /// again later.
    pub async fn poll(&self, catalog: &RwLock<Catalog>) -> Result<PollReport, String> {
        let _one = self.one.lock().await;
        let r = self.poll_inner(catalog).await;
        if let Ok(mut i) = self.inner.lock() {
            i.polls += 1;
            match &r {
                Ok(_) => i.last_error = None,
                Err(e) => i.last_error = Some(e.clone()),
            }
        }
        self.refresh_health();
        r
    }

    async fn poll_inner(&self, catalog: &RwLock<Catalog>) -> Result<PollReport, String> {
        let chain = hex_u64(
            &self.rpc.call("eth_chainId", json!([])).await?,
            "eth_chainId",
        )?;
        if chain != self.cfg.chain_id {
            return Err(format!("node is chain {chain}, want {}", self.cfg.chain_id));
        }
        let b = self
            .rpc
            .call("eth_getBlockByNumber", json!(["finalized", false]))
            .await?;
        if b.is_null() {
            return Err("node has no finalized block".into());
        }
        let reported = hex_u64(&b["number"], "finalized block number")?;
        let mut report = PollReport {
            head: reported,
            ..PollReport::default()
        };
        let (next, prev) = {
            let mut i = self.inner.lock().map_err(|_| "indexer lock")?;
            let prev = i.head.replace(reported);
            if i.polls >= i.next_rescan_at
                && i.rescan.is_none()
                && !self.held.is_empty()
                && i.next > self.cfg.deploy_block
            {
                // The next one is due once this one ended (IDX-R5-4).
                i.next_rescan_at = u64::MAX;
                // Resumed only for the held set it was reading for
                // (M7L3-R1-2); any other set reads again from deployBlock.
                let held = self.held.ids_hash();
                let from = match i.resume_rescan.take() {
                    Some((next, Some(h))) if h == held => next,
                    _ => self.cfg.deploy_block,
                };
                start_rescan(&mut i, from);
            }
            (i.next, prev)
        };
        // Judge at this answer's head, never a remembered one: one lying
        // `finalized` answer far ahead (a block no honest node has) fails
        // this poll's judging only (IDX-R5-1).
        let pin = reported;
        // Read only up to what this answer called finalized: a lagging
        // backend's older head scans less, never further. And at most
        // `max_jump` blocks past the head this answer and the previous poll's
        // both reached (or past the cursor): one lying answer far ahead moves
        // the cursor that far at most, and the overlap reads those blocks
        // again (IDX-R5-5).
        let agreed = prev.map_or(0, |p| p.min(reported));
        let to = reported.min(agreed.max(next.saturating_sub(1).saturating_add(self.cfg.max_jump)));
        let from = if next > self.cfg.deploy_block {
            next.saturating_sub(self.cfg.overlap)
                .max(self.cfg.deploy_block)
        } else {
            self.cfg.deploy_block
        };
        // Logs not judged yet (after a restart, or a judging the node
        // failed) are judged before the scan, so a full position can tell a
        // refuted candidate from a live one (IDX-R2-1).
        let unchecked = self
            .inner
            .lock()
            .map_err(|_| "indexer lock")?
            .status
            .values()
            .any(|s| *s == ShardStatus::Unchecked);
        let prejudged = if unchecked {
            self.judge(catalog, pin, &mut report).await
        } else {
            Ok(())
        };
        let scanned = if from <= to {
            self.scan(from, to, &mut report, Mode::All).await
        } else {
            Ok(())
        };
        let rescanned = self.rescan_step(to, &mut report).await;
        let judged = self.judge(catalog, pin, &mut report).await;
        scanned?;
        rescanned?;
        prejudged?;
        judged?;
        Ok(report)
    }

    /// Continue the held-shard rescan, if one runs, up to `to` (this poll's
    /// finalized head) at most (ADR-0033 Amendment 4). A failure leaves its
    /// cursor where it stood; the next poll continues.
    async fn rescan_step(&self, to: u64, report: &mut PollReport) -> Result<(), String> {
        let Some(r) = self.inner.lock().map_err(|_| "indexer lock")?.rescan else {
            return Ok(());
        };
        let end = r.to.saturating_sub(1).min(to);
        let res = if r.next <= end {
            self.scan(r.next, end, report, Mode::Rescan).await
        } else {
            Ok(())
        };
        let mut i = self.inner.lock().map_err(|_| "indexer lock")?;
        if i.rescan.is_some_and(|r| r.next >= r.to) {
            i.rescan = None;
            i.dirty = true;
            i.rescans += 1;
            report.rescan_done = true;
            // Counted from the end, so rescans never run back to back
            // however long one takes (IDX-R5-4).
            i.next_rescan_at = match self.cfg.rescan_every {
                0 => u64::MAX,
                every => i.polls.saturating_add(every),
            };
        }
        res
    }

    /// Scan `from..=to`, then write the state file once (and every
    /// [`SAVE_EVERY_RANGES`] ranges on the way), whether or not a range failed.
    async fn scan(
        &self,
        from: u64,
        to: u64,
        report: &mut PollReport,
        mode: Mode,
    ) -> Result<(), String> {
        let r = self.scan_ranges(from, to, report, mode).await;
        self.save().await;
        r
    }

    async fn scan_ranges(
        &self,
        from: u64,
        to: u64,
        report: &mut PollReport,
        mode: Mode,
    ) -> Result<(), String> {
        let mut a = from;
        let mut span = self.cfg.span;
        let mut calls = 0u64;
        let mut answered = 0u64;
        // The rescan has its own, smaller budget, so the scan of new blocks
        // never waits on it (IDX-R5-4); both continue on the next poll.
        let budget = match mode {
            Mode::All => MAX_RANGES_PER_POLL,
            Mode::Rescan => self.cfg.rescan_budget,
        };
        // A held-shard read costs one call per [`HELD_TOPICS_PER_CALL`] ids.
        let cost = match mode {
            Mode::All => 1,
            Mode::Rescan => u64::try_from(self.held.len().div_ceil(HELD_TOPICS_PER_CALL).max(1))
                .unwrap_or(u64::MAX),
        };
        while a <= to {
            if calls >= budget {
                break;
            }
            calls = calls.saturating_add(cost);
            let b = to.min(a.saturating_add(span - 1));
            let got = match mode {
                Mode::All => self.get_logs(a, b).await,
                Mode::Rescan => self.held_logs(a, b).await,
            };
            let logs = match got {
                Ok(logs) => logs,
                // Split on any error (a span cap, an answer too large, a
                // backend that lost the range): halves down to one block.
                Err(_) if b > a => {
                    report.splits += 1;
                    span = (b - a).div_ceil(2).max(1);
                    continue;
                }
                Err(e) if mode == Mode::Rescan => {
                    return Err(format!("held-shard rescan, eth_getLogs {a}..={b}: {e}"))
                }
                // One block the node will not answer whole (more logs than
                // an answer may carry, IDX-R1-2): read it for the held
                // shards only, which is all this operator can serve. The
                // overlap reads it whole again next poll.
                Err(e) => match self.get_held_logs(a).await {
                    Ok(logs) => {
                        report.partial_blocks.push(a);
                        if let Ok(mut i) = self.inner.lock() {
                            i.partial_blocks += 1;
                        }
                        logs
                    }
                    Err(e2) => {
                        return Err(format!(
                            "eth_getLogs {a}..={b}: {e}; for the held shards only: {e2}"
                        ))
                    }
                },
            };
            answered += 1;
            {
                let mut i = self.inner.lock().map_err(|_| "indexer lock")?;
                match mode {
                    Mode::All => {
                        report.ranges += 1;
                        for l in logs {
                            add_log(&mut i, &self.held, l, None, report);
                        }
                        if b + 1 > i.next {
                            i.next = b + 1;
                            i.dirty = true;
                        }
                        report.scanned = Some((report.scanned.map_or(a, |(s, _)| s), b));
                    }
                    Mode::Rescan => {
                        // What this answer returned per position: at a full
                        // position, a held log it did not return may make
                        // room for one it did (ADR-0033 Amendment 4).
                        let mut returned: BTreeMap<Pos, Vec<Says>> = BTreeMap::new();
                        for l in &logs {
                            returned
                                .entry((l.block, l.log_index))
                                .or_default()
                                .push(says(l));
                        }
                        for l in logs {
                            if self.held.get(&l.shard).is_none() {
                                continue;
                            }
                            let r = returned
                                .get(&(l.block, l.log_index))
                                .cloned()
                                .unwrap_or_default();
                            add_log(&mut i, &self.held, l, Some(&r), report);
                        }
                        if let Some(r) = i.rescan.as_mut() {
                            r.next = r.next.max(b + 1);
                            // Saved, so a restart resumes here (IDX-R6-2).
                            i.dirty = true;
                        }
                        report.rescanned = Some((report.rescanned.map_or(a, |(s, _)| s), b));
                    }
                }
            }
            // A span that had to be split stays split for the rest of this
            // poll (the node's cap does not move); the next poll starts again
            // from the configured span.
            a = b + 1;
            if answered.is_multiple_of(SAVE_EVERY_RANGES) {
                self.save().await;
            }
        }
        Ok(())
    }

    async fn get_logs(&self, a: u64, b: u64) -> Result<Vec<FinalizedLog>, String> {
        self.logs_matching(a, b, json!([FINALIZED_TOPIC])).await
    }

    async fn logs_matching(
        &self,
        a: u64,
        b: u64,
        topics: Value,
    ) -> Result<Vec<FinalizedLog>, String> {
        let v = self
            .rpc
            .call(
                "eth_getLogs",
                json!([{
                    "address": hx(&self.cfg.contract),
                    "fromBlock": format!("0x{a:x}"),
                    "toBlock": format!("0x{b:x}"),
                    "topics": topics,
                }]),
            )
            .await?;
        let arr = v.as_array().ok_or("eth_getLogs answer is not a list")?;
        if arr.len() > MAX_LOGS_PER_RANGE {
            return Err(format!("{} logs in one range", arr.len()));
        }
        arr.iter()
            .map(|l| parse_log(l, &self.cfg.contract, a, b))
            .collect()
    }

    /// Block `a`'s logs for the held shards only (IDX-R1-2): the block's
    /// header first (a node that is down fails here, so a dead node never
    /// moves the cursor), then the held shard ids as a topic-2 filter, at
    /// most [`HELD_TOPICS_PER_CALL`] per call. A shard id finalizes at most
    /// once, so each answer is small. The round (topic 1) is indexed too, but
    /// the rounds in a block are not known before it is read, so it cannot
    /// bound the answer.
    async fn get_held_logs(&self, a: u64) -> Result<Vec<FinalizedLog>, String> {
        let h = self
            .rpc
            .call("eth_getBlockByNumber", json!([format!("0x{a:x}"), false]))
            .await?;
        if h.is_null() || hex_u64(&h["number"], "block number")? != a {
            return Err(format!("node has no block {a}"));
        }
        self.held_logs(a, a).await
    }

    /// Blocks `a..=b`'s logs for the held shards only: their ids as a topic-2
    /// filter, at most [`HELD_TOPICS_PER_CALL`] per call.
    async fn held_logs(&self, a: u64, b: u64) -> Result<Vec<FinalizedLog>, String> {
        let ids: Vec<String> = self.held.ids().map(|s| s.to_string()).collect();
        let mut out = Vec::new();
        for chunk in ids.chunks(HELD_TOPICS_PER_CALL) {
            out.extend(
                self.logs_matching(a, b, json!([FINALIZED_TOPIC, null, chunk]))
                    .await?,
            );
        }
        Ok(out)
    }

    /// A judging `eth_call` at block `pin`. A failed call is sent once more
    /// at once (on a load-balanced node, likely to another backend), so one
    /// failing answer does not leave a key unjudged for a poll (IDX-R3-1).
    async fn eth_call(&self, data: Vec<u8>, pin: u64) -> Result<Vec<u8>, String> {
        let params = json!([
            {"to": hx(&self.cfg.contract), "data": hx(&data)},
            format!("0x{pin:x}")
        ]);
        let ret = match self.rpc.call("eth_call", params.clone()).await {
            Ok(v) => v,
            Err(_) => self.rpc.call("eth_call", params).await?,
        };
        hex_bytes(&ret, "eth_call return")
    }

    /// The contract's final (root, binding, finalizedAt) for `k` at block
    /// `pin`. A confirmed view is not read again; one read is used for this
    /// poll's judging but cached only when a read on a later poll agrees, so
    /// one lying answer is never kept (IDX-R3-2). A read that disagrees (or
    /// says not final) starts over.
    async fn view(&self, k: Key, pin: u64) -> Result<Option<View>, String> {
        if let Some(v) = self.inner.lock().ok().and_then(|i| i.view.get(&k).copied()) {
            return Ok(Some(v));
        }
        let data = self
            .eth_call(get_finalized_root_calldata(k.0, &k.1), pin)
            .await?;
        let v = decode_finalized_root(&data).map_err(|e| e.to_string())?;
        if let Ok(mut i) = self.inner.lock() {
            let now = i.polls;
            let t = i.track.entry(k).or_default();
            let confirmed = match (v, t.view_seen) {
                (Some(v), Some((w, p))) if v == w && p < now => {
                    t.view_seen = None;
                    true
                }
                (Some(v), Some((w, _))) if v == w => false,
                (Some(v), _) => {
                    t.view_seen = Some((v, now));
                    false
                }
                (None, _) => {
                    t.view_seen = None;
                    false
                }
            };
            if let (true, Some(v)) = (confirmed, v) {
                i.view.insert(k, v);
            }
        }
        Ok(v)
    }

    /// How `k` ended at block `pin`, if no root can become final any more
    /// (IDX-R1-3).
    async fn ended(&self, k: (u64, Hash32), pin: u64) -> Result<Option<Ended>, String> {
        let data = self.eth_call(shard_state_calldata(k.0, &k.1), pin).await?;
        Ended::from_status(decode_shard_state(&data)?)
    }

    async fn judge(
        &self,
        catalog: &RwLock<Catalog>,
        pin: u64,
        report: &mut PollReport,
    ) -> Result<(), String> {
        let todo: Vec<Key> = {
            let i = self.inner.lock().map_err(|_| "indexer lock")?;
            // A (round, shard) all of whose kept logs lie above this poll's
            // head (an older head from a lagging backend) waits for a later
            // poll: read at a block before its event, the contract would
            // not know it yet (IDX-R5-1).
            let mut first: BTreeMap<Key, u64> = BTreeMap::new();
            for e in i.events.values() {
                let b = first.entry(key(e)).or_insert(e.block);
                *b = (*b).min(e.block);
            }
            i.named
                .keys()
                .filter(|k| first.get(k).is_some_and(|b| *b <= pin))
                .filter(|k| match i.status.get(k) {
                    Some(ShardStatus::NotHeld) => false,
                    // A served one is read again when a periodic rescan
                    // starts (IDX-R5-2).
                    Some(ShardStatus::Servable { .. }) => i.track.get(k).is_some_and(|t| t.recheck),
                    // An end is final only once confirmed (IDX-R3-2).
                    Some(ShardStatus::Ended(_)) => {
                        !ended_confirmed(&i, k) || i.track.get(k).is_some_and(|t| t.recheck)
                    }
                    _ => true,
                })
                .copied()
                .collect()
        };
        let mut first_err = None;
        for k in todo {
            let served = self
                .inner
                .lock()
                .map_err(|_| "indexer lock")?
                .status
                .get(&k)
                .is_some_and(|s| matches!(s, ShardStatus::Servable { .. }));
            if served {
                if let Err(e) = self.recheck_served(catalog, k, pin, report).await {
                    first_err.get_or_insert(e);
                }
                continue;
            }
            let status = match self.judge_one(catalog, k, pin).await {
                Ok(s) => s,
                Err(e) => {
                    first_err.get_or_insert(e);
                    continue;
                }
            };
            if let Ok(mut i) = self.inner.lock() {
                record(&mut i, k, status.clone());
            }
            if matches!(status, ShardStatus::Servable { .. }) {
                report.admitted.push(k);
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Read a served (round, shard)'s final view again (IDX-R5-2). The view
    /// it was admitted on holds: it stays. A read naming another view (or
    /// none) is a refutation; a second read on a later poll naming the same
    /// takes it out of the catalog and judges it again from the start, as
    /// two agreeing reads admit it. A read that disagrees starts over.
    async fn recheck_served(
        &self,
        catalog: &RwLock<Catalog>,
        k: Key,
        pin: u64,
        report: &mut PollReport,
    ) -> Result<(), String> {
        // IDX-R6-1: a backend whose head is below the block this shard was
        // admitted at (a lagging member of a load-balanced node) may not
        // know the root is final yet; its read is no refutation. The
        // recheck stays due, so a later poll at or above that block reads it.
        let below = self
            .inner
            .lock()
            .map_err(|_| "indexer lock")?
            .admitted_at
            .get(&k)
            .is_some_and(|a| pin < *a);
        if below {
            return Ok(());
        }
        let data = self
            .eth_call(get_finalized_root_calldata(k.0, &k.1), pin)
            .await?;
        let v = decode_finalized_root(&data).map_err(|e| e.to_string())?;
        let mut i = self.inner.lock().map_err(|_| "indexer lock")?;
        let now = i.polls;
        let admitted = i.view.get(&k).map(|(r, b, _)| (*r, *b));
        let holds = v.is_some_and(|(r, b, _)| Some((r, b)) == admitted);
        let t = i.track.entry(k).or_default();
        if holds {
            t.recheck = false;
            t.refuted = None;
            return Ok(());
        }
        match t.refuted {
            Some((w, p)) if w == v && p < now => {}
            Some((w, _)) if w == v => return Ok(()),
            _ => {
                t.refuted = Some((v, now));
                return Ok(());
            }
        }
        *t = Track::default();
        i.view.remove(&k);
        i.admitted_at.remove(&k);
        i.status.insert(k, ShardStatus::Unchecked);
        i.removed += 1;
        drop(i);
        catalog
            .write()
            .map_err(|_| "catalog lock")?
            .remove_shard(k.0, &k.1);
        report.removed.push(k);
        Ok(())
    }

    async fn judge_one(
        &self,
        catalog: &RwLock<Catalog>,
        (round, shard): (u64, Hash32),
        pin: u64,
    ) -> Result<ShardStatus, String> {
        let Some(held) = self.held.get(&shard) else {
            return Ok(ShardStatus::NotHeld);
        };
        let Some((root, binding, at)) = self.view((round, shard), pin).await? else {
            // Not final: still in its window or a case, or ended for good.
            return Ok(match self.ended((round, shard), pin).await? {
                Some(e) => ShardStatus::Ended(e),
                None => ShardStatus::Pending,
            });
        };
        // No event has to name this root: a kept log is how the (round,
        // shard) was found, and the contract's view is the authority on its
        // root (the same view `ChainRoots` reads per request). Requiring an
        // event's root let forged logs at the honest log's position keep a
        // final shard out for good (IDX-R2-1).
        let mut meta = match check_held(
            held,
            round,
            &root,
            &binding,
            self.cas.as_ref(),
            self.cfg.allow_test_labels,
        ) {
            Ok(m) => m,
            Err(r) => return Ok(ShardStatus::Unservable(r)),
        };
        // Listed only on a view two reads on different polls agree on, or
        // with `shardState` saying Finalized: one lying view never lists a
        // round the contract did not finalize, so forged rounds of a held
        // shard cannot crowd a metric's candidates (IDX-R4-2). `ChainRoots`
        // reads the root again on every request either way.
        let confirmed = self
            .inner
            .lock()
            .map_err(|_| "indexer lock")?
            .view
            .contains_key(&(round, shard));
        if !confirmed {
            let data = self
                .eth_call(shard_state_calldata(round, &shard), pin)
                .await?;
            if decode_shard_state(&data)? != STATUS_FINALIZED {
                return Ok(ShardStatus::Pending);
            }
        }
        if let Some(registry) = self.cfg.registry {
            let level = crate::levels::registry_level(
                &self.rpc,
                registry,
                round,
                &shard,
                &meta.metric,
                &meta.binding.recipe_id,
                &meta.binding.label_set_hash,
            )
            .await?
            .unwrap_or(ProofLevel::TrackRecord);
            // L16: the attested level stands in for a label level the
            // catalog file cannot give; otherwise it only weakens.
            meta = meta.with_attested_level(level);
        }
        let mut c = catalog.write().map_err(|_| "catalog lock")?;
        if c.observations(&meta.metric).is_some() {
            return Ok(ShardStatus::Unservable(Refusal::MetricConflict));
        }
        c.add_shard(meta);
        drop(c);
        // Admitted on this view: keep it (the shard is not judged again),
        // with the pin it was admitted at (IDX-R6-1).
        if let Ok(mut i) = self.inner.lock() {
            i.view.insert((round, shard), (root, binding, at));
            i.admitted_at.insert((round, shard), pin);
        }
        Ok(ShardStatus::Servable {
            root,
            finalized_at: at,
        })
    }

    /// Write the cursor and events when they changed (temp file, fsync,
    /// rename, fsync of the directory) on a blocking thread, so the async
    /// runtime never waits on a disk (IDX-R1-5). A failure is counted and
    /// shown on `/v1/health`; the scan goes on in memory, and a restart
    /// reads again from the last saved cursor (safe: the scan is idempotent).
    async fn save(&self) {
        let Some(path) = self.state_path.clone() else {
            return;
        };
        let body = {
            let Ok(mut i) = self.inner.lock() else {
                return;
            };
            if !i.dirty {
                return;
            }
            i.dirty = false;
            state_json(&self.cfg, &i, self.held.ids_hash())
        };
        let r = tokio::task::spawn_blocking(move || write_atomic(&path, &body))
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))
            .and_then(|r| r);
        if let Ok(mut i) = self.inner.lock() {
            i.state_writes += 1;
        }
        if let Err(e) = r {
            if let Ok(mut i) = self.inner.lock() {
                i.write_errors += 1;
                i.dirty = true;
                i.last_error = Some(format!("indexer state write: {e}"));
            }
        }
    }

    /// Rebuild the cached `/v1/health` field (after each poll and at open).
    fn refresh_health(&self) {
        let known = self.known();
        let Ok(mut i) = self.inner.lock() else {
            return;
        };
        i.health = health_json(&self.cfg, &i, &known);
    }
}

/// Whether `k` ended and a second read on a later poll agreed (IDX-R3-2).
fn ended_confirmed(i: &Inner, k: &Key) -> bool {
    matches!(i.status.get(k), Some(ShardStatus::Ended(_)))
        && i.track.get(k).is_some_and(|t| t.ending_since.is_none())
}

/// Store a judged status for `k`. An end is confirmed only when a read on a
/// later poll also says ended, with no live read between (the latest end is
/// shown); a live read starts over. So one lying `shardState` answer never
/// ends a shard (IDX-R3-2).
fn record(i: &mut Inner, k: Key, status: ShardStatus) {
    let now = i.polls;
    let confirmed = ended_confirmed(i, &k);
    let t = i.track.entry(k).or_default();
    t.recheck = false;
    t.ending_since = match (&status, t.ending_since) {
        (ShardStatus::Ended(_), _) if confirmed => None,
        (ShardStatus::Ended(_), Some(p)) if p < now => None,
        (ShardStatus::Ended(_), Some(p)) => Some(p),
        (ShardStatus::Ended(_), None) => Some(now),
        _ => None,
    };
    i.status.insert(k, status);
}

/// Start a held-shard rescan from `deploy` (or, after a restart, from where
/// the saved one had read up to: IDX-R6-2) to the cursor (ADR-0033
/// Amendment 4) and read again, on this poll's judging, every confirmed end,
/// every cached view of a (round, shard) not served, and every served one's
/// final view (IDX-R5-2): so a lie confirmed by two answers holds only until
/// the next rescan, also without a restart.
fn start_rescan(i: &mut Inner, deploy: u64) {
    i.rescan = Some(Rescan {
        next: deploy,
        to: i.next,
    });
    let again: Vec<Key> = i
        .status
        .iter()
        .filter(|(k, s)| ended_confirmed(i, k) || matches!(s, ShardStatus::Servable { .. }))
        .map(|(k, _)| *k)
        .collect();
    for k in again {
        i.track.entry(k).or_default().recheck = true;
    }
    let status = &i.status;
    i.view
        .retain(|k, _| matches!(status.get(k), Some(ShardStatus::Servable { .. })));
}

/// The candidate at a full position to evict for a newcomer naming a new held
/// (round, shard), if any may go (ADR-0033 Amendment 4, guarantee 1). In
/// order:
/// 0. a log whose shard is not held;
/// 1. a log whose (round, shard) another kept log also names, a root the
///    contract's confirmed final view contradicts first, then 2. any such;
/// 3. only for a newcomer the held-shard rescan read (`returned`: what that
///    answer returned at this position), a log it did not return whose
///    (round, shard) ended, confirmed by reads on two polls.
///
/// Never, on a scan's read, the last log naming a held (round, shard),
/// whatever the chain status reads said (an end, confirmed or not) or
/// however its judging failed: an end read through a lying node never costs
/// a held (round, shard) its last log (IDX-R4-1). Rule 3 lets an honest
/// rescan put back a log that forged logs of held shards in rounds never
/// opened crowded out; evicting a live log takes a rescan answer leaving
/// it out and two agreeing ends, and the next honest rescan puts it back.
fn victim(i: &Inner, held: &Holdings, there: &[Says], returned: Option<&[Says]>) -> Option<Says> {
    there
        .iter()
        .filter_map(|s| {
            let k = (s.0, s.1);
            let rank = if held.get(&k.1).is_none()
                || matches!(i.status.get(&k), Some(ShardStatus::NotHeld))
            {
                0u8
            } else if i.named.get(&k).copied().unwrap_or(0) >= 2 {
                let contradicted = i
                    .view
                    .get(&k)
                    .is_some_and(|(r, b, _)| (*r, *b) != (s.2, s.3));
                if contradicted {
                    1
                } else {
                    2
                }
            } else if returned.is_some_and(|r| !r.contains(s)) && ended_confirmed(i, &k) {
                3
            } else {
                return None;
            };
            Some((rank, *s))
        })
        .min()
        .map(|(_, s)| s)
}

/// Remove the candidate `s` at `pos`; when no kept log names its (round,
/// shard) any more, forget that key's status and view too.
fn evict(i: &mut Inner, pos: Pos, s: Says) {
    i.events.remove(&(pos, s));
    let k = (s.0, s.1);
    let left = match i.named.get_mut(&k) {
        Some(n) => {
            *n = n.saturating_sub(1);
            *n
        }
        None => 0,
    };
    if left == 0 {
        i.named.remove(&k);
        i.status.remove(&k);
        i.view.remove(&k);
        i.track.remove(&k);
        i.admitted_at.remove(&k);
    }
}

/// Add a log the node answered. The same log again (with or without its tx
/// hash) is ignored. A log at a (block, log index) that already holds a
/// different one is a conflict: a lying or broken backend, or a changed
/// history at a block the node called finalized. It is counted and kept as
/// another candidate, and the contract view decides: an event is a hint,
/// never trusted alone (IDX-R1-1). A shard already served stays served (its
/// root is final); a new event for it is listed.
///
/// A position holds at most [`MAX_CANDIDATES_PER_LOG`] (IDX-R2-1). At a full
/// position a newcomer is refused (counted) when its (round, shard) is
/// already named by a kept log or is not held: it adds nothing the judging
/// needs. Otherwise it takes the slot of a [`victim`] (a log of a shard not
/// held, or one naming a (round, shard) another kept log names; for a log
/// the held-shard rescan read, also a log that rescan did not return), or is
/// refused. So once a log naming a held (round, shard) is kept, no chain
/// status read evicts it (ADR-0033 Amendment 4, guarantee 1); a lying node
/// can only crowd a position before the honest log is read, which the next
/// honest held-shard rescan undoes (guarantee 2).
///
/// A refused newcomer changes nothing (IDX-R3-1: it used to reopen its
/// (round, shard) first). An accepted one naming a (round, shard) judged
/// never opened is read again (the round may have opened since).
fn add_log(
    i: &mut Inner,
    held: &Holdings,
    l: FinalizedLog,
    returned: Option<&[Says]>,
    report: &mut PollReport,
) {
    let pos = (l.block, l.log_index);
    let s = says(&l);
    if i.events.contains_key(&(pos, s)) {
        return;
    }
    let k = key(&l);
    let there: Vec<Says> = i
        .events
        .range((pos, SAYS_MIN)..=(pos, SAYS_MAX))
        .map(|((_, s), _)| *s)
        .collect();
    if !there.is_empty() {
        i.conflicts += 1;
        report.conflicts += 1;
        i.last_conflict = Some(format!(
            "block {} log {}: {} different logs read; the contract view decides",
            l.block,
            l.log_index,
            there.len() + 1
        ));
    }
    if there.len() >= MAX_CANDIDATES_PER_LOG {
        let named = i.named.get(&k).copied().unwrap_or(0) > 0;
        let out = if named || held.get(&k.1).is_none() {
            None
        } else {
            victim(i, held, &there, returned)
        };
        match out {
            Some(v) => {
                evict(i, pos, v);
                i.evicted += 1;
                report.evicted += 1;
            }
            None => {
                i.dropped += 1;
                report.dropped += 1;
                return;
            }
        }
    }
    // Only now, with the log kept: a (round, shard) the contract had not
    // opened may open in a later round's future, so a new event for it is
    // read again. Its end and its eviction rank stay until a read says
    // otherwise, so renaming it never protects its slots (IDX-R3-1). The
    // other ends are final (the shard is closed).
    if matches!(
        i.status.get(&k),
        Some(ShardStatus::Ended(Ended::NotOnChain))
    ) {
        i.track.entry(k).or_default().recheck = true;
    }
    i.status.entry(k).or_insert(ShardStatus::Unchecked);
    *i.named.entry(k).or_default() += 1;
    i.events.insert((pos, s), l);
    i.dirty = true;
    report.new_events += 1;
}

fn state_json(cfg: &IndexerConfig, i: &Inner, held: Hash32) -> Value {
    let events: Vec<Value> = i
        .events
        .values()
        .map(|e| {
            json!({
                "round": e.round,
                "shard": e.shard.to_string(),
                "root": e.root.to_string(),
                "binding": e.binding.to_string(),
                "block": e.block,
                "logIndex": e.log_index,
                "tx": e.tx,
            })
        })
        .collect();
    json!({
        "onyxIndexer": STATE_VERSION,
        "chainId": cfg.chain_id,
        "contract": hx(&cfg.contract),
        "deployBlock": cfg.deploy_block,
        "deployChecked": i.deploy_checked,
        "next": i.next,
        // A rescan in progress (IDX-R6-2): a restart holding the same
        // shards resumes it (M7L3-R1-2).
        "rescan": i.rescan.map(|r| json!({ "next": r.next, "to": r.to, "held": held.to_string() })),
        "events": events,
    })
}

pub(crate) fn write_atomic(path: &std::path::Path, body: &Value) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(body).map_err(std::io::Error::other)?)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

fn load_state(text: &str, cfg: &IndexerConfig, inner: &mut Inner) -> Result<(), String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if v["onyxIndexer"] != json!(STATE_VERSION) {
        return Err(format!("not a version {STATE_VERSION} indexer state"));
    }
    let contract = hex_bytes(&v["contract"], "contract")?;
    if v["chainId"] != json!(cfg.chain_id)
        || contract != cfg.contract
        || v["deployBlock"] != json!(cfg.deploy_block)
    {
        return Err(
            "written for another chain, contract or deploy block (use a new state file)".into(),
        );
    }
    let next = v["next"].as_u64().ok_or("next must be a number")?;
    if next < cfg.deploy_block {
        return Err("next is before the deploy block".into());
    }
    let mut events = BTreeMap::new();
    for e in v["events"].as_array().ok_or("events must be a list")? {
        let n = |f: &str| {
            e[f].as_u64()
                .ok_or_else(|| format!("event {f} must be a number"))
        };
        let s = |f: &str| {
            e[f].as_str()
                .ok_or_else(|| format!("event {f} must be a string"))
        };
        let l = FinalizedLog {
            round: n("round")?,
            shard: h32(s("shard")?, "event shard")?,
            root: h32(s("root")?, "event root")?,
            binding: h32(s("binding")?, "event binding")?,
            block: n("block")?,
            log_index: n("logIndex")?,
            tx: e["tx"].as_str().map(str::to_string),
        };
        if l.block < cfg.deploy_block || l.block >= next {
            return Err(format!(
                "event at block {} outside the scanned range",
                l.block
            ));
        }
        let pos = (l.block, l.log_index);
        if events.range((pos, SAYS_MIN)..=(pos, SAYS_MAX)).count() >= MAX_CANDIDATES_PER_LOG {
            return Err(format!(
                "more than {MAX_CANDIDATES_PER_LOG} events at block {} log {}",
                l.block, l.log_index
            ));
        }
        if events.insert((pos, says(&l)), l).is_some() {
            return Err("an event is listed twice".into());
        }
    }
    // A rescan cut short (IDX-R6-2): `deployBlock <= next <= to <= cursor`.
    let resume = match &v["rescan"] {
        Value::Null => None,
        r => {
            let (rn, rt) = (
                r["next"].as_u64().ok_or("rescan.next must be a number")?,
                r["to"].as_u64().ok_or("rescan.to must be a number")?,
            );
            if rn < cfg.deploy_block || rn > rt || rt > next {
                return Err("rescan is outside the scanned range".into());
            }
            let held = match &r["held"] {
                Value::Null => None,
                h => Some(h32(
                    h.as_str().ok_or("rescan.held must be a string")?,
                    "rescan.held",
                )?),
            };
            Some((rn, held))
        }
    };
    inner.next = next;
    inner.events = events;
    inner.resume_rescan = resume;
    // Absent in files written before SOAK-4: checked again.
    inner.deploy_checked = v["deployChecked"].as_bool().unwrap_or(false);
    Ok(())
}

fn health_json(cfg: &IndexerConfig, i: &Inner, known: &[KnownShard]) -> Value {
    let count = |c: &str| known.iter().filter(|k| k.status.code() == c).count();
    let servable = count("servable");
    let pending = count("pending") + count("unchecked");
    let not_held = count("not_held");
    let ended = known
        .iter()
        .filter(|k| matches!(k.status, ShardStatus::Ended(_)))
        .count();
    let conflict_positions = {
        let mut n = 0usize;
        let mut last = None;
        for (pos, _) in i.events.keys() {
            if last == Some(*pos) {
                n += 1;
            }
            last = Some(*pos);
        }
        n
    };
    let mut h = json!({
        "contract": hx(&cfg.contract),
        "deployBlock": cfg.deploy_block,
        "nextBlock": i.next,
        "finalizedHead": i.head.unwrap_or(0),
        "polls": i.polls,
        "events": i.events.len(),
        "servable": servable,
        "pending": pending,
        "notHeld": not_held,
        "ended": ended,
        "unservable": known.len() - servable - pending - not_held - ended,
        "conflicts": i.conflicts,
        "conflictingPositions": conflict_positions,
        "candidatesEvicted": i.evicted,
        "candidatesDropped": i.dropped,
        "partialBlocks": i.partial_blocks,
        "rescans": i.rescans,
        "rescanning": i.rescan.is_some(),
        "removed": i.removed,
        "stateWrites": i.state_writes,
        "stateWriteErrors": i.write_errors,
        "detail": cfg.health_detail,
    });
    if cfg.health_detail {
        h["lastError"] = json!(i.last_error);
        h["lastConflict"] = json!(i.last_conflict);
        h["shards"] = json!(known
            .iter()
            .take(HEALTH_LIST)
            .map(|k| json!({
                "round": k.round,
                "shard": k.shard.to_string(),
                "status": k.status.code(),
                "events": k.events,
                "superseded": k.superseded,
            }))
            .collect::<Vec<_>>());
    } else {
        // Counts only: whether the last poll failed, not why (the node's
        // log has the text).
        h["failing"] = json!(i.last_error.is_some());
    }
    h
}

impl<T: JsonRpc> IndexerHealth for Indexer<T> {
    fn indexer_health(&self) -> Value {
        self.inner
            .lock()
            .map(|i| i.health.clone())
            .unwrap_or_else(|_| json!({"lastError": "indexer lock", "failing": true}))
    }
}

#[cfg(test)]
#[path = "indexer_tests.rs"]
mod tests;
