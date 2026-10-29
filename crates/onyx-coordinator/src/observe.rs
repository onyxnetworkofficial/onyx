//! Proof level 2, "re-observed" (ADR-0027, docs/spec/proof-ladder.md).
//!
//! Assigned bonded observers read one public source in one window with the
//! feed's pinned fetcher, post an address-bound commit, then reveal. A value is
//! settled only when a strict majority of ALL assigned observers agrees, so
//! observers who stay silent can never let a minority decide. Off-chain
//! prototype: the clock is unix seconds; the on-chain port maps it to block
//! time (open question in the spec).

use crate::sealed::{PostedSealed, SealedSnapshot};
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::claim::MetricId;
use onyx_core::{
    from_canonical_bytes, keccak256, observation_commit, Answer, CanonicalError, Claim, ClaimError,
    FeedTemplate, Hash32, MinerAddress, Observation, ObservationError, ObservationKind,
    ObservationSpec, ProofLevel, Registry, Snapshot, TemplateError, Value, VerificationMethod,
};
use onyx_labels::attest::{eip191_recover, eip191_sign_with, secret_address};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

/// Longest gap between the end of the observation window and the commit
/// deadline, in seconds: observers commit right after they fetch.
pub const MAX_COMMIT_GRACE_SECS: u64 = 600;
/// Shortest reveal phase, in seconds.
pub const MIN_REVEAL_SECS: u64 = 60;
/// Longest reveal phase, in seconds (L2API-R1-F2). The reveal close is not a
/// price input (L2API-R2-F2, R3-F1): it only bounds how long a round runs.
pub const MAX_REVEAL_SECS: u64 = 86_400;

/// Round timing and economics, fixed when the round opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ObserveConfig {
    /// Last second a commit is accepted (at or after the window end).
    pub commit_close: u64,
    /// Last second a reveal is accepted (reveals open after `commit_close`).
    pub reveal_close: u64,
    /// Paid to each observer who agreed with the settled result.
    pub reward: u128,
    /// Taken from an observer whose answer disagreed with the settled value,
    /// and from one who committed but never revealed.
    pub outlier_slash: u128,
    /// Taken from an observer who never committed, or who answered
    /// `Unavailable` while a majority settled a value.
    pub miss_slash: u128,
}

impl ObserveConfig {
    /// The timing and economics rules a round must meet to open: commits close
    /// within [`MAX_COMMIT_GRACE_SECS`] of the window end, reveals last between
    /// [`MIN_REVEAL_SECS`] and [`MAX_REVEAL_SECS`], withholding is never
    /// cheaper than a wrong answer, and every amount fits a ledger delta.
    /// Public so a consumer can apply the same rules to a served round
    /// (L2API-R1-F1).
    pub fn validate(&self, spec: &ObservationSpec) -> Result<(), ObserveError> {
        if self.commit_close < spec.window_end
            || self.commit_close - spec.window_end > MAX_COMMIT_GRACE_SECS
        {
            return Err(ObserveError::BadConfig("commit_close"));
        }
        if self.reveal_close < self.commit_close.saturating_add(MIN_REVEAL_SECS)
            || self.reveal_close - self.commit_close > MAX_REVEAL_SECS
        {
            return Err(ObserveError::BadConfig("reveal_close"));
        }
        // Withholding a wrong answer must never be cheaper than revealing it.
        if self.miss_slash == 0 || self.outlier_slash < self.miss_slash {
            return Err(ObserveError::BadConfig("slashes"));
        }
        // Every amount must be a ledger delta as is: no silent clamp (L2R-F7).
        let max = i128::MAX.unsigned_abs();
        if self.reward > max || self.outlier_slash > max || self.miss_slash > max {
            return Err(ObserveError::BadConfig("amount above i128::MAX"));
        }
        Ok(())
    }
}

/// An assigned observer and the bond it posted for this feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Observer {
    /// Address.
    pub address: MinerAddress,
    /// Bond; at least `outlier_slash`, so every slash is covered.
    pub bond: u128,
}

/// Most members an observer pool snapshot may hold (bounds the draw's work).
pub const MAX_POOL: usize = 1_024;
/// Most draws a fact may take: attempts 0..=63 (L2RND-R1-F7), the same bound
/// as `OnyxRegistry.MAX_DRAW_ATTEMPTS`. An attempt is used when it is
/// announced (L2RND-R1-F3), never reused, and never reset (a reviewer's
/// reopening included), so a coordinator cannot draw again for free.
pub const MAX_DRAW_ATTEMPTS: u32 = 64;
/// Most draws a (metric, subject) may hold that did not end with a recorded
/// value, across every window and attempt, before the registry owner records
/// a reopen (L2RND-R2-F1/F2): `OnyxRegistry.MAX_DRAWS`, equal to
/// [`MAX_NON_VALUE_ROUNDS`]. Enforced on chain (`announceDraw` reverts with
/// `DrawsExhausted`); runners and clients read the budget from the chain.
pub const MAX_SUBJECT_DRAWS: u32 = 3;

/// The deployment chain as a coordinator's store reads it (L2RND-R1-F1): its
/// current height and its block hashes. A draw's beacon height is announced
/// above [`height`](ChainView::height), and the beacon is only ever the
/// block hash read here, never a caller argument, so a coordinator cannot
/// try beacons offline and open with the one it likes. Consumers check the
/// same beacon against `OnyxRegistry.drawMatches` (the announcement and the
/// beacon fixed on chain).
pub trait ChainView {
    /// The chain's current height.
    fn height(&self) -> Result<u64, String>;
    /// The block hash at `height`, `None` while that block does not exist.
    fn block_hash(&self, height: u64) -> Result<Option<Hash32>, String>;
}

/// A simulated chain for tests and offline rehearsals: a fixed height, and
/// one fixed hash for every block at or below `beacon_upto`. Never a real
/// deployment's chain: a beacon it serves is not random.
#[derive(Clone, Copy, Debug)]
pub struct SimulatedChain {
    /// The height `height()` reports.
    pub height: u64,
    /// The hash every existing block has.
    pub beacon: Hash32,
    /// Blocks above this height do not exist yet.
    pub beacon_upto: u64,
}

impl SimulatedChain {
    /// A chain at `height` (for announcing), whose blocks all hash to a fixed value.
    pub fn at(height: u64) -> Self {
        SimulatedChain {
            height,
            beacon: keccak256(b"onyx-simulated-beacon"),
            beacon_upto: u64::MAX,
        }
    }
    /// A chain whose every block hashes to `beacon` (for opening).
    pub fn with_beacon(beacon: Hash32) -> Self {
        SimulatedChain {
            height: 0,
            beacon,
            beacon_upto: u64::MAX,
        }
    }
}

impl ChainView for SimulatedChain {
    fn height(&self) -> Result<u64, String> {
        Ok(self.height)
    }
    fn block_hash(&self, height: u64) -> Result<Option<Hash32>, String> {
        Ok((height <= self.beacon_upto).then_some(self.beacon))
    }
}

/// How many blocks above the chain height a runner puts its beacon, so the
/// announcement is included before the beacon block exists.
pub const DRAW_LEAD: u64 = 3;

/// The EVM's `blockhash` window: `fixBeacon` can read a beacon only while
/// its block is among the chain's latest 256 (`BeaconUnavailable` after).
pub const BLOCKHASH_WINDOW: u64 = 256;

/// The fact arguments `OnyxRegistry` keys a draw by (`factKey(metric,
/// subjectKey, windowEnd)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnchainFact {
    /// `onchain::metric_key` of the spec's metric.
    pub metric_key: Hash32,
    /// `onchain::subject_key` of the spec's subject.
    pub subject_key: Hash32,
    /// The spec's window end (the registry zeroes it for a dated metric).
    pub window_end: u64,
    /// Whether the metric is dated (the registry knows it from the metric's
    /// registered kind; only [`SimulatedRegistry`] reads this).
    pub dated: bool,
    /// The metric's template id (the registry knows it from the metric's
    /// registration): with the rest it makes the registry's fact key.
    pub template: Hash32,
}

impl OnchainFact {
    /// `OnyxRegistry.factKeyOf` of this fact
    /// ([`onyx_core::onchain::registry_fact_key`]): the key the transcript
    /// log (ADR-0027 amendment 8) and the draws are stored under.
    pub fn registry_key(&self) -> Hash32 {
        onyx_core::onchain::registry_fact_key(
            &self.metric_key,
            &self.template,
            &self.subject_key,
            self.window_end,
            self.dated,
        )
    }
}

/// The arguments of `OnyxRegistry.announceDraw` for one draw (L2RND-R1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnchainDraw {
    /// The fact.
    pub fact: OnchainFact,
    /// The attempt (the fact's draw count before this one).
    pub attempt: u32,
    /// [`draw_commit`] of the snapshot, the spec and the config: every input of
    /// the draw, fixed before the beacon exists (L2RND-R3).
    pub commit: Hash32,
    /// The beacon's height, above the chain height at announcement.
    pub beacon_height: u64,
}

/// A draw as the registry recorded it (`drawOf`, `drawCount`), read back
/// after `fixBeacon`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DrawRecord {
    /// The chain's EIP-155 id.
    pub chain_id: u64,
    /// The `OnyxRegistry` address.
    pub registry: [u8; 20],
    /// The attempt.
    pub attempt: u32,
    /// The fact's draw count (the attempt is the latest iff it is count - 1).
    pub draw_count: u32,
    /// The block the announcement was included in.
    pub announced_at: u64,
    /// The draw commitment announced ([`draw_commit`], L2RND-R3).
    pub commit: Hash32,
    /// The beacon height announced.
    pub beacon_height: u64,
    /// The beacon `fixBeacon` read (`blockhash(beaconHeight)`).
    pub beacon: Hash32,
}

impl DrawRecord {
    /// The record as JSON (fixtures and site summaries).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "chain_id": self.chain_id,
            "registry": format!("0x{}", hex::encode(self.registry)),
            "attempt": self.attempt,
            "draw_count": self.draw_count,
            "announced_at": self.announced_at,
            "commit": self.commit.to_string(),
            "beacon_height": self.beacon_height,
            "beacon": self.beacon.to_string(),
        })
    }

    /// Read [`to_json`](Self::to_json)'s output back.
    pub fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        let n = |k: &str| v[k].as_u64().ok_or_else(|| format!("draw record: {k}"));
        let n32 =
            |k: &str| n(k).and_then(|x| u32::try_from(x).map_err(|_| format!("draw record: {k}")));
        let h = |k: &str| {
            v[k].as_str()
                .and_then(parse_hash)
                .ok_or_else(|| format!("draw record: {k}"))
        };
        let registry: [u8; 20] = v["registry"]
            .as_str()
            .and_then(|s| hex::decode(s.strip_prefix("0x")?).ok())
            .and_then(|b| b.try_into().ok())
            .ok_or("draw record: registry")?;
        Ok(DrawRecord {
            chain_id: n("chain_id")?,
            registry,
            attempt: n32("attempt")?,
            draw_count: n32("draw_count")?,
            announced_at: n("announced_at")?,
            commit: h("commit")?,
            beacon_height: n("beacon_height")?,
            beacon: h("beacon")?,
        })
    }
}

/// Calldata of `OnyxRegistry.drawMatches(metric, subjectKey, windowEnd,
/// attempt, commit, beaconHeight, beacon)` for a round's draw `seed` of `fact`
/// (L2RND-R1). The chain answers true only if this is the fact's latest
/// announced draw, announced before its beacon height with this commitment
/// (`commit`, the caller's own [`draw_commit`] of pool, spec and config,
/// L2RND-R3), and the beacon is the block hash `fixBeacon` read at that height.
pub fn draw_matches_calldata(fact: &OnchainFact, seed: &DrawSeed, commit: &Hash32) -> Vec<u8> {
    let word = |n: u64| {
        let mut w = [0u8; 32];
        w[24..].copy_from_slice(&n.to_be_bytes());
        w
    };
    let mut c = keccak256(b"drawMatches(bytes32,bytes32,uint64,uint32,bytes32,uint64,bytes32)").0
        [..4]
        .to_vec();
    c.extend_from_slice(&fact.metric_key.0);
    c.extend_from_slice(&fact.subject_key.0);
    c.extend_from_slice(&word(fact.window_end));
    c.extend_from_slice(&word(u64::from(seed.attempt)));
    c.extend_from_slice(&commit.0);
    c.extend_from_slice(&word(seed.beacon_height));
    c.extend_from_slice(&seed.beacon.0);
    c
}

/// The `OnyxRegistry` deployment a runner announces draws on, as it tags
/// its results store (review R10-1): chain id and registry address, and
/// (review R11-1) the hash of the block the registry was deployed in. The
/// address alone separates nothing on a fresh local chain: every
/// rehearsal anvil deploys from the same dev account at nonce 0, so every
/// one has the same chain id and registry address; its deploy block's hash
/// (which covers the chain's own genesis and timestamps) differs between
/// anvils started at different times. It does NOT always differ (review
/// R12, REVLOG-R12-4): two anvils started with the same `--timestamp` mine
/// the same block 1 and the same deploy block hash. There the beacon match
/// (review R10-1 (a): a published draw's nonzero beacon must be the
/// chain's) still guards; on a live chain the hash is unique.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistryDeployment {
    /// The chain's EIP-155 id.
    pub chain_id: u64,
    /// The `OnyxRegistry` address.
    pub registry: [u8; 20],
    /// The hash of the block the registry's code first appears in.
    pub deploy_block: Hash32,
}

/// A chain a runner announces its draws on (L2RND-R1): an `OnyxRegistry`
/// deployment. Reads go through [`ChainView`].
pub trait DrawChain: ChainView {
    /// Review R10-1, R11-1: the deployment draws are announced on (chain
    /// id, registry address, the registry's deploy block hash), which a
    /// runner tags its results store with and checks a results store
    /// against before a run. `None` (the default): a chain without one (a
    /// replay, a test double): only a results store naming no deployment is
    /// read with it.
    fn deployment(&self) -> Result<Option<RegistryDeployment>, String> {
        Ok(None)
    }
    /// Blocks above the chain height a new draw's beacon goes ([`DRAW_LEAD`];
    /// more on a chain with fast blocks, ROADMAP B6).
    fn draw_lead(&self) -> Result<u64, String> {
        Ok(DRAW_LEAD)
    }
    /// `drawCount` of the fact: how many draws the registry has recorded.
    fn draw_count(&self, fact: &OnchainFact) -> Result<u32, String>;
    /// `drawBudget(metric, subject)`: the subject's draws that did not end
    /// with a recorded value, and how many it may hold (L2RND-R2-F1).
    fn draw_budget(&self, fact: &OnchainFact) -> Result<(u32, u32), String>;
    /// Send `announceDraw` (before the beacon height).
    fn announce(&mut self, draw: &OnchainDraw) -> Result<(), String>;
    /// Wait until the beacon block exists, send `fixBeacon`, and read the
    /// draw back. Like the registry, a beacon already fixed (by anyone) is an
    /// error here (`BeaconAlreadyFixed`); [`draw_and_open`] reads the draw
    /// first and again after any error ([`Self::draw_of`], review REVLOG-R5),
    /// so a stranger's `fixBeacon` or a failed read after the send is never
    /// a lost draw.
    fn fix_beacon(&mut self, draw: &OnchainDraw) -> Result<DrawRecord, String>;
    /// `drawOf(fact, attempt)` with the fact's `drawCount`: the draw as the
    /// registry holds it now (its beacon zero until fixed), `None` when no
    /// draw is announced at `attempt`. Read-only (review REVLOG-R5).
    fn draw_of(&self, fact: &OnchainFact, attempt: u32) -> Result<Option<DrawRecord>, String>;
    /// `drawOf(fact, attempt)` read AT one confirmed block (review R6-B,
    /// R6-A, R6-D): the chain's finalized block (a pinned depth below the
    /// head on a local anvil, the head of a simulated chain), the same block
    /// [`Self::log_block`] reports. Returns that block's number and the draw
    /// as of it. The read names the block (`--block N`): a node without it
    /// must answer an error, never an older state, so a lagging node cannot
    /// show a stale draw as of a block it never had. What
    /// [`draw_and_open`] decides from it holds for ever: no announcement
    /// lands at or after the beacon height (`BeaconNotFuture`), and no
    /// beacon is fixed after its block left the 256-block window. So the
    /// block must be FINAL: a live chain needs an RPC that serves state at
    /// the finalized block (archive-capable), and an OP stack's `safe` block
    /// is refused (review R7); without such a read every decision that needs
    /// it stops the run (the draw stays in the book), none voids a draw.
    fn draw_of_final(
        &self,
        fact: &OnchainFact,
        attempt: u32,
    ) -> Result<(u64, Option<DrawRecord>), String>;
    /// Relay an observer's signed commit to the transcript log
    /// (`OnyxRegistry.postCommit`, ADR-0027 amendment 8). A slot already
    /// filled (by anyone, once per observer and round) is
    /// [`Posted::Before`], never an error: what the slot holds is what
    /// [`absorb_posted_commits`] reads. Required (review REVLOG-R2 LOW-5):
    /// no chain gets a silent default.
    fn post_commit(
        &mut self,
        fact: &OnchainFact,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
    ) -> Result<Posted, String>;
    /// Post a reveal receipt to the transcript log (`postReceipt`); a slot
    /// already filled is [`Posted::Before`].
    fn post_receipt(
        &mut self,
        fact: &OnchainFact,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
    ) -> Result<Posted, String>;
    /// The newest block the transcript log may be read at, number and time
    /// from ONE answer (review REVLOG-R3-1): at or below the chain's
    /// `finalized` block (OP-stack finality), or a pinned depth below the
    /// head on a local anvil, so no reorg can add an in-phase post after
    /// the read. `None` only for a chain without a log (a recorded replay).
    fn log_block(&self) -> Result<Option<LogBlock>, String>;
    /// `observer`'s commit posted for `round_id`, read from contract state AT
    /// `block` exactly (`commitLogOf` with that block tag: commit, block
    /// time and the observer's signature in one `eth_call`; never
    /// `eth_getLogs`, review REVLOG-R2 HIGH-1), `None` when its slot is
    /// empty at `block`. A node that does not have `block` must answer an
    /// error, never an older state (review REVLOG-R3-1).
    fn posted_commit(
        &self,
        block: u64,
        fact: &OnchainFact,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
    ) -> Result<Option<PostedCommit>, String>;
    /// `OnyxRegistry.transcriptComplete` for `q`.
    fn transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String>;
    /// `OnyxRegistry.drawReceiptSigner(fact, attempt)`: the receipt signer
    /// the draw snapshotted when it was announced (ROADMAP B3, THREATS
    /// REVLOG-4), `None` when this chain view cannot tell (a recorded
    /// replay; the registry's `postReceipt` still checks it). Read before
    /// the first receipt ([`finish_reveals`]): a run whose receipt key is not
    /// the draw's (the key was rotated since the draw was announced) stops
    /// there, before any receipt, and keeps the round unfinished.
    fn draw_receipt_signer(
        &self,
        _fact: &OnchainFact,
        _attempt: u32,
    ) -> Result<Option<[u8; 20]>, String> {
        Ok(None)
    }
    /// `OnyxRegistry.drawRevealKey(fact, attempt)` (ADR-0027 amendment 10 item
    /// 3, ROADMAP C11): the reveal key the draw snapshotted when it was
    /// announced; `None` when the draw has none (its observers reveal through
    /// the spool, REVLOG-1 open for them) or this chain view cannot tell (a
    /// recorded replay, a test double: nothing sealed is read there; a client
    /// still asks the registry itself and refuses a round of a draw with a key
    /// that comes without its sealed posts).
    fn draw_reveal_key(
        &self,
        _fact: &OnchainFact,
        _attempt: u32,
    ) -> Result<Option<crate::sealed::Point>, String> {
        Ok(None)
    }
    /// `OnyxRegistry.drawTimeLocked(fact, attempt)` (ADR-0027 amendment 14,
    /// ROADMAP C51): whether the draw was announced while the registry required
    /// time-locks, so its posts are opened only with the lock round's beacon
    /// signature. By default `false` only for a draw this view shows WITHOUT a
    /// reveal key (no sealed post at all); for a keyed draw a view must answer
    /// it itself, else the run stops (review C51-R1-3: a wrapper that forwarded
    /// `draw_reveal_key` alone would open time-locked posts amendment 10 style,
    /// dismiss every honest one and record that round for good).
    fn draw_time_locked(&self, fact: &OnchainFact, attempt: u32) -> Result<bool, String> {
        match self.draw_reveal_key(fact, attempt)? {
            None => Ok(false),
            Some(_) => Err(
                "this chain view cannot answer drawTimeLocked for a draw with a reveal key".into(),
            ),
        }
    }
    /// Where this coordinator reads drand beacon signatures for time-locked
    /// draws (ROADMAP C51): none by default, and then a time-locked round WAITS
    /// (it is never opened another way). Whatever it returns is verified
    /// against the pinned quicknet key before use.
    fn beacon(&self) -> Option<&dyn crate::timelock::BeaconSource> {
        None
    }
    /// `observer`'s sealed post for `round_id` (ROADMAP C11), read from contract
    /// state AT `block` exactly (`sealedRevealOf` and `sealedKeyProofOf` with
    /// that block tag; never event logs, review REVLOG-R2-H1), `None` when its
    /// slot is empty there. A node that does not have `block` must answer an
    /// error, never an older state. Required of a chain that answers
    /// [`Self::draw_reveal_key`].
    fn sealed_post(
        &self,
        _block: u64,
        _fact: &OnchainFact,
        _attempt: u32,
        _observer: &MinerAddress,
        _round_id: &Hash32,
    ) -> Result<Option<PostedSealed>, String> {
        Err("this chain view cannot read sealed posts".into())
    }
    /// `OnyxRegistry.revealLogComplete` for `q` (ROADMAP C11). Required of a
    /// chain that answers [`Self::draw_reveal_key`].
    fn reveal_log_complete(&self, _q: &crate::sealed::RevealLogQuery) -> Result<bool, String> {
        Err("this chain view cannot answer revealLogComplete".into())
    }
    /// Whether this chain records rounds (`recordAttempt`, `recordObservation`)
    /// when they settle ([`record_round`], ROADMAP C11): the live coordinator's
    /// chain does; replays, test doubles and runners that record elsewhere (the
    /// deploy run) do not.
    fn records_rounds(&self) -> bool {
        false
    }
    /// `OnyxRegistry.recordAttempt(q…, result)` for a round that settled no
    /// value (amendment 10 item 7). A draw already recorded with exactly this
    /// round is [`Recorded::Before`]; one recorded with another is an error.
    fn record_attempt(
        &mut self,
        _q: &crate::sealed::RevealLogQuery,
        _result: &Hash32,
    ) -> Result<Recorded, String> {
        Err("this chain view does not record attempts".into())
    }
    /// `OnyxRegistry.recordObservation` for a value round (`rec`); a fact
    /// already recorded with exactly this result is [`Recorded::Before`].
    fn record_observation(&mut self, _rec: &OnchainObservationCheck) -> Result<Recorded, String> {
        Err("this chain view does not record observations".into())
    }
}

/// What [`DrawChain::record_attempt`] or [`DrawChain::record_observation`]
/// found (ROADMAP C11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// This call recorded the round.
    Now,
    /// The chain already held exactly this record (a retry).
    Before,
}

/// What a post to the transcript log found (ADR-0027 amendment 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Posted {
    /// This call filled the slot.
    Now,
    /// The slot already held this commit (posted first by anyone).
    Before(Hash32),
}

/// A block of the chain the transcript log is read at (review REVLOG-R3-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct LogBlock {
    /// The block number every read of the round names.
    pub number: u64,
    /// Its block time (unix seconds); the log is read only once it is past
    /// `commit_close`.
    pub time: u64,
}

/// The round's transcript log as read ONCE, from one block (review
/// REVLOG-R3-1): kept in the round ([`ObservationRound::log_snapshot`]) and
/// saved with it, so retries and later runs reuse it and never read the
/// log again.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct LogSnapshot {
    /// The block every slot was read at; `None` for a chain without a log
    /// (a recorded replay).
    pub block: Option<LogBlock>,
    /// Every drawn observer's filled slot at `block`, ascending by observer
    /// (in phase or not: [`ObservationRound::absorb_log`] decides).
    pub posted: Vec<(MinerAddress, PostedCommit)>,
}

/// A commit posted to the transcript log: the commit, the observer's own
/// signature (from `commitLogOf`) and the block time it landed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PostedCommit {
    /// The posted commit.
    pub commit: Hash32,
    /// The observer's commit signature.
    pub sig: ObserverSig,
    /// The block time it landed (`commitLogOf.postedAt`).
    pub posted_at: u64,
}

/// How long a runner waits on the chain (reviews REVLOG-R2, REVLOG-R3-1). A
/// chain error or a commit phase not yet over on chain means "try again",
/// never "publish a failed round": each chain call is tried
/// [`tries`](Self::tries) times, and the log block
/// ([`DrawChain::log_block`], finalized) is polled [`polls`](Self::polls)
/// times for a block past the commit phase, [`pause`](Self::pause) apart.
/// When the budget runs out the run stops with its round unfinished (saved
/// with the round book, finished later on the same draw), so waiting never
/// burns one of the fact's draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainWait {
    /// Tries of one chain call (transient RPC errors).
    pub tries: u32,
    /// Polls of the log block for the end of the commit phase.
    pub polls: u32,
    /// Pause between tries and polls.
    pub pause: std::time::Duration,
}

impl ChainWait {
    /// Rehearsals and operators: 5 tries per call, the commit phase waited
    /// for up to 30 minutes (360 polls, 5 s apart): an OP-stack block is
    /// typically finalized 15 to 30 minutes after it is produced (ADR-0027
    /// amendment 8, item 4b); a longer wait leaves the round unfinished in
    /// the book, and the next run finishes it.
    pub const DEFAULT: ChainWait = ChainWait {
        tries: 5,
        polls: 360,
        pause: std::time::Duration::from_secs(5),
    };
    /// Tests: 3 tries, one poll, no pause.
    pub const NOW: ChainWait = ChainWait {
        tries: 3,
        polls: 1,
        pause: std::time::Duration::ZERO,
    };

    /// `f` until it succeeds, at most [`tries`](Self::tries) times; the last
    /// error otherwise.
    pub fn retry<T>(&self, mut f: impl FnMut() -> Result<T, String>) -> Result<T, String> {
        let mut last = String::from("no try");
        for i in 0..self.tries.max(1) {
            if i > 0 {
                std::thread::sleep(self.pause);
            }
            match f() {
                Ok(v) => return Ok(v),
                Err(e) => last = e,
            }
        }
        Err(format!("{last} (after {} tries)", self.tries.max(1)))
    }

    /// Wait for the block the round's log is read at (review REVLOG-R3-1):
    /// the chain's log block ([`DrawChain::log_block`], finalized) once its
    /// time is past `commit_close`, so no post can still land in the commit
    /// phase and no reorg can add one. Err (the round stays unfinished) when
    /// it is not after [`polls`](Self::polls) polls. `None`: a chain without
    /// a log never waits.
    pub fn commit_phase_over(
        &self,
        chain: &dyn DrawChain,
        commit_close: u64,
    ) -> Result<Option<LogBlock>, String> {
        let mut now = None;
        for i in 0..self.polls.max(1) {
            if i > 0 {
                std::thread::sleep(self.pause);
            }
            match self.retry(|| chain.log_block())? {
                None => return Ok(None),
                Some(b) if b.time > commit_close => return Ok(Some(b)),
                Some(b) => now = Some(b),
            }
        }
        let b = now.unwrap_or(LogBlock { number: 0, time: 0 });
        Err(format!(
            "the chain's finalized block {} (time {}) is not past commit close {commit_close}: commits may still be posted; finish the round later on the same draw",
            b.number, b.time
        ))
    }

    /// Wait for the lock round's beacon signature of a time-locked round
    /// (ROADMAP C51, ADR-0027 amendment 14): [`DrawChain::beacon`] is polled
    /// [`polls`](Self::polls) times for round
    /// [`crate::timelock::lock_round`]`(reveal_close)`, and only a signature that
    /// verifies against the pinned quicknet key is returned. Missing, malformed,
    /// forged or another round's: the round WAITS (Err, unfinished, nothing
    /// opened; a later run finishes it on the same draw). There is no path that
    /// opens a time-locked post without it (THREATS C12-4; abandonment is C53).
    pub fn lock_round_signature(
        &self,
        chain: &dyn DrawChain,
        reveal_close: u64,
    ) -> Result<(u64, Vec<u8>), String> {
        use crate::timelock::TimeLock as _;
        let q = crate::timelock::QuicknetTimeLock::pinned().map_err(|e| e.to_string())?;
        let rho = crate::timelock::lock_round(q.chain(), reveal_close)
            .ok_or("no lock round for this window")?;
        let Some(source) = chain.beacon() else {
            return Err(format!(
                "a time-locked draw: its posts open only with drand quicknet round {rho}'s signature and this coordinator has no beacon source (--beacon); the round waits, unfinished"
            ));
        };
        let mut last = String::from("not published yet");
        for i in 0..self.polls.max(1) {
            if i > 0 {
                std::thread::sleep(self.pause);
            }
            match source.signature(rho) {
                Ok(Some(sig)) if q.verify_signature(rho, &sig) => return Ok((rho, sig)),
                Ok(Some(_)) => {
                    last = "the source gave a signature that is not the pinned chain's for this round (refused)".into();
                }
                Ok(None) => last = "not published yet".into(),
                Err(e) => last = e,
            }
            if i == 0 {
                eprintln!(
                    "time-locked round: waiting for drand quicknet round {rho} ({last}); nothing is opened before it"
                );
            }
        }
        Err(format!(
            "drand quicknet round {rho} (the lock round of reveal close {reveal_close}): {last}; nothing opened, the round waits unfinished: finish it later on the same draw (THREATS C12-4, C53)"
        ))
    }

    /// Wait for the block a round's sealed posts are read at (ROADMAP C11):
    /// the chain's log block ([`DrawChain::log_block`], finalized) once its
    /// time is past `reveal_close`, so no post can still land in the reveal
    /// phase and no reorg can add one. A coordinator that read before (C11-1)
    /// would miss a post landing later in the window, and every client would
    /// refuse the round. Err (the round stays unfinished) when it is not after
    /// [`polls`](Self::polls) polls, or the chain has no log at all.
    pub fn reveal_phase_over(
        &self,
        chain: &dyn DrawChain,
        reveal_close: u64,
    ) -> Result<LogBlock, String> {
        let mut now = LogBlock { number: 0, time: 0 };
        for i in 0..self.polls.max(1) {
            if i > 0 {
                std::thread::sleep(self.pause);
            }
            match self.retry(|| chain.log_block())? {
                None => {
                    return Err(
                        "this chain has no final block to read sealed posts at; a sealed draw is finished on its own chain"
                            .into(),
                    )
                }
                Some(b) if b.time > reveal_close => return Ok(b),
                Some(b) => now = b,
            }
        }
        Err(format!(
            "the chain's finalized block {} (time {}) is not past reveal close {reveal_close}: sealed posts may still land; finish the round later on the same draw",
            now.number, now.time
        ))
    }
}

/// Every drawn observer's slot of `round` at `block` (one block for every
/// read, review REVLOG-R3-1).
fn read_log_at(
    round: &ObservationRound,
    chain: &dyn DrawChain,
    fact: &OnchainFact,
    block: LogBlock,
) -> Result<LogSnapshot, String> {
    let mut posted = vec![];
    for o in &round.observers {
        if let Some(p) = chain.posted_commit(
            block.number,
            fact,
            round.draw.attempt,
            &o.address,
            &round.round_id,
        )? {
            posted.push((o.address, p));
        }
    }
    Ok(LogSnapshot {
        block: Some(block),
        posted,
    })
}

/// The coordinator reads the transcript log ONCE per round (ADR-0027
/// amendment 8; reviews REVLOG HIGH-1, REVLOG-R3-1), before it takes any
/// reveal: from one block N, at or below the chain's finalized block with
/// its time past `commit_close` ([`ChainWait::commit_phase_over`]), every
/// drawn observer's slot is read at N, and every commit posted inside the
/// commit phase replaces what that observer sent off chain
/// ([`ObservationRound::absorb_log`]). Before the snapshot is kept, the
/// chain's own `transcriptComplete` must accept the round with the snapshot
/// absorbed and no reveal: a stale or wrong answer for N (a lagging node, a
/// node that does not have N) is caught here, before any receipt, and the
/// read is tried again. The snapshot then lives in the round: once it is
/// there, this returns its absorbed observers without touching the chain,
/// so a retry or a later run never reads the log again. A posted commit
/// that never gets a valid reveal is a non-reveal, as any other (never
/// slashed). Returns the observers whose posted commit was absorbed.
pub fn absorb_posted_commits(
    round: &mut ObservationRound,
    chain: &dyn DrawChain,
    fact: &OnchainFact,
    wait: &ChainWait,
) -> Result<Vec<MinerAddress>, String> {
    if let Some(s) = &round.log {
        return Ok(round.absorbed_of(s));
    }
    let at = wait.commit_phase_over(chain, round.cfg.commit_close)?;
    wait.retry(|| {
        let snap = match at {
            None => LogSnapshot {
                block: None,
                posted: vec![],
            },
            Some(b) => read_log_at(round, chain, fact, b)?,
        };
        let mut next = round.clone();
        let absorbed = next
            .absorb_log(snap)
            .map_err(|e| format!("transcript log: {e}"))?;
        if let Some(b) = at {
            let q = next.transcript_query(fact.registry_key());
            if !chain.transcript_complete(&q)? {
                return Err(format!(
                    "the log read at block {} disagrees with the chain's transcriptComplete (a stale or wrong node answer); nothing was receipted, read again",
                    b.number
                ));
            }
        }
        *round = next;
        Ok(absorbed)
    })
}

/// Take `observer`'s reveal after the commit phase and post its receipt
/// (ADR-0027 amendment 8), signed with `receipt_key`. Only after the round's
/// log snapshot is kept ([`absorb_posted_commits`], review REVLOG-R3-1): no
/// receipt is ever signed before. Whatever the observer sends never stops
/// the round (review REVLOG-R2 MEDIUM-2): a reveal that does not open the
/// observer's commit (a posted commit replaced it), is badly signed, fails
/// the feed's checks, comes without a commit or outside the reveal phase is
/// `Ok(false)`, a non-reveal, with no receipt. A second reveal is ignored:
/// the round keeps the first, and its receipt is posted again (a slot
/// already holding it is success), so a retry after a failed `postReceipt`
/// finishes the job. `Ok(true)`: the round holds the observer's reveal and
/// its receipt is on chain. Err only for the chain (retry it), a round whose
/// log was not read, or a receipt slot holding another commit (a leaked
/// receipt key, THREATS REVLOG-4). `now` is the time the reveal ARRIVED
/// ([`ArrivedReveal`], review REVLOG-R4-3), not the time of the receipt.
#[allow(clippy::too_many_arguments)]
pub fn take_reveal(
    round: &mut ObservationRound,
    chain: &mut dyn DrawChain,
    fact: &OnchainFact,
    receipt_key: &[u8; 32],
    now: u64,
    observer: MinerAddress,
    observation: Observation,
    salt: Hash32,
    sig: ObserverSig,
) -> Result<bool, String> {
    if round.log.is_none() {
        return Err(
            "the transcript log was not read: no receipt before the round's log snapshot".into(),
        );
    }
    match round.reveal(now, observer, observation, salt, sig) {
        Ok(()) | Err(ObserveError::AlreadyRevealed) => {}
        Err(_) => return Ok(false),
    }
    let commit = round
        .commits
        .get(&observer)
        .map(|(h, _)| *h)
        .ok_or("reveal without a commit")?;
    let receipt = round
        .reveal_receipt(receipt_key, &observer)
        .ok_or("no receipt for an accepted reveal")?;
    match chain
        .post_receipt(
            fact,
            round.draw.attempt,
            &observer,
            &round.round_id,
            &commit,
            &receipt,
        )
        .map_err(|e| format!("postReceipt: {e}"))?
    {
        Posted::Before(c) if c != commit => {
            Err("postReceipt: the slot holds a receipt for another commit".into())
        }
        _ => Ok(true),
    }
}

/// A signed reveal an observer sent: observer, observation, salt, the
/// observer's signature.
pub type SignedReveal = (MinerAddress, Observation, Hash32, ObserverSig);

/// A signed reveal and the time it reached the coordinator, by the
/// coordinator's own [`Clock`] (ADR-0027 amendment 8 item 4f, review
/// REVLOG-R4-3). The reveal phase is checked against `arrived`, never
/// against the time the receipt is signed: receipts wait for the finalized
/// log snapshot, which on an OP-stack chain lands long after `reveal_close`.
#[derive(Clone, Debug, PartialEq, BorshSerialize, BorshDeserialize)]
pub struct ArrivedReveal {
    /// The signed reveal.
    pub reveal: SignedReveal,
    /// Unix seconds it arrived (the coordinator's clock).
    pub arrived: u64,
}

/// The coordinator's own clock (unix seconds): it stamps each reveal's
/// arrival ([`UnfinishedRound::receive_reveal`], review REVLOG-R4-3).
pub trait Clock {
    /// The time now.
    fn now(&self) -> u64;
    /// Return once [`now`](Self::now) is at least `t`.
    fn wait_until(&self, t: u64);
}

/// The system clock: a live coordinator.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
    fn wait_until(&self, t: u64) {
        while self.now() < t {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
}

/// The system clock shifted by a fixed number of seconds (ROADMAP C51): a
/// LOCAL ANVIL rehearsal whose chain clock was started in the past
/// (`anvil --timestamp`), so its time-locked rounds lock to drand rounds that
/// are already published and served from fixtures (`--beacon fixture:FILE`).
/// It runs at the wall clock's rate; only
/// [`crate::cast_chain::anvil_clock`] makes one, and only on chain 31337.
#[derive(Clone, Copy, Debug)]
pub struct ShiftedClock {
    /// Seconds added to the system clock (negative: the past).
    pub offset: i64,
}

impl Clock for ShiftedClock {
    fn now(&self) -> u64 {
        let now = i64::try_from(SystemClock.now()).unwrap_or(i64::MAX);
        u64::try_from(now.saturating_add(self.offset)).unwrap_or(0)
    }
    fn wait_until(&self, t: u64) {
        while self.now() < t {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
}

/// A replayed clock (recorded rehearsals and tests): it stands still and
/// [`wait_until`](Clock::wait_until) jumps to the time waited for, so a
/// recorded round's observers reveal right after its commit phase, on the
/// round's own (past) timeline.
#[derive(Debug, Default)]
pub struct ReplayClock(std::cell::Cell<u64>);

impl ReplayClock {
    /// A replayed clock at `t`.
    pub fn at(t: u64) -> Self {
        ReplayClock(std::cell::Cell::new(t))
    }
    /// Move the clock forward by `secs` (a test's passing time).
    pub fn advance(&self, secs: u64) {
        self.0.set(self.0.get().saturating_add(secs));
    }
}

impl Clock for ReplayClock {
    fn now(&self) -> u64 {
        self.0.get()
    }
    fn wait_until(&self, t: u64) {
        self.0.set(self.0.get().max(t));
    }
}

/// The coordinator's reveal phase, up to its last check (ADR-0027 amendment
/// 8, reviews REVLOG HIGH-1, REVLOG-R2, REVLOG-R3 and REVLOG-R4): read the
/// log once ([`absorb_posted_commits`]; a round that holds its snapshot
/// reuses it), hand a newly taken snapshot to `checkpoint` (the runner saves
/// the round with its book there, BEFORE the first receipt is signed), take
/// each of `reveals` at the time it ARRIVED ([`take_reveal`]: what an
/// observer sends is never an error; review REVLOG-R4-3: the phase is the
/// arrival's, not the receipt's, since the snapshot waits for finality) and
/// ask `transcriptComplete`. Every chain call is retried per
/// `wait`. Idempotent: run it again with the same reveals after an error and
/// it finishes the job (the snapshot is reused, a reveal taken before is
/// ignored and its receipt posted again). An Err means "the round is
/// unfinished": the caller keeps it and finishes it later, never publishes
/// it as a failed round (receipted reveals would be dropped, REVLOG-3).
#[allow(clippy::too_many_arguments)]
pub fn finish_reveals(
    round: &mut ObservationRound,
    chain: &mut dyn DrawChain,
    fact: &OnchainFact,
    receipt_key: &[u8; 32],
    reveals: &[ArrivedReveal],
    wait: &ChainWait,
    checkpoint: &mut dyn FnMut(&ObservationRound) -> Result<(), String>,
) -> Result<(), String> {
    finish_reveals_sealed(
        round,
        chain,
        fact,
        None,
        receipt_key,
        None,
        reveals,
        wait,
        checkpoint,
    )
}

/// [`finish_reveals`] for a coordinator that may hold the draw's reveal key
/// (ROADMAP C11, ADR-0027 amendment 10 item 5). When the chain shows the draw
/// with a reveal key (`drawRevealKey`), the draw is sealed only: no spool
/// reveal is taken or receipted (review C11-R1-3), and
/// `reveal_secret` must be the key it names (a draw announced before a
/// rotation is finished with the key it snapshotted; without one the run stops
/// before reading anything); then, ONCE, from one finalized block past
/// `reveal_close` ([`ChainWait::reveal_phase_over`]: never before, C11-1),
/// every drawn observer's slot is read at that block (`sealedRevealOf` with
/// `sealedKeyProofOf`) and opened or dismissed ([`ObservationRound::absorb_sealed`]),
/// and the chain's own `revealLogComplete` must accept the round with them
/// before the snapshot is kept (a node that hides a post at the read block, or
/// a slot left unread, is caught here: C11-2); the round is handed to
/// `checkpoint` (saved with the book, dismissals included) and the last check
/// is `revealLogComplete`. A draw without a reveal key ends with
/// `transcriptComplete`, as before. A TIME-LOCKED draw (ROADMAP C51, C52) needs
/// the round's registered `template`: its far round, every post's far copy's,
/// is derived from it ([`crate::timelock::LockRounds::of_round`]); without it
/// the run stops before reading anything.
#[allow(clippy::too_many_arguments)]
pub fn finish_reveals_sealed(
    round: &mut ObservationRound,
    chain: &mut dyn DrawChain,
    fact: &OnchainFact,
    template: Option<&FeedTemplate>,
    receipt_key: &[u8; 32],
    reveal_secret: Option<&[u8; 32]>,
    reveals: &[ArrivedReveal],
    wait: &ChainWait,
    checkpoint: &mut dyn FnMut(&ObservationRound) -> Result<(), String>,
) -> Result<(), String> {
    // ROADMAP B3: receipts are signed only with the key the draw
    // snapshotted (`drawReceiptSigner`); after a rotation the round of an
    // earlier draw is finished with that draw's key, never the new one.
    let signer = secret_address(receipt_key).ok_or("the receipt key is not a valid key")?;
    if let Some(want) = wait.retry(|| chain.draw_receipt_signer(fact, round.draw.attempt))? {
        if want != signer {
            return Err(format!(
                "draw {} snapshotted the receipt signer 0x{} but this run's receipt key is 0x{}: finish this round with the key configured when its draw was announced (the receipt key was rotated since; ROADMAP B3)",
                round.draw.attempt,
                hex::encode(want),
                hex::encode(signer)
            ));
        }
    }
    let fresh = round.log.is_none();
    absorb_posted_commits(round, &*chain, fact, wait)?;
    if fresh {
        checkpoint(round).map_err(|e| format!("saving the log snapshot: {e}"))?;
    }
    let attempt = round.draw.attempt;
    // Review C11-R1-3: a draw with a reveal key is SEALED ONLY. Its reveals
    // are its in-phase posts; a reveal through the spool (stamped by the
    // coordinator's own clock, receipted at its choice after it saw every
    // value) is never taken, so the coordinator has no inclusion choice; and
    // clients require a served post for every revealed observer of such a draw.
    let key = wait.retry(|| chain.draw_reveal_key(fact, attempt))?;
    if key.is_none() {
        for ArrivedReveal {
            reveal: (a, o, s, sig),
            arrived,
        } in reveals
        {
            wait.retry(|| {
                take_reveal(
                    round,
                    chain,
                    fact,
                    receipt_key,
                    *arrived,
                    *a,
                    o.clone(),
                    *s,
                    *sig,
                )
            })?;
        }
    }
    let Some(key) = key else {
        let q = round.transcript_query(fact.registry_key());
        return if wait.retry(|| chain.transcript_complete(&q))? {
            Ok(())
        } else {
            Err("transcriptComplete is false: the round leaves out a posted commit or a receipted reveal; not recorded".into())
        };
    };
    let secret = reveal_secret.ok_or_else(|| {
        format!(
            "draw {attempt} has a reveal key (0x{}…): its sealed posts are opened only with the coordinator's reveal key; give --reveal-key FILE",
            hex::encode(&key[..8])
        )
    })?;
    if crate::sealed::public_point(secret).ok() != Some(key) {
        return Err(format!(
            "draw {attempt} snapshotted the reveal key 0x{}… but this run's is another: finish this round with the key configured when its draw was announced (ROADMAP C11)",
            hex::encode(&key[..8])
        ));
    }
    // ROADMAP C51: a time-locked draw is opened only with its lock round's
    // verified beacon signature, read before any post (the round waits
    // without it, never opens another way).
    let time_locked = wait.retry(|| chain.draw_time_locked(fact, attempt))?;
    if let Some(s) = &round.sealed {
        if time_locked != s.timelock.is_some() {
            return Err(format!(
                "draw {attempt}: the kept sealed snapshot is {} but the chain says the draw is {}",
                if s.timelock.is_some() {
                    "time-locked"
                } else {
                    "not time-locked"
                },
                if time_locked {
                    "time-locked"
                } else {
                    "not time-locked"
                }
            ));
        }
    }
    if round.sealed.is_none() {
        // ROADMAP C52: the far round every post's far copy must carry, from the
        // registered template, fixed before anything is read.
        let rounds = if time_locked {
            let t = template.ok_or_else(|| {
                format!(
                    "draw {attempt} is time-locked: its far round comes from the feed's registered template, which this run was not given"
                )
            })?;
            Some(
                crate::timelock::LockRounds::of_round(
                    &crate::timelock::BeaconChain::quicknet(),
                    t,
                    &round.spec,
                    round.cfg.reveal_close,
                )
                .map_err(|e| format!("draw {attempt}: {e}"))?,
            )
        } else {
            None
        };
        let at = wait.reveal_phase_over(&*chain, round.cfg.reveal_close)?;
        let lock = match rounds {
            Some(r) => Some((
                wait.lock_round_signature(&*chain, round.cfg.reveal_close)?
                    .1,
                r,
            )),
            None => None,
        };
        wait.retry(|| {
            let mut posts = vec![];
            for o in &round.observers {
                if let Some(p) =
                    chain.sealed_post(at.number, fact, attempt, &o.address, &round.round_id)?
                {
                    posts.push((o.address, p));
                }
            }
            let mut next = round.clone();
            next.absorb_sealed_locked(
                at,
                key,
                posts,
                secret,
                lock.as_ref().map(|(sig, r)| (sig.as_slice(), *r)),
            )
                .map_err(|e| format!("sealed posts read at block {}: {e}", at.number))?;
            let q = next.reveal_log_query(fact.registry_key());
            if !chain.reveal_log_complete(&q)? {
                return Err(format!(
                    "the sealed posts read at block {} disagree with the chain's revealLogComplete (a stale or wrong node answer, or a slot left out); nothing kept, read again",
                    at.number
                ));
            }
            *round = next;
            Ok(())
        })?;
        checkpoint(round).map_err(|e| format!("saving the sealed snapshot: {e}"))?;
    }
    let q = round.reveal_log_query(fact.registry_key());
    if wait.retry(|| chain.reveal_log_complete(&q))? {
        Ok(())
    } else {
        Err("revealLogComplete is false: the round leaves out a posted commit, a receipted reveal or a sealed post; not recorded".into())
    }
}

/// Record a settled round on chain before it is served (ROADMAP C11), when the
/// chain records rounds ([`DrawChain::records_rounds`]; `Ok(None)` otherwise):
/// a value round with `recordObservation` (`store`'s
/// [`ObservationResults::onchain_record`]); a round without a value with
/// `recordAttempt` (amendment 10 item 7, REVLOG-5), once the chain's finalized
/// block is past `reveal_close` (the registry refuses it before, `TooEarly`),
/// with [`ObservationRound::reveal_log_query`]: every drawn observer, the
/// config's window, the round id, and the settlement's result hash. Idempotent
/// (a retry finds the same record, [`Recorded::Before`]). Returns the attempt
/// digest of a recorded non-value round (served with the round), `None` for a
/// value round.
pub fn record_round(
    store: &ObservationResults,
    round: &ObservationRound,
    settlement: &ObservationSettlement,
    chain: &mut dyn DrawChain,
    fact: &OnchainFact,
    wait: &ChainWait,
) -> Result<Option<Hash32>, String> {
    if !chain.records_rounds() {
        return Ok(None);
    }
    let result = settlement.result_hash();
    if matches!(settlement.outcome(), ObservationOutcome::Value(_)) {
        let rec = store
            .onchain_record(&result)
            .ok_or("a value round this store did not settle")?;
        wait.retry(|| chain.record_observation(&rec))
            .map_err(|e| format!("recordObservation: {e}"))?;
        return Ok(None);
    }
    wait.reveal_phase_over(&*chain, round.cfg.reveal_close)?;
    let q = round.reveal_log_query(fact.registry_key());
    wait.retry(|| chain.record_attempt(&q, &result))
        .map_err(|e| format!("recordAttempt: {e}"))?;
    Ok(Some(q.digest(&result)))
}

/// The reveal phase's intake (ADR-0027 amendment 8 item 4f, review
/// REVLOG-R4-3): wait, by the coordinator's own `clock`, for the commit phase
/// to end; keep the round (`store`) and `save` it; then take each of
/// `reveals` as it arrives, stamped with the clock
/// ([`UnfinishedRound::receive_reveal`]), and save the round with every kept
/// reveal before the next one, so each reveal is on disk before any receipt
/// is signed ([`finish_reveals`] runs after this, once the finalized log
/// snapshot exists). An Err leaves the round kept in the store: unfinished,
/// nothing receipted.
pub fn receive_reveals(
    store: &mut ObservationResults,
    u: &mut UnfinishedRound,
    reveals: Vec<SignedReveal>,
    clock: &dyn Clock,
    save: &mut dyn FnMut(&ObservationResults) -> Result<(), String>,
) -> Result<(), String> {
    clock.wait_until(u.round.cfg.commit_close.saturating_add(1));
    store
        .keep_unfinished(u.clone())
        .map_err(|e| e.to_string())?;
    save(store).map_err(|e| format!("saving the round before its reveals: {e}"))?;
    for r in reveals {
        if u.receive_reveal(r, clock.now()) {
            store
                .keep_unfinished(u.clone())
                .map_err(|e| e.to_string())?;
            save(store).map_err(|e| format!("saving a reveal before its receipt: {e}"))?;
        }
    }
    Ok(())
}

/// Seconds between two reads of a [`RevealInbox`] while the reveal window is
/// open ([`keep_taking_reveals`]).
pub const REVEAL_POLL_SECS: u64 = 2;

/// Where the coordinator's reveal intake reads observers' signed reveals
/// from while a round's reveal window is open (THREATS REVLOG-R5-1d,
/// ADR-0027 amendment 8 item 8g): after a restart, independent observers
/// send again what the crash lost (ROADMAP B2 gives them their own keys and
/// transport). What it returns is untrusted: each reveal is checked by
/// [`UnfinishedRound::receive_reveal`] (drawn, signed by the observer, the
/// observer's first, in the window by the coordinator's clock).
pub trait RevealInbox {
    /// The reveals for round `round` (drawn observers `observers`) that
    /// reached the coordinator no later than `now` (the coordinator's clock,
    /// the arrival time they are stamped with; one that arrives during the
    /// call waits for the next). A reveal returned again is harmless: the
    /// intake keeps each observer's first.
    fn take(&mut self, round: &Hash32, observers: &[MinerAddress], now: u64) -> Vec<SignedReveal>;
    /// Whether anything can ever reach this inbox. `false` ([`NoInbox`]):
    /// the intake does not wait out the window for reveals that cannot come
    /// (review B5-L2: a live run would hold its book lock for nothing).
    fn can_deliver(&self) -> bool {
        true
    }
}

/// An inbox nothing reaches (the rehearsals' in-process observers hand
/// their reveals over directly).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoInbox;

impl RevealInbox for NoInbox {
    fn take(&mut self, _: &Hash32, _: &[MinerAddress], _: u64) -> Vec<SignedReveal> {
        Vec::new()
    }
    fn can_deliver(&self) -> bool {
        false
    }
}

/// A list of reveals, all arrived at the first read.
impl RevealInbox for Vec<SignedReveal> {
    fn take(&mut self, _: &Hash32, _: &[MinerAddress], _: u64) -> Vec<SignedReveal> {
        std::mem::take(self)
    }
}

/// THREATS REVLOG-R5-1d (ROADMAP B5): keep the reveal intake of the kept
/// round `u` open while its reveal window is, by the coordinator's own
/// `clock`. Used on the first run after [`receive_reveals`] and on a run
/// that resumes a round from the book, so a restart inside the window keeps
/// taking reveals instead of finishing at once. The window is the round's
/// config, saved with the book and bound to the draw's on-chain commitment
/// (`cfg_hash` in `draw_commit`, which clients check with `drawMatches`), so
/// a restart cannot move it. `inbox` is read every [`REVEAL_POLL_SECS`] (and
/// once at `reveal_close`) until every drawn observer revealed or the clock
/// is past `reveal_close`; each reveal it returns is stamped with the clock
/// at that read (at most [`REVEAL_POLL_SECS`] after it reached the inbox,
/// never late for one that reached it by `reveal_close`;
/// [`UnfinishedRound::receive_reveal`]: a replay keeps the first and its
/// arrival time; late, forged or undrawn ones are non-reveals), and each one
/// kept is kept in `store` and saved (`save`) before the next is read, so
/// it is on disk before any receipt. The intake is closed for good once the
/// round holds its log snapshot (review B5-M1): the snapshot is saved with the
/// book BEFORE the first receipt, so a round that may have receipts, a
/// settlement or a result recorded on chain takes no reveal after a restart,
/// whatever the clock reads (a replayed clock starts at 0, a wall clock can
/// step back). A round settled in `store` takes none either. An inbox that
/// cannot deliver ([`RevealInbox::can_deliver`]) is read once, never waited
/// on. The reveals `u` already holds (read back from a book, which loads
/// them unchecked) are checked again at their arrival time first, so an
/// entry that is no reveal never holds its observer's slot (review B5-R2-1).
/// Returns how many reveals were taken; Err only for a failed keep or save
/// (the caller keeps the round unfinished). Bounded: a griefing observer
/// that never reveals holds it until `reveal_close` at most.
pub fn keep_taking_reveals(
    store: &mut ObservationResults,
    u: &mut UnfinishedRound,
    inbox: &mut dyn RevealInbox,
    clock: &dyn Clock,
    save: &mut dyn FnMut(&ObservationResults) -> Result<(), String>,
) -> Result<usize, String> {
    if store.is_settled(&u.round.round_id) || u.round.log.is_some() {
        return Ok(0);
    }
    let (commit_close, reveal_close) = (u.round.cfg.commit_close, u.round.cfg.reveal_close);
    if clock.now() > reveal_close {
        return Ok(0);
    }
    // Review B5-R2-1: the reveals a book holds are not re-checked when it
    // loads, and each one holds its observer's slot here (first one only;
    // the intake stops once every drawn observer has one). Keep only those
    // that pass the arrival check again at their stamped time, so an entry
    // that is no reveal (a corrupted or edited book) never blocks the
    // observer's real one. No receipt exists yet (no log snapshot), and
    // `finish_reveals` would refuse such an entry anyway.
    let saved = std::mem::take(&mut u.reveals);
    let before = saved.len();
    for a in saved {
        u.receive_reveal(a.reveal, a.arrived);
    }
    if u.reveals.len() != before {
        store
            .keep_unfinished(u.clone())
            .map_err(|e| e.to_string())?;
        save(store).map_err(|e| format!("saving the checked reveals: {e}"))?;
    }
    clock.wait_until(commit_close.saturating_add(1));
    let mut taken = 0;
    let drawn: Vec<MinerAddress> = u.round.observers.iter().map(|o| o.address).collect();
    loop {
        let now = clock.now();
        for r in inbox.take(&u.round.round_id, &drawn, now) {
            if u.receive_reveal(r, now) {
                store
                    .keep_unfinished(u.clone())
                    .map_err(|e| e.to_string())?;
                save(store).map_err(|e| format!("saving a reveal before its receipt: {e}"))?;
                taken += 1;
            }
        }
        if now > reveal_close || u.reveals.len() >= u.round.observers.len() || !inbox.can_deliver()
        {
            return Ok(taken);
        }
        // Read once more AT `reveal_close`, so a reveal that reached the
        // inbox in time is never stamped late by the poll step.
        let next = if now < reveal_close {
            now.saturating_add(REVEAL_POLL_SECS).min(reveal_close)
        } else {
            match reveal_close.checked_add(1) {
                Some(t) => t,
                None => return Ok(taken),
            }
        };
        clock.wait_until(next);
    }
}

/// The coordinator's last check before it records or publishes a round
/// (review REVLOG HIGH-1): the chain's `transcriptComplete` for the round as
/// it would be served. False means the round leaves out a posted commit or a
/// receipted reveal; clients would refuse it, so the caller must not record
/// or publish it as is.
pub fn check_transcript_logged(
    round: &ObservationRound,
    chain: &dyn DrawChain,
    fact: &OnchainFact,
) -> Result<(), String> {
    let q = round.transcript_query(fact.registry_key());
    if chain.transcript_complete(&q)? {
        Ok(())
    } else {
        Err("transcriptComplete is false: the round leaves out a posted commit or a receipted reveal; not recorded".into())
    }
}

/// A recorded draw replayed (tests and the recorded rehearsals' replays): a
/// chain that holds one announced draw, as an earlier run on a real (local)
/// chain recorded it. Not a source of randomness: the beacon is the one that
/// run's chain gave.
#[derive(Clone, Copy, Debug)]
pub struct RecordedDraw {
    /// The recorded draw.
    pub record: DrawRecord,
    announced: bool,
}

impl RecordedDraw {
    /// Replay `record`.
    pub fn new(record: DrawRecord) -> Self {
        RecordedDraw {
            record,
            announced: false,
        }
    }
}

impl ChainView for RecordedDraw {
    fn height(&self) -> Result<u64, String> {
        Ok(if self.announced {
            self.record.beacon_height
        } else {
            self.record.beacon_height.saturating_sub(DRAW_LEAD)
        })
    }
    fn block_hash(&self, height: u64) -> Result<Option<Hash32>, String> {
        Ok((self.announced && height == self.record.beacon_height).then_some(self.record.beacon))
    }
}

impl DrawChain for RecordedDraw {
    fn draw_count(&self, _: &OnchainFact) -> Result<u32, String> {
        Ok(if self.announced {
            self.record.draw_count
        } else {
            self.record.attempt
        })
    }
    fn draw_budget(&self, _: &OnchainFact) -> Result<(u32, u32), String> {
        // The recorded run's draw was announced within the subject's bound.
        Ok((
            if self.announced {
                self.record.attempt + 1
            } else {
                self.record.attempt
            },
            MAX_SUBJECT_DRAWS.max(self.record.attempt + 1),
        ))
    }
    fn announce(&mut self, draw: &OnchainDraw) -> Result<(), String> {
        let r = &self.record;
        if draw.attempt != r.attempt
            || draw.commit != r.commit
            || draw.beacon_height != r.beacon_height
        {
            return Err("the draw is not the recorded one".into());
        }
        self.announced = true;
        Ok(())
    }
    fn fix_beacon(&mut self, _: &OnchainDraw) -> Result<DrawRecord, String> {
        if !self.announced {
            return Err("no draw announced".into());
        }
        Ok(self.record)
    }
    fn draw_of(&self, _: &OnchainFact, attempt: u32) -> Result<Option<DrawRecord>, String> {
        Ok((self.announced && attempt == self.record.attempt).then_some(self.record))
    }
    /// A replay is final as recorded.
    fn draw_of_final(
        &self,
        fact: &OnchainFact,
        attempt: u32,
    ) -> Result<(u64, Option<DrawRecord>), String> {
        Ok((self.height()?, self.draw_of(fact, attempt)?))
    }
    // A replay holds no transcript log (review REVLOG-R2 LOW-5: said here,
    // not by a default): posts go nowhere and every transcript is complete
    // on it. A replayed round is only ever checked by clients against the
    // chain that recorded it.
    fn post_commit(
        &mut self,
        _: &OnchainFact,
        _: u32,
        _: &MinerAddress,
        _: &Hash32,
        _: &Hash32,
        _: &ObserverSig,
    ) -> Result<Posted, String> {
        Ok(Posted::Now)
    }
    fn post_receipt(
        &mut self,
        _: &OnchainFact,
        _: u32,
        _: &MinerAddress,
        _: &Hash32,
        _: &Hash32,
        _: &ObserverSig,
    ) -> Result<Posted, String> {
        Ok(Posted::Now)
    }
    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        Ok(None)
    }
    fn posted_commit(
        &self,
        _: u64,
        _: &OnchainFact,
        _: u32,
        _: &MinerAddress,
        _: &Hash32,
    ) -> Result<Option<PostedCommit>, String> {
        Ok(None)
    }
    fn transcript_complete(&self, _: &TranscriptLogQuery) -> Result<bool, String> {
        Ok(true)
    }
}

/// An in-memory model of `OnyxRegistry`'s draw rules on a simulated chain
/// (tests): sequential attempts below [`MAX_DRAW_ATTEMPTS`], a beacon height
/// above the height at announcement, a beacon fixed once its block exists.
/// Each transaction mines one block; block `n` hashes to
/// keccak("onyx-simulated-block" ‖ n). Never a source of randomness. It
/// keeps the transcript log too (ADR-0027 amendment 8): posts land at block
/// time [`time`](Self::time).
#[derive(Clone, Debug)]
pub struct SimulatedRegistry {
    /// The chain height.
    pub height: u64,
    /// The block time (unix seconds) posts land at, and the time of the head
    /// block [`DrawChain::log_block`] reports (final here: no reorgs). Starts at `u64::MAX`: a chain far past every round, where
    /// every post lands outside the commit phase (as the rehearsals' recorded
    /// rounds on anvil); a test sets it to post inside a round's phase.
    pub time: u64,
    /// The receipt signer each announced draw snapshots
    /// ([`test_keys::receipt_signer`] by default).
    pub receipt_signer: [u8; 20],
    /// The transcript log, keyed by the registry fact key.
    pub log: SimulatedTranscriptLog,
    /// The block each commit slot was filled in (`transcript_log_key` ->
    /// block): a read at an older block sees the slot empty.
    pub commit_blocks: BTreeMap<Hash32, u64>,
    draws: BTreeMap<(Hash32, Hash32, u64), Vec<DrawRecord>>,
    /// (metric, subject) -> draws not ended with a recorded value.
    strikes: BTreeMap<(Hash32, Hash32), u32>,
    /// (metric, subject) -> owner reopens.
    reopens: BTreeMap<(Hash32, Hash32), u32>,
    /// What [`DrawChain::deployment`] answers (`None` by default).
    pub deployment: Option<RegistryDeployment>,
    /// The reveal key draws announced from now on snapshot (`setRevealKey`;
    /// `None` by default: draws take no sealed post), ROADMAP C11.
    pub reveal_key: Option<crate::sealed::Point>,
    /// Sealed posts, the draws' reveal keys and the attempt records, on the
    /// registry's rules ([`crate::sealed::SimulatedSealedLog`] over [`Self::log`]).
    pub sealed: crate::sealed::SimulatedSealedLog,
    /// The block each sealed slot was filled in: a read at an older block
    /// sees the slot empty.
    pub sealed_blocks: BTreeMap<Hash32, u64>,
    /// [`DrawChain::records_rounds`] (off by default): `recordAttempt` here
    /// lands at [`Self::time`].
    pub records: bool,
    /// `recordObservation`s taken (when [`Self::records`]): the recorded
    /// results, by (metric, subject, window end).
    pub observations: BTreeMap<(Hash32, Hash32, u64), OnchainObservationCheck>,
    /// Each draw's `drawTimeLocked`, in attempt order, keyed as the draws
    /// (ROADMAP C51; the mode itself is [`crate::sealed::SimulatedSealedLog::time_lock_required`]).
    draw_locks: BTreeMap<(Hash32, Hash32, u64), Vec<bool>>,
    /// What [`DrawChain::beacon`] answers (ROADMAP C51): published quicknet
    /// rounds from fixtures; `None` by default (time-locked rounds wait).
    pub beacon: Option<crate::timelock::FixtureBeacon>,
}

impl Default for SimulatedRegistry {
    fn default() -> Self {
        SimulatedRegistry {
            height: 0,
            time: u64::MAX,
            receipt_signer: test_keys::receipt_signer(),
            log: SimulatedTranscriptLog::default(),
            commit_blocks: BTreeMap::new(),
            draws: BTreeMap::new(),
            strikes: BTreeMap::new(),
            reopens: BTreeMap::new(),
            deployment: None,
            reveal_key: None,
            sealed: crate::sealed::SimulatedSealedLog::default(),
            sealed_blocks: BTreeMap::new(),
            records: false,
            observations: BTreeMap::new(),
            draw_locks: BTreeMap::new(),
            beacon: None,
        }
    }
}

impl SimulatedRegistry {
    /// An empty registry at height 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// The draw's beacon is fixed (`postCommit` needs it).
    fn beacon_fixed(&self, fact: &OnchainFact, attempt: u32) -> bool {
        self.draws
            .get(&Self::key(fact))
            .and_then(|l| l.get(usize::try_from(attempt).ok()?))
            .is_some_and(|r| r.beacon != Hash32([0; 32]))
    }

    fn key(f: &OnchainFact) -> (Hash32, Hash32, u64) {
        (
            f.metric_key,
            f.subject_key,
            if f.dated { 0 } else { f.window_end },
        )
    }

    /// `recordObservation`: the fact's latest draw ended with a value, so it
    /// stops counting against the subject (L2RND-R2-F1).
    pub fn record_value(&mut self, fact: &OnchainFact) {
        let s = self
            .strikes
            .entry((fact.metric_key, fact.subject_key))
            .or_insert(0);
        *s = s.saturating_sub(1);
    }

    /// `reopenDraws` (the registry owner, never the coordinator).
    pub fn reopen_draws(&mut self, fact: &OnchainFact) {
        *self
            .reopens
            .entry((fact.metric_key, fact.subject_key))
            .or_insert(0) += 1;
    }

    fn allowance(&self, fact: &OnchainFact) -> u32 {
        MAX_SUBJECT_DRAWS.saturating_mul(
            1 + self
                .reopens
                .get(&(fact.metric_key, fact.subject_key))
                .copied()
                .unwrap_or(0),
        )
    }

    /// The hash of simulated block `n`.
    pub fn hash_of(n: u64) -> Hash32 {
        keccak256(&[b"onyx-simulated-block".as_slice(), &n.to_be_bytes()].concat())
    }

    /// `requireTimeLock` (ROADMAP C51): from now on every draw is keyed and
    /// time-locked, and rounds of other draws are not recorded. One way.
    pub fn require_time_lock(&mut self) {
        self.sealed.time_lock_required = true;
    }

    /// `postSealedReveal` (ROADMAP C11) at block time [`Self::time`], in a
    /// block of its own: the registry's rules (the draw's beacon fixed and its
    /// reveal key, the observer's signature and key proof, one post per slot).
    pub fn post_sealed(
        &mut self,
        fact: &OnchainFact,
        attempt: u32,
        round_id: &Hash32,
        post: &crate::sealed::SealedPost,
    ) -> Result<(), String> {
        if !self.beacon_fixed(fact, attempt) {
            return Err("postSealedReveal: NoDraw".into());
        }
        let k = fact.registry_key();
        self.height += 1;
        self.sealed
            .post_sealed_with(&self.log, &k, attempt, round_id, post, self.time)
            .map_err(|e| format!("postSealedReveal: {e}"))?;
        self.sealed_blocks.insert(
            transcript_log_key(&k, attempt, &post.observer, round_id),
            self.height,
        );
        Ok(())
    }
}

impl ChainView for SimulatedRegistry {
    fn height(&self) -> Result<u64, String> {
        Ok(self.height)
    }
    fn block_hash(&self, height: u64) -> Result<Option<Hash32>, String> {
        Ok((height <= self.height).then(|| Self::hash_of(height)))
    }
}

impl DrawChain for SimulatedRegistry {
    fn deployment(&self) -> Result<Option<RegistryDeployment>, String> {
        Ok(self.deployment)
    }
    fn draw_count(&self, fact: &OnchainFact) -> Result<u32, String> {
        Ok(self
            .draws
            .get(&Self::key(fact))
            .map_or(0, |d| u32::try_from(d.len()).unwrap_or(u32::MAX)))
    }
    fn draw_budget(&self, fact: &OnchainFact) -> Result<(u32, u32), String> {
        Ok((
            self.strikes
                .get(&(fact.metric_key, fact.subject_key))
                .copied()
                .unwrap_or(0),
            self.allowance(fact),
        ))
    }
    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        if self.receipt_signer == [0; 20] {
            return Err("announceDraw: NoReceiptSigner".into());
        }
        let (strikes, allowance) = self.draw_budget(&d.fact)?;
        if strikes >= allowance {
            return Err("announceDraw: DrawsExhausted".into());
        }
        self.height += 1;
        let at = self.height;
        let list = self.draws.entry(Self::key(&d.fact)).or_default();
        if usize::try_from(d.attempt).ok() != Some(list.len()) || d.attempt >= MAX_DRAW_ATTEMPTS {
            return Err("announceDraw: BadAttempt".into());
        }
        if d.beacon_height <= at {
            return Err("announceDraw: BeaconNotFuture".into());
        }
        // ROADMAP C51 (amendment 14): with time-locks required, a draw needs a
        // reveal key and is time-locked.
        let locked = self.sealed.time_lock_required;
        if locked && self.reveal_key.is_none() {
            return Err("announceDraw: NoRevealKey".into());
        }
        self.draw_locks
            .entry(Self::key(&d.fact))
            .or_default()
            .push(locked);
        if locked {
            self.sealed
                .time_locked
                .insert((d.fact.registry_key(), d.attempt));
        }
        list.push(DrawRecord {
            chain_id: 31_337,
            registry: [0; 20],
            attempt: d.attempt,
            draw_count: 0,
            announced_at: at,
            commit: d.commit,
            beacon_height: d.beacon_height,
            beacon: Hash32([0; 32]),
        });
        *self
            .strikes
            .entry((d.fact.metric_key, d.fact.subject_key))
            .or_insert(0) += 1;
        self.log
            .signers
            .insert((d.fact.registry_key(), d.attempt), self.receipt_signer);
        if let Some(k) = self.reveal_key {
            self.sealed
                .keys
                .insert((d.fact.registry_key(), d.attempt), k);
        }
        Ok(())
    }
    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        self.height = self.height.max(d.beacon_height) + 1;
        let list = self
            .draws
            .get_mut(&Self::key(&d.fact))
            .ok_or("fixBeacon: NoDraw")?;
        let n = u32::try_from(list.len()).unwrap_or(u32::MAX);
        let r = list
            .get_mut(usize::try_from(d.attempt).unwrap_or(usize::MAX))
            .ok_or("fixBeacon: NoDraw")?;
        // As `OnyxRegistry.fixBeacon` (review REVLOG-R5): anyone may fix a
        // beacon, once; a second call reverts. `draw_and_open` reads the
        // draw before and after, so a stranger who went first is success
        // there (L2RND-R2-F4), not here.
        if r.beacon != Hash32([0; 32]) {
            return Err("fixBeacon: BeaconAlreadyFixed".into());
        }
        // As the EVM's `blockhash` (review R8): the beacon block's hash is
        // readable only from the next 256 blocks; later, `fixBeacon`
        // reverts and the beacon can never be fixed.
        if self.height > r.beacon_height.saturating_add(BLOCKHASH_WINDOW) {
            return Err("fixBeacon: BeaconUnavailable".into());
        }
        r.beacon = Self::hash_of(r.beacon_height);
        Ok(DrawRecord {
            draw_count: n,
            ..*r
        })
    }
    fn draw_of(&self, fact: &OnchainFact, attempt: u32) -> Result<Option<DrawRecord>, String> {
        let list = self.draws.get(&Self::key(fact));
        let n = list.map_or(0, |l| u32::try_from(l.len()).unwrap_or(u32::MAX));
        Ok(list
            .and_then(|l| l.get(usize::try_from(attempt).ok()?))
            .map(|r| DrawRecord {
                draw_count: n,
                ..*r
            }))
    }
    /// The head is final here (no reorgs).
    fn draw_of_final(
        &self,
        fact: &OnchainFact,
        attempt: u32,
    ) -> Result<(u64, Option<DrawRecord>), String> {
        Ok((self.height, self.draw_of(fact, attempt)?))
    }
    fn post_commit(
        &mut self,
        fact: &OnchainFact,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
    ) -> Result<Posted, String> {
        if !self.beacon_fixed(fact, attempt) {
            return Err("postCommit: NoDraw".into());
        }
        let k = fact.registry_key();
        if let Some(p) = self.log.posted(&k, attempt, observer, round_id) {
            return Ok(Posted::Before(p.commit));
        }
        self.height += 1;
        self.log
            .post_commit(&k, attempt, observer, round_id, commit, sig, self.time)
            .map_err(|e| format!("postCommit: {e}"))?;
        self.commit_blocks.insert(
            transcript_log_key(&k, attempt, observer, round_id),
            self.height,
        );
        Ok(Posted::Now)
    }
    fn post_receipt(
        &mut self,
        fact: &OnchainFact,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
    ) -> Result<Posted, String> {
        let k = fact.registry_key();
        if let Some(c) = self.log.receipt(&k, attempt, observer, round_id) {
            return Ok(Posted::Before(c));
        }
        self.height += 1;
        self.log
            .post_receipt(&k, attempt, observer, round_id, commit, sig)
            .map_err(|e| format!("postReceipt: {e}"))?;
        Ok(Posted::Now)
    }
    /// The head is final here (no reorgs), at block time [`Self::time`].
    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        Ok(Some(LogBlock {
            number: self.height,
            time: self.time,
        }))
    }
    fn posted_commit(
        &self,
        block: u64,
        fact: &OnchainFact,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
    ) -> Result<Option<PostedCommit>, String> {
        if block > self.height {
            return Err(format!(
                "header not found: block {block} is above the head {}",
                self.height
            ));
        }
        let k = fact.registry_key();
        let filled = self
            .commit_blocks
            .get(&transcript_log_key(&k, attempt, observer, round_id))
            .is_none_or(|b| *b <= block);
        Ok(self
            .log
            .posted(&k, attempt, observer, round_id)
            .filter(|_| filled))
    }
    fn transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String> {
        Ok(self.log.complete(q))
    }
    fn draw_receipt_signer(
        &self,
        fact: &OnchainFact,
        attempt: u32,
    ) -> Result<Option<[u8; 20]>, String> {
        Ok(self
            .log
            .signers
            .get(&(fact.registry_key(), attempt))
            .copied())
    }
    fn draw_reveal_key(
        &self,
        fact: &OnchainFact,
        attempt: u32,
    ) -> Result<Option<crate::sealed::Point>, String> {
        Ok(self
            .sealed
            .keys
            .get(&(fact.registry_key(), attempt))
            .copied())
    }
    fn sealed_post(
        &self,
        block: u64,
        fact: &OnchainFact,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
    ) -> Result<Option<PostedSealed>, String> {
        if block > self.height {
            return Err(format!(
                "header not found: block {block} is above the head {}",
                self.height
            ));
        }
        let k = fact.registry_key();
        let filled = self
            .sealed_blocks
            .get(&transcript_log_key(&k, attempt, observer, round_id))
            .is_none_or(|b| *b <= block);
        Ok(self
            .sealed
            .sealed_post(&k, attempt, observer, round_id)
            .filter(|_| filled)
            .cloned())
    }
    fn reveal_log_complete(&self, q: &crate::sealed::RevealLogQuery) -> Result<bool, String> {
        Ok(self.sealed.complete_with(&self.log, q))
    }
    fn draw_time_locked(&self, fact: &OnchainFact, attempt: u32) -> Result<bool, String> {
        Ok(self
            .sealed
            .time_locked
            .contains(&(fact.registry_key(), attempt)))
    }
    fn beacon(&self) -> Option<&dyn crate::timelock::BeaconSource> {
        self.beacon
            .as_ref()
            .map(|b| b as &dyn crate::timelock::BeaconSource)
    }
    fn records_rounds(&self) -> bool {
        self.records
    }
    fn record_attempt(
        &mut self,
        q: &crate::sealed::RevealLogQuery,
        result: &Hash32,
    ) -> Result<Recorded, String> {
        if self.sealed.attempt_matches(q, result) {
            return Ok(Recorded::Before);
        }
        self.height += 1;
        self.sealed
            .record_attempt_with(&self.log, q, result, self.time)
            .map_err(|e| format!("recordAttempt: {e}"))?;
        Ok(Recorded::Now)
    }
    /// One result per fact; the value's draw stops counting against the
    /// subject ([`Self::record_value`]).
    fn record_observation(&mut self, rec: &OnchainObservationCheck) -> Result<Recorded, String> {
        let k = (rec.metric_key, rec.subject_key, rec.window_end);
        match self.observations.get(&k) {
            Some(r) if r == rec => return Ok(Recorded::Before),
            Some(_) => return Err("recordObservation: AlreadyRecorded".into()),
            None => {}
        }
        // ROADMAP C51: once time-locks are required, only a time-locked latest
        // draw records (draws are keyed with window end 0 for a dated fact).
        if self.sealed.time_lock_required {
            let locks = self
                .draw_locks
                .get(&k)
                .or_else(|| self.draw_locks.get(&(rec.metric_key, rec.subject_key, 0)));
            if locks.and_then(|l| l.last()) != Some(&true) {
                return Err("recordObservation: NotTimeLocked".into());
            }
        }
        self.height += 1;
        let s = self
            .strikes
            .entry((rec.metric_key, rec.subject_key))
            .or_insert(0);
        *s = s.saturating_sub(1);
        self.observations.insert(k, *rec);
        Ok(Recorded::Now)
    }
}

/// Draw a round's observers and open it (L2-F2, L2RND-R1), the way every
/// runner does: align the store with the registry's draw count, announce the
/// draw in the store (counting the attempt), persist the book (`save`),
/// announce it on chain before its beacon height, fix the beacon on chain
/// once its block exists, open with the beacon the chain gives, and keep the
/// opened round in the store and save the book again.
///
/// THE DRAW INVARIANT (review R8, ADR-0027 amendment 8 item 8f): the book
/// is saved with the draw BEFORE `announceDraw` is sent, and from then on
/// the draw leaves the book only through
/// [`ObservationResults::release_draw`], under a proof: its round or failed
/// attempt durably published, or ONE final block showing it can never land
/// (`BeaconNotFuture`/`BadAttempt`) or never have its beacon fixed
/// (`BeaconUnavailable`). Every other outcome (an error, a stale read, a
/// mismatch, a crash, a timeout) leaves it in the book, announced or kept,
/// and returns an error: the next run resumes the SAME draw. A draw the
/// book holds announced and not opened is resumed here (no second
/// announcement). Opening is one step with keeping
/// ([`ObservationResults::open_drawn`]): the registry's record is checked
/// before the round is opened, so no draw is ever opened and not kept.
///
/// A chain draw the book lacks (a lost or older book, another book's) is
/// adopted when its commitment is this runner's, the results store (read
/// first, [`ObservationResults::note_published_draws`]) does not publish it,
/// and one final block shows it as the chain's last draw
/// ([`ObservationResults::adopt_draw`], review R9-F1); the latest one the
/// results store publishes is followed by a next draw; an earlier one is drawn past when
/// its beacon is fixed (settled history: a draw gets a successor only once
/// it is released, review R8-2) or can never be fixed; the latest is drawn
/// past only once it can never be fixed; otherwise the run stops. The beacon
/// fix is idempotent ([`fix_beacon_idempotent`]). Every chain read is
/// retried per `wait`.
pub fn draw_and_open(
    store: &mut ObservationResults,
    spec: ObservationSpec,
    cfg: ObserveConfig,
    members: &[Observer],
    chain: &mut dyn DrawChain,
    wait: &ChainWait,
    save: &mut dyn FnMut(&ObservationResults) -> Result<(), String>,
) -> Result<(ObservationRound, DrawRecord), String> {
    let fact = store.onchain_fact(&spec).map_err(|e| e.to_string())?;
    let mut onchain = wait.retry(|| chain.draw_count(&fact))?;
    // Review REVLOG-R5: a draw announced (on chain and in the saved book)
    // whose run stopped before the round opened is resumed on that draw.
    let resume = match store.pending_draw(&spec) {
        Some(d) if d.attempt.saturating_add(1) == onchain => Some(d),
        // Review R6-B: the count says the chain lacks the book's draw. One
        // `drawCount` answer (a lagging node behind a load balancer) never
        // drops it: it is confirmed by `drawOf`, and released only under a
        // final-block proof that it can never land.
        Some(d) if d.attempt >= onchain => match confirm_announced(chain, &d, wait) {
            Confirm::Landed => {
                onchain = d.attempt.saturating_add(1);
                Some(d)
            }
            Confirm::Gone(proof) => {
                release(store, &spec, proof, save)?;
                None
            }
            Confirm::Unknown(c) => {
                return Err(format!(
                    "the chain is behind the book: draw {} is in the book and {onchain} draws on chain ({c}); it stays in the book: run again with the same book",
                    d.attempt
                ))
            }
        },
        // Review R7-2: the chain holds draws after the book's pending draw
        // (another book announced them). The book's draw is not the latest,
        // so it can never be resumed; it leaves the book only under a proof
        // (it can never land, or never be fixed), then the later draws are
        // followed as for a book that lacks them (review R8-2: the latest
        // may be adopted).
        Some(d) => {
            let proof = gone_proof(chain, &d, wait).map_err(|why| {
                format!(
                    "the chain holds {onchain} draws, past the book's draw {} ({why}); it stays in the book: restore the book that announced the later draws",
                    d.attempt
                )
            })?;
            release(store, &spec, proof, save)?;
            follow_chain(store, &spec, &cfg, members, chain, &fact, onchain, wait, save)?
        }
        None => follow_chain(store, &spec, &cfg, members, chain, &fact, onchain, wait, save)?,
    };
    store
        .sync_draws(&spec, onchain)
        .map_err(|e| format!("sync draws: {e}"))?;
    // L2RND-R2-F1: the subject's bound lives on chain; refuse before
    // announcing rather than let the registry revert.
    let (strikes, allowance) = wait.retry(|| chain.draw_budget(&fact))?;
    let (draw, snapshot) = match resume {
        Some(draw) => {
            // The resumed draw is one of the chain's strikes and already
            // passed the bound when it was announced.
            store
                .sync_budget(&spec, strikes.saturating_sub(1), allowance)
                .map_err(|e| format!("sync budget: {e}"))?;
            let snapshot = store
                .announced_pool(&spec)
                .map_err(|e| format!("resume: {e}"))?;
            (draw, snapshot)
        }
        None => {
            store
                .sync_budget(&spec, strikes, allowance)
                .map_err(|e| format!("sync budget: {e}"))?;
            if strikes >= allowance {
                return Err(format!(
                    "DrawsExhausted: the subject holds {strikes} of {allowance} draws without a recorded value; the registry owner must reopen it"
                ));
            }
            let attempt = store
                .next_attempt(&spec)
                .map_err(|e| format!("announce: {e}"))?;
            if attempt != onchain {
                return Err(format!(
                    "the book is ahead of the chain: attempt {attempt} but {onchain} draws on chain"
                ));
            }
            let lead = wait.retry(|| chain.draw_lead())?;
            let height = wait.retry(|| chain.height())?.saturating_add(lead);
            let snapshot = store
                .announce_draw(&spec, &cfg, members, height, &*chain)
                .map_err(|e| format!("announce: {e}"))?;
            let draw = store.latest_draw(&spec).map_err(|e| e.to_string())?;
            // Review R7-2 (THREATS REVLOG-R5-2): the book holds the draw on
            // disk BEFORE `announceDraw` is sent, so no crash leaves a draw
            // on chain that the book lacks. A failed save sends nothing.
            if let Err(s) = save(store) {
                let back = store
                    .release_draw(&spec, Release::NeverSent)
                    .map_err(|e| format!("; {e}"))
                    .err()
                    .unwrap_or_default();
                return Err(format!(
                    "saving the book with draw {} before announcing it: {s}; nothing was sent{back}",
                    draw.attempt
                ));
            }
            // L2RND-R2-F5, review R6-A: an announcement error does not mean
            // the announcement did not land (a receipt timeout after the
            // send). The draw stays in the book until the chain shows it, or
            // a final block proves it can never land.
            if let Err(e) = chain.announce(&draw) {
                match confirm_announced(chain, &draw, wait) {
                    Confirm::Landed => {}
                    Confirm::Gone(proof) => {
                        release(store, &spec, proof, save)?;
                        return Err(format!(
                            "announce: {e}; the chain shows draw {} never landed: forgotten",
                            draw.attempt
                        ));
                    }
                    Confirm::Unknown(c) => {
                        return Err(format!(
                            "announce: {e}; draw {} is not confirmed on chain yet ({c}); it stays in the book: run again with the same book (it resumes the draw if the chain holds it, forgets it once a final block past its beacon height shows it absent)",
                            draw.attempt
                        ))
                    }
                }
            }
            (draw, snapshot)
        }
    };
    let rec = match fix_beacon_idempotent(chain, &draw, wait) {
        Ok(r) => r,
        Err(e) => return Err(beacon_not_fixed(store, &spec, chain, &draw, wait, save, e)),
    };
    // Review R8-1: opened and kept in one step, after the registry's record
    // is checked; a mismatch leaves the draw announced in the book.
    let round = open_retrying(store, &spec, cfg, snapshot, &*chain, &draw, rec, wait)
        .map_err(|e| format!("open: {e}; the draw stays announced in the book: run again with the same book to open it on the same draw"))?;
    let rec = store
        .unfinished(&spec)
        .map(|u| u.onchain_draw)
        .ok_or("the opened round is not kept")?;
    // THREATS REVLOG-R4-5: the opened round is in the book from here on, so
    // a crash in its commit phase finishes it on this draw.
    save(store).map_err(|e| {
        format!("saving the opened round: {e}; it is kept in the store: finish it on the same draw")
    })?;
    Ok((round, rec))
}

/// Release the book's draw of `spec` under `why`
/// ([`ObservationResults::release_draw`]) and save the book.
fn release(
    store: &mut ObservationResults,
    spec: &ObservationSpec,
    why: Release,
    save: &mut dyn FnMut(&ObservationResults) -> Result<(), String>,
) -> Result<(), String> {
    store
        .release_draw(spec, why)
        .map_err(|e| format!("releasing the book's draw: {e}"))?;
    save(store)
}

/// Review R11-3: a runner read no results store file because the operator
/// passed `--new-store` (a fact's first run on a deployment). Refused when
/// the chain already shows draws of `spec`'s fact and the book holds none:
/// that is not a first run but a lost book with a missing or mistyped
/// store path (`--new-store` left on), which would read as "nothing
/// published" and could adopt a draw whose round the real store publishes
/// (REVLOG-R9-F1). A book that holds the chain's latest draw (announced,
/// opened or kept: a first run that stopped before its store was written)
/// goes on.
///
/// Review R12 (REVLOG-R12-2): the book must hold the chain's LATEST draw
/// `n - 1` (or `n`, saved before its announcement landed): a book holding
/// an earlier draw while the chain shows more is not this fact's first run
/// either. The count is the larger of the latest block's and a final
/// block's ([`draw_count_seen`]): a lagging node answering a low count at
/// `latest` cannot let the run through, since a final block's state must
/// be served as of that block or the read fails.
pub fn new_store_allowed(
    store: &ObservationResults,
    spec: &ObservationSpec,
    chain: &dyn DrawChain,
    wait: &ChainWait,
) -> Result<(), String> {
    let fact = store.onchain_fact(spec).map_err(|e| e.to_string())?;
    let n = draw_count_seen(chain, &fact, wait)?;
    let held = store.held_draw(spec).map(|d| d.attempt);
    if n > 0 && !held.is_some_and(|a| a + 1 >= n && a <= n) {
        return Err(format!(
            "--new-store: there is no results store file, but the chain already shows {n} draw(s) of this fact and the book holds {}: this is not the fact's first run on this deployment. Check the --store path; pass --new-store only on a fact's first run (review R11-3, R12)",
            held.map_or("none".to_string(), |a| format!("draw {a}, not the chain's latest"))
        ));
    }
    Ok(())
}

/// Review R12: `drawCount` of `fact` read twice, at `latest` (retried) and
/// AT a final block ([`DrawChain::draw_of_final`] of attempt 0, whose
/// record carries the count as of that block; no draw 0 there is a count
/// of 0), and the larger taken. A node behind the head can under-report the
/// latest count; the final read names its block, so a node without that
/// block's state errors instead of answering an older count.
pub fn draw_count_seen(
    chain: &dyn DrawChain,
    fact: &OnchainFact,
    wait: &ChainWait,
) -> Result<u32, String> {
    let latest = wait.retry(|| chain.draw_count(fact))?;
    let (_, first) = wait.retry(|| chain.draw_of_final(fact, 0))?;
    Ok(latest.max(first.map_or(0, |r| r.draw_count)))
}

/// The chain holds `onchain` draws and the book holds none it can resume:
/// the draws past the book's count (a lost or older book, or another
/// book's). Review R8-2: an earlier one is drawn past when its beacon is
/// fixed (settled history: a draw gets a successor only once released, so
/// its round is published) or can never be fixed; the latest is followed
/// (a next draw) when the results store publishes it (review R9-F1), else
/// adopted when its commitment is this runner's (THREATS REVLOG-R5-2) and
/// one final block shows it as the chain's last draw, else drawn past only
/// once it can never be fixed. Otherwise Err: the run stops
/// (drawing past a fixable draw leaves it for anyone to fix, and a fixed
/// beacon without a published round voids the fact). `Some(draw)`: adopted,
/// to resume.
#[allow(clippy::too_many_arguments)]
fn follow_chain(
    store: &mut ObservationResults,
    spec: &ObservationSpec,
    cfg: &ObserveConfig,
    members: &[Observer],
    chain: &dyn DrawChain,
    fact: &OnchainFact,
    onchain: u32,
    wait: &ChainWait,
    save: &mut dyn FnMut(&ObservationResults) -> Result<(), String>,
) -> Result<Option<OnchainDraw>, String> {
    let Ok(known) = store.next_attempt(spec) else {
        return Ok(None);
    };
    if known >= onchain {
        return Ok(None);
    }
    let last = onchain - 1;
    for a in known..last {
        draw_past(chain, fact, a, wait, Past::Earlier)?;
    }
    // `drawCount` showed the draw: a lagging `drawOf` never lets the run
    // draw past it.
    let Some(rec) = wait.retry(|| chain.draw_of(fact, last))? else {
        return Err(format!(
            "drawCount shows {onchain} draws but drawOf shows no draw at attempt {last}: run again with the same book"
        ));
    };
    store
        .sync_draws(spec, last)
        .map_err(|e| format!("sync draws: {e}"))?;
    // Review R9-F1: the results store publishes the latest draw (a book
    // this one lost published its round, then was lost): it is settled
    // history, released when it was published, and the next draw follows
    // it. Adopting it would open a second, different round on a published
    // draw, which the results store refuses for good.
    //
    // Review R10-1: matched by beacon height AND beacon. A results store of
    // another deployment (fresh chains give the same heights) would
    // otherwise make the run draw past this chain's latest draw, unfixed and
    // fixable by anyone. A published draw has a fixed beacon, so the
    // chain's must be that same, nonzero beacon. (Runners also refuse a
    // results store tagged with another deployment, `DrawChain::deployment`.)
    //
    // Review R11-4: a node lagging behind the `fixBeacon` shows the draw
    // with a zero beacon: a draw that differs is read again (`wait.retry`)
    // before the run stops, and the message names both causes.
    if let Some(Some(p)) = store.published_draw(spec, last) {
        let same = |r: &DrawRecord| {
            p.beacon_height == r.beacon_height && p.beacon != Hash32::ZERO && p.beacon == r.beacon
        };
        if !same(&rec) {
            let mut seen = rec;
            let again = if p.beacon == Hash32::ZERO {
                Err("a published draw without a beacon".to_string())
            } else {
                wait.retry(|| match chain.draw_of(fact, last)? {
                    Some(r) if same(&r) => Ok(()),
                    Some(r) => {
                        seen = r;
                        Err("differs".into())
                    }
                    None => Err(format!("drawOf shows no draw at attempt {last}")),
                })
            };
            if again.is_err() {
                return Err(format!(
                    "the results store publishes draw {last} with beacon height {} and beacon {}, the chain's draw {last} still reads {} and {} ({}, review R11-4): a zero beacon is a node that has not seen fixBeacon yet (run again later, or with another node); any other is the results store of another registry (run again with this registry's results store)",
                    p.beacon_height,
                    p.beacon,
                    seen.beacon_height,
                    seen.beacon,
                    again.err().unwrap_or_default()
                ));
            }
        }
        return Ok(None);
    }
    if store
        .adoptable(spec, cfg, members, &rec)
        .map_err(|e| format!("adopt: {e}"))?
    {
        // Review R9-F1: adopted only when ONE final block shows it as the
        // chain's last draw (`drawOf` and `drawCount` of that block, the
        // count equal to the latest one read), read again on an
        // inconsistent answer: a stale or mixed read never makes an earlier
        // draw (released, so its round is published) look like the latest.
        let fin = wait
            .retry(|| match chain.draw_of_final(fact, last)? {
                (_, Some(r))
                    if r.attempt == last
                        && r.commit == rec.commit
                        && r.beacon_height == rec.beacon_height
                        && r.draw_count == onchain =>
                {
                    Ok(r)
                }
                (b, r) => Err(format!(
                    "final block {b} shows {} at attempt {last}",
                    r.map_or("no draw".into(), |r| format!(
                        "a draw with drawCount {}",
                        r.draw_count
                    ))
                )),
            })
            .map_err(|e| {
                format!(
                    "the chain's latest draw {last} was announced with this runner's inputs; it is adopted only once one final block shows it as the chain's last of {onchain} draws ({e}): run again with the same book"
                )
            })?;
        if store
            .adopt_draw(spec, cfg, members, &fin)
            .map_err(|e| format!("adopt: {e}"))?
        {
            save(store)?;
            return Ok(Some(store.latest_draw(spec).map_err(|e| e.to_string())?));
        }
    }
    draw_past(chain, fact, last, wait, Past::Latest)?;
    Ok(None)
}

/// [`ObservationResults::open_drawn`] on the registry's record `rec`, with a
/// chain error (the beacon's block hash read) retried per `wait`, and a
/// record that does not match the round (a stale or mixed read) read again
/// (`drawOf`, one block) before the next try.
#[allow(clippy::too_many_arguments)]
fn open_retrying(
    store: &mut ObservationResults,
    spec: &ObservationSpec,
    cfg: ObserveConfig,
    snapshot: Vec<Observer>,
    chain: &dyn DrawChain,
    draw: &OnchainDraw,
    mut rec: DrawRecord,
    wait: &ChainWait,
) -> Result<ObservationRound, ObserveError> {
    let mut last = ObserveError::Chain("no try".into());
    for i in 0..wait.tries.max(1) {
        if i > 0 {
            std::thread::sleep(wait.pause);
        }
        match store.open_drawn(spec.clone(), cfg, snapshot.clone(), chain, &rec) {
            Err(ObserveError::Chain(e)) => last = ObserveError::Chain(e),
            Err(e @ ObserveError::RegistryDrawMismatch(_)) => {
                last = e;
                if let Ok(Some(r)) = chain.draw_of(&draw.fact, draw.attempt) {
                    if is_announced(&r, draw) && r.beacon != Hash32::ZERO {
                        rec = r;
                    }
                }
            }
            other => return other,
        }
    }
    Err(last)
}

/// What [`confirm_announced`] found for the book's announced draw.
enum Confirm {
    /// The registry holds it (attempt, commitment, beacon height).
    Landed,
    /// A final block proves it can never land: release it.
    Gone(Release),
    /// Neither, after `wait`'s polls: the draw stays in the book.
    Unknown(String),
}

/// Whether the registry holds `draw`, the book's announced draw (reviews
/// R6-A, R6-B): [`Confirm::Landed`] when it does (`drawOf`, retried per
/// `wait`, at the latest block, read again on every poll so a landed draw's
/// beacon is fixed in time (review R7-1), or at a final block),
/// [`Confirm::Gone`] when ONE final block ([`DrawChain::draw_of_final`])
/// proves it never will: another draw at its attempt (`BadAttempt` for
/// ever), or none at or past its beacon height (`announceDraw` reverts
/// `BeaconNotFuture` from there on). [`Confirm::Unknown`] otherwise: the
/// caller keeps the draw in the book.
fn confirm_announced(chain: &dyn DrawChain, draw: &OnchainDraw, wait: &ChainWait) -> Confirm {
    let mut last = String::from("no read");
    for i in 0..wait.polls.max(1) {
        if i > 0 {
            std::thread::sleep(wait.pause);
        }
        // Review R7-1: the latest block is read on EVERY poll, and a
        // matching draw there is acted on at once: its beacon must be fixed
        // within 256 blocks of its beacon height, and a final block can lag
        // the head by more (about 500 blocks on Base Sepolia). Only
        // forgetting the draw waits for a final block.
        if let Ok(Some(r)) = wait.retry(|| chain.draw_of(&draw.fact, draw.attempt)) {
            if is_announced(&r, draw) {
                return Confirm::Landed;
            }
        }
        match final_read(chain, draw, wait) {
            Ok(r) => match r.record {
                Some(x) if is_announced(&x, draw) => return Confirm::Landed,
                Some(_) => return Confirm::Gone(Release::NeverLands(r)),
                None if r.block >= draw.beacon_height => {
                    return Confirm::Gone(Release::NeverLands(r))
                }
                None => {
                    last = format!(
                        "no draw at attempt {} as of final block {}, below its beacon height {}",
                        draw.attempt, r.block, draw.beacon_height
                    );
                }
            },
            Err(e) => last = e,
        }
    }
    Confirm::Unknown(last)
}

/// The registry holds `draw` as announced: the same attempt, commitment and
/// beacon height.
fn is_announced(r: &DrawRecord, draw: &OnchainDraw) -> bool {
    r.attempt == draw.attempt && r.commit == draw.commit && r.beacon_height == draw.beacon_height
}

/// ONE read of a draw at ONE final block ([`DrawChain::draw_of_final`]):
/// the block and what it shows at the attempt. The only material of a
/// chain release proof (review R8): both values come from the same block,
/// and a final block is never reorged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FinalRead {
    block: u64,
    record: Option<DrawRecord>,
}

/// Why the book's draw may leave it ([`ObservationResults::release_draw`],
/// the draw invariant, review R8). Private: a chain proof is built only from
/// a [`FinalRead`] here; [`Release::Published`] only by
/// [`ObservationResults::published`] and
/// [`ObservationResults::attempt_published`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Release {
    /// The round (settled here) or failed attempt with this id is durably
    /// written.
    Published(Hash32),
    /// The final block shows another draw at its attempt, or none at or past
    /// its beacon height.
    NeverLands(FinalRead),
    /// The final block, past its beacon height plus the `blockhash` window,
    /// shows it with a zero beacon.
    Unfixable(FinalRead),
    /// The save that would have held it failed, and nothing was sent.
    NeverSent,
}

/// `draw`'s attempt at one final block, retried per `wait`.
fn final_read(
    chain: &dyn DrawChain,
    draw: &OnchainDraw,
    wait: &ChainWait,
) -> Result<FinalRead, String> {
    wait.retry(|| chain.draw_of_final(&draw.fact, draw.attempt))
        .map(|(block, record)| FinalRead { block, record })
}

/// A proof, from ONE final block, that the book's `draw` can never become a
/// fixed beacon without a published round: it can never land
/// ([`Release::NeverLands`]) or never have its beacon fixed
/// ([`Release::Unfixable`]). Err (why not) otherwise: the draw stays.
fn gone_proof(
    chain: &dyn DrawChain,
    draw: &OnchainDraw,
    wait: &ChainWait,
) -> Result<Release, String> {
    let r = final_read(chain, draw, wait).map_err(|e| format!("no final read: {e}"))?;
    let window_end = draw.beacon_height.saturating_add(BLOCKHASH_WINDOW);
    match r.record {
        Some(x) if is_announced(&x, draw) => {
            if x.beacon != Hash32::ZERO {
                Err(format!(
                    "final block {} shows draw {} with its beacon fixed",
                    r.block, draw.attempt
                ))
            } else if r.block > window_end {
                Ok(Release::Unfixable(r))
            } else {
                Err(format!(
                    "final block {} shows draw {} with its beacon zero, still fixable until block {window_end}",
                    r.block, draw.attempt
                ))
            }
        }
        Some(_) => Ok(Release::NeverLands(r)),
        None if r.block >= draw.beacon_height => Ok(Release::NeverLands(r)),
        None => Err(format!(
            "final block {} is below draw {}'s beacon height {} and does not show it yet",
            r.block, draw.attempt, draw.beacon_height
        )),
    }
}

/// Which chain draw the book lacks [`draw_past`] judges.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Past {
    /// Below the chain's latest draw: it has a successor.
    Earlier,
    /// The chain's latest draw.
    Latest,
}

/// Review R7-2, R8-2: Ok when the chain's draw at `attempt`, one the book
/// lacks and cannot resume or adopt, may be drawn past, from ONE final
/// block ([`DrawChain::draw_of_final`]): its beacon zero past its beacon
/// height plus the 256-block `blockhash` window (`fixBeacon` reverts
/// `BeaconUnavailable` from there on), or, for an [`Past::Earlier`] draw,
/// its beacon fixed: a draw gets a successor only once it is released (its
/// round published), so a fixed earlier draw is settled history, never one
/// this book could still publish. Err (the run stops, nothing is announced)
/// otherwise: a zero, still fixable beacon; the latest draw with a fixed
/// beacon; or no final read.
fn draw_past(
    chain: &dyn DrawChain,
    fact: &OnchainFact,
    attempt: u32,
    wait: &ChainWait,
    which: Past,
) -> Result<(), String> {
    let stop = |why: String| {
        format!(
            "the chain holds draw {attempt}, which the book cannot resume or adopt ({why}): restore the book this coordinator saved, or run with the inputs it announced (pool, spec, config); a draw the book lacks is drawn past only once its beacon can never be fixed (zero at a final block past its beacon height + {BLOCKHASH_WINDOW}), or when a later draw follows it with its beacon fixed"
        )
    };
    match wait.retry(|| chain.draw_of_final(fact, attempt)) {
        Ok((h, Some(r)))
            if r.beacon == Hash32::ZERO && h > r.beacon_height.saturating_add(BLOCKHASH_WINDOW) =>
        {
            Ok(())
        }
        Ok((_, Some(r))) if r.beacon != Hash32::ZERO && which == Past::Earlier => Ok(()),
        Ok((_, Some(r))) if r.beacon != Hash32::ZERO => Err(stop("its beacon is fixed".into())),
        Ok((h, Some(r))) => Err(stop(format!(
            "its beacon is zero at final block {h}, still fixable until block {}",
            r.beacon_height.saturating_add(BLOCKHASH_WINDOW)
        ))),
        Ok((h, None)) => Err(stop(format!("final block {h} does not show it yet"))),
        Err(e) => Err(stop(format!("no final read: {e}"))),
    }
}

/// Fix `draw`'s beacon, idempotently (review REVLOG-R5; L2RND-R2-F4):
/// `fixBeacon` is public and fixes a beacon once, so the registry is read
/// first ([`DrawChain::draw_of`], retried per `wait`, review R8-3: a stale
/// read is retried, not taken as the answer) and a beacon already fixed is
/// success whoever fixed it; after the send, and after ANY send error (a
/// stranger's `fixBeacon` landing first makes it revert
/// `BeaconAlreadyFixed`, or a read after the coordinator's own send failed),
/// the draw is read again, with `wait`'s retries, and a fixed beacon on the
/// announced draw (attempt, commitment, beacon height) is success. Err only
/// when the chain shows the beacon unfixed (or cannot be read) after the
/// retries.
pub fn fix_beacon_idempotent(
    chain: &mut dyn DrawChain,
    draw: &OnchainDraw,
    wait: &ChainWait,
) -> Result<DrawRecord, String> {
    let fixed = |r: Option<DrawRecord>| -> Result<Option<DrawRecord>, String> {
        match r {
            None => Err(format!("drawOf: no draw at attempt {}", draw.attempt)),
            Some(r) if !is_announced(&r, draw) => Err(format!(
                "drawOf: the registry's draw at attempt {} is not the one announced",
                draw.attempt
            )),
            Some(r) => Ok((r.beacon != Hash32::ZERO).then_some(r)),
        }
    };
    if let Some(r) = wait.retry(|| fixed(chain.draw_of(&draw.fact, draw.attempt)?))? {
        return Ok(r);
    }
    let sent = match chain.fix_beacon(draw) {
        Ok(r) if is_announced(&r, draw) && r.beacon != Hash32::ZERO => return Ok(r),
        Ok(_) => "fixBeacon read back another draw".to_string(),
        Err(e) => e,
    };
    wait.retry(|| {
        fixed(chain.draw_of(&draw.fact, draw.attempt)?)?
            .ok_or_else(|| format!("the beacon is not fixed on chain ({sent})"))
    })
}

/// `fix_beacon_idempotent` failed with `e`: the announced draw stays in the
/// book (saved), to be resumed by the next run on the same draw (review
/// REVLOG-R5), unless ONE final block proves it can never become a fixed
/// beacon without a published round ([`gone_proof`], review R6-D): its
/// beacon block left the chain's `blockhash` window with the beacon unfixed
/// (no committee exists for it, clients accept a draw without a beacon), or
/// it can never land. Then it is released
/// ([`ObservationResults::release_draw`]).
fn beacon_not_fixed(
    store: &mut ObservationResults,
    spec: &ObservationSpec,
    chain: &dyn DrawChain,
    draw: &OnchainDraw,
    wait: &ChainWait,
    save: &mut dyn FnMut(&ObservationResults) -> Result<(), String>,
    e: String,
) -> String {
    match gone_proof(chain, draw, wait) {
        Ok(proof) => match release(store, spec, proof, save) {
            Ok(()) => format!("fix beacon: {e}; draw {} can never have its beacon fixed (a final block shows it unfixable or never landed): it is given up (no beacon, no committee); the next run draws again", draw.attempt),
            Err(a) => format!("fix beacon: {e}; giving the draw up failed: {a}; it stays in the book"),
        },
        Err(_) => format!(
            "fix beacon: {e}; draw {} stays announced in the book: run again with the same book to fix its beacon and open it on the same draw",
            draw.attempt
        ),
    }
}

/// The public inputs of a round's observer draw (L2-F2, ADR-0027 amendment 6).
/// The coordinator never picks observers: they are the `size` pool members
/// with the smallest draw keys, where every key hashes the feed, the pool
/// snapshot and a beacon announced before it existed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DrawSeed {
    /// [`pool_hash`] of the snapshot the observers are drawn from.
    pub pool: Hash32,
    /// The beacon's height, announced before that block existed
    /// ([`ObservationResults::announce_draw`]).
    pub beacon_height: u64,
    /// The beacon: the deployment chain's block hash at `beacon_height`.
    pub beacon: Hash32,
    /// 0 for a fact's first draw, +1 for each draw of the fact that ended
    /// without a value (a fresh announcement and beacon each time).
    pub attempt: u32,
}

impl DrawSeed {
    /// The seed of a draw from `pool` with `beacon` at `beacon_height`.
    pub fn for_pool(pool: &[Observer], beacon_height: u64, beacon: Hash32, attempt: u32) -> Self {
        DrawSeed {
            pool: pool_hash(pool),
            beacon_height,
            beacon,
            attempt,
        }
    }
}

/// `keccak256("onyx-observer-pool-v1" ‖ borsh(pool))`.
pub fn pool_hash(pool: &[Observer]) -> Hash32 {
    let mut b = b"onyx-observer-pool-v1".to_vec();
    b.extend_from_slice(&borsh::to_vec(pool).unwrap_or_default());
    keccak256(&b)
}

/// `keccak256("onyx-observe-cfg-v1" ‖ borsh(cfg))`: the whole round config,
/// so `outlier_slash` (the draw's `min_bond`) is fixed before the beacon
/// (L2RND-R3-F2).
pub fn cfg_hash(cfg: &ObserveConfig) -> Hash32 {
    let mut b = b"onyx-observe-cfg-v1".to_vec();
    b.extend_from_slice(&borsh::to_vec(cfg).unwrap_or_default());
    keccak256(&b)
}

/// `keccak256("onyx-draw-commit-v1" ‖ pool ‖ spec_id ‖ cfg_hash)`, the
/// packing of `OnyxRegistry.drawCommit` (ADR-0011 style). `announceDraw`
/// stores it before the beacon exists and `recordObservation` refuses any
/// other (pool, spec, config): one announced draw reaches one committee
/// (L2RND-R3-F1/F2). The spec id covers the window, which a dated fact key
/// does not.
pub fn draw_commit(pool: &Hash32, spec_id: &Hash32, cfg_hash: &Hash32) -> Hash32 {
    let mut b = b"onyx-draw-commit-v1".to_vec();
    b.extend_from_slice(&pool.0);
    b.extend_from_slice(&spec_id.0);
    b.extend_from_slice(&cfg_hash.0);
    keccak256(&b)
}

/// `keccak256("onyx-observe-draw-v1" ‖ borsh(seed))`.
pub fn draw_hash(seed: &DrawSeed) -> Hash32 {
    let mut b = b"onyx-observe-draw-v1".to_vec();
    b.extend_from_slice(&borsh::to_vec(seed).unwrap_or_default());
    keccak256(&b)
}

/// A pool snapshot a draw may use: 1..=[`MAX_POOL`] members in strictly
/// ascending (so distinct) address order.
pub fn check_pool(pool: &[Observer]) -> Result<(), ObserveError> {
    if pool.is_empty()
        || pool.len() > MAX_POOL
        || !pool.windows(2).all(|w| w[0].address < w[1].address)
    {
        return Err(ObserveError::BadPool);
    }
    Ok(())
}

/// The observers of a round (pure, public: every consumer recomputes it).
/// Members whose snapshot bond covers `min_bond` are eligible; each gets the
/// key `keccak256("onyx-observe-draw-v1" ‖ spec_id ‖ draw_hash(seed) ‖
/// address)`; the `size` smallest keys are drawn and returned ascending by
/// address. Empty when fewer than `size` members are eligible.
pub fn draw_observers(
    spec_id: &Hash32,
    pool: &[Observer],
    seed: &DrawSeed,
    size: usize,
    min_bond: u128,
) -> Vec<Observer> {
    let dh = draw_hash(seed);
    let mut keyed: Vec<(Hash32, Observer)> = pool
        .iter()
        .filter(|o| o.bond >= min_bond)
        .map(|o| {
            let mut b = b"onyx-observe-draw-v1".to_vec();
            b.extend_from_slice(&spec_id.0);
            b.extend_from_slice(&dh.0);
            b.extend_from_slice(&o.address);
            (keccak256(&b), *o)
        })
        .collect();
    if size == 0 || keyed.len() < size {
        return Vec::new();
    }
    keyed.sort_unstable_by_key(|a| a.0);
    let mut drawn: Vec<Observer> = keyed.into_iter().take(size).map(|(_, o)| o).collect();
    drawn.sort_by_key(|o| o.address);
    drawn
}

/// Every check a consumer applies to a served round's draw: the pool is a
/// valid snapshot named by `seed.pool`, the attempt is within
/// [`MAX_DRAW_ATTEMPTS`], and `observers` are exactly the draw of
/// `spec.min_observers` members. The beacon itself is the coordinator's word
/// unless the consumer compares it with the chain's block hash at
/// `seed.beacon_height` (THREATS L2-F2).
pub fn check_draw(
    spec: &ObservationSpec,
    cfg: &ObserveConfig,
    pool: &[Observer],
    seed: &DrawSeed,
    observers: &[Observer],
) -> Result<(), ObserveError> {
    check_pool(pool)?;
    if pool_hash(pool) != seed.pool || seed.attempt >= MAX_DRAW_ATTEMPTS {
        return Err(ObserveError::BadPool);
    }
    let drawn = draw_observers(
        &spec.id(),
        pool,
        seed,
        usize::from(spec.min_observers),
        cfg.outlier_slash,
    );
    if drawn.is_empty() {
        return Err(ObserveError::TooFewObservers {
            have: pool.iter().filter(|o| o.bond >= cfg.outlier_slash).count(),
            need: spec.min_observers,
        });
    }
    if drawn != observers {
        return Err(ObserveError::NotTheDraw);
    }
    Ok(())
}

/// Check a draw's beacon against the chain (L2RND-R1-F1): it must be the
/// block hash at `seed.beacon_height`. A consumer runs this with its own
/// chain reader, never the coordinator's; on chain, `OnyxRegistry.drawMatches`
/// checks the same against the beacon `fixBeacon` read with `blockhash`.
pub fn check_beacon(seed: &DrawSeed, chain: &dyn ChainView) -> Result<(), ObserveError> {
    match chain
        .block_hash(seed.beacon_height)
        .map_err(ObserveError::Chain)?
    {
        None => Err(ObserveError::BeaconNotYet),
        Some(h) if h == seed.beacon => Ok(()),
        Some(_) => Err(ObserveError::BeaconMismatch),
    }
}

/// The rule an observer applies before it commits (ATT-R1-F4): the
/// registry's `drawOf(fact, attempt)`, read by the observer from its own
/// chain reader, must show this round's draw (`draw_commit`, beacon height,
/// announced before it) with its beacon FIXED and equal to the round's.
/// A client reads a draw on chain without a fixed beacon as one that never
/// had a committee (`client::verify_attempts_onchain`); that holds only
/// because honest observers refuse to commit to such a draw. Without this
/// rule a coordinator could compute the committee from the block hash, run
/// the round, and never call `fixBeacon` when it disliked the outcome.
pub fn check_commit_allowed(
    seed: &DrawSeed,
    draw_commit: &Hash32,
    draw_of: &DrawRecord,
) -> Result<(), ObserveError> {
    if draw_of.beacon == Hash32::ZERO {
        return Err(ObserveError::BeaconNotFixed);
    }
    if draw_of.attempt != seed.attempt
        || draw_of.commit != *draw_commit
        || draw_of.beacon_height != seed.beacon_height
        || draw_of.announced_at >= draw_of.beacon_height
        || draw_of.beacon != seed.beacon
    {
        return Err(ObserveError::BeaconMismatch);
    }
    Ok(())
}

/// One announced draw (L2-F2): the pool snapshot and beacon height fixed
/// before the beacon exists, so the coordinator cannot try beacons.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Announcement {
    spec_id: Hash32,
    cfg: ObserveConfig,
    pool: Vec<Observer>,
    beacon_height: u64,
    /// Each member's full bond at announcement (exposure is re-checked at open).
    full_bond: BTreeMap<MinerAddress, u128>,
}

/// A book's announced, unopened draw (version 5, review REVLOG-R5).
#[derive(BorshSerialize, BorshDeserialize)]
struct AnnouncedWire {
    fact: Hash32,
    attempt: u32,
    spec_id: Hash32,
    cfg: ObserveConfig,
    pool: Vec<Observer>,
    beacon_height: u64,
    full_bond: Vec<(MinerAddress, u128)>,
}

/// Error from a level-2 round.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ObserveError {
    /// The feed or a reveal is malformed.
    #[error(transparent)]
    Observation(#[from] ObservationError),
    /// Timing or economics invalid.
    #[error("observe config invalid: {0}")]
    BadConfig(&'static str),
    /// Fewer assigned observers than the feed requires.
    #[error("{have} observers assigned, the feed needs {need}")]
    TooFewObservers {
        /// Assigned.
        have: usize,
        /// Required.
        need: u16,
    },
    /// Observers not strictly ascending by address, or a bond below `outlier_slash`.
    #[error("observers must be strictly ascending with bonds covering the slash")]
    BadObservers,
    /// The address is not an assigned observer.
    #[error("not an assigned observer")]
    NotObserver,
    /// Outside the phase for this action.
    #[error("wrong phase for this action")]
    WrongPhase,
    /// A second commit from the same observer.
    #[error("already committed")]
    AlreadyCommitted,
    /// Reveal without a commit.
    #[error("no commit to reveal")]
    NoCommit,
    /// A second reveal.
    #[error("already revealed")]
    AlreadyRevealed,
    /// The reveal does not hash to the commit.
    #[error("reveal does not match the commit")]
    HashMismatch,
    /// The commit or reveal is not signed by the observer's key (ATT-R1-F3).
    #[error("not signed by the observer")]
    BadSignature,
    /// A level-2 claim that the settlement does not back (L2R-F3).
    #[error("claim not backed by this level-2 settlement: {0}")]
    ClaimNotBacked(&'static str),
    /// The claim itself is malformed or its rights are not the registry's.
    #[error(transparent)]
    Claim(#[from] ClaimError),
    /// This round was already settled into the store (L2R2-F1): its entries
    /// must never be applied twice.
    #[error("round already settled")]
    AlreadySettled,
    /// A round for the same fact (metric, subject, window end) was already
    /// settled into the store (L2R3-F5, L2R4-F1): a coordinator may not settle
    /// competing observer sets, or competing feeds that differ only in an
    /// irrelevant field, for one fact and pick a result.
    #[error("a round for this metric, subject and window end was already settled")]
    FeedAlreadySettled,
    /// `accept_claim` is the level-2 intake; other levels go through their own
    /// lanes (L2R4-F4).
    #[error("not a level-2 claim: use the claim's own lane")]
    NotLevelTwo,
    /// Bytes are not a canonical claim encoding.
    #[error("bytes are not a canonical claim encoding")]
    NotCanonical,
    /// No feed template is registered for this metric (L2SRC-R2-F1): a
    /// level-2 round or claim exists only for a registered metric.
    #[error("no feed template registered for this metric")]
    UnknownFeed,
    /// The spec is not the metric's registered feed, or its date is not
    /// observable in its window (L2SRC-R2-F1, F4).
    #[error(transparent)]
    Template(#[from] TemplateError),
    /// The capability was issued for another store.
    #[error("this feed admin capability belongs to another store")]
    NotFeedAdmin,
    /// A metric's template is registered once and never replaced.
    #[error("a feed template is already registered for this metric")]
    FeedAlreadyRegistered,
    /// A dated or period fact reached [`MAX_NON_VALUE_ROUNDS`] rounds that
    /// settled no value (SEC-R2-F1): it is closed until a reviewer reopens it
    /// ([`ObservationResults::reopen_fact`]), so a coordinator cannot re-draw
    /// observers until a colluding majority settles.
    #[error("this fact reached its cap of rounds without a value; a reviewer must reopen it")]
    FactClosed,
    /// [`ObservationResults::reopen_fact`] on a fact that is not closed.
    #[error("this fact is not closed")]
    FactNotClosed,
    /// A non-value book (AUD-2-F9) that does not parse.
    #[error("non-value book invalid: {0}")]
    BadNonValueBook(String),
    /// A pool snapshot that is empty, too large, not strictly ascending, or not
    /// the one the draw names; or an attempt past [`MAX_DRAW_ATTEMPTS`].
    #[error("observer pool snapshot invalid")]
    BadPool,
    /// The observers are not the draw of the pool with the announced beacon.
    #[error("observers are not the draw")]
    NotTheDraw,
    /// The chain could not be read (height or block hash).
    #[error("chain read failed: {0}")]
    Chain(String),
    /// The beacon block does not exist yet.
    #[error("the beacon block does not exist yet")]
    BeaconNotYet,
    /// The draw's beacon is not the chain's block hash at its height.
    #[error("the beacon is not the chain's block hash at its height")]
    BeaconMismatch,
    /// The registry shows the round's draw without a fixed beacon: an
    /// observer must not commit (ATT-R1-F4, [`check_commit_allowed`]).
    #[error("the registry shows no fixed beacon for this draw; observers must not commit")]
    BeaconNotFixed,
    /// No draw was announced for this fact and attempt (L2-F2).
    #[error("no draw announced for this fact and attempt")]
    DrawNotAnnounced,
    /// A draw is announced once per fact and attempt: announcing again after
    /// the beacon is known would let a coordinator try beacons (L2-F2).
    #[error("a draw was already announced for this fact and attempt")]
    DrawAlreadyAnnounced,
    /// The beacon height must be above the chain height at announcement.
    #[error("the beacon height must not exist yet")]
    BeaconNotFuture,
    /// The open call does not match the announcement (spec, config or pool).
    #[error("this round does not match its announced draw")]
    DrawMismatch,
    /// A drawn observer's bond no longer covers the slash after the rounds it
    /// already sits on (exposure, L31-F2 lesson).
    #[error("a drawn observer's bond is committed to other rounds")]
    ObserverExposed,
    /// The round's transcript log was already read (review REVLOG-R3-1): it
    /// is read once, from one pinned block, and never again.
    #[error("the transcript log of this round was already read")]
    LogAlreadyRead,
    /// A log snapshot that cannot be this round's: reveals already taken, a
    /// block not past `commit_close`, or slots of observers not drawn or out
    /// of order.
    #[error("transcript log snapshot invalid: {0}")]
    BadLogSnapshot(&'static str),
    /// The fact has an unfinished round (review REVLOG-R3-2): it is finished
    /// on its own draw, never replaced by a new draw.
    #[error("this fact has an unfinished round; finish it on the same draw")]
    RoundUnfinished,
    /// A saved unfinished round that is not one this store could have opened.
    #[error("saved unfinished round invalid: {0}")]
    BadUnfinished(&'static str),
    /// Only a round settled here leaves the book once its results are
    /// written (review REVLOG-R4-1): dropping an unsettled one would burn
    /// its draw.
    #[error("this round is not settled here; it stays in the book until it is")]
    RoundNotSettled,
    /// The book holds this draw (the draw invariant, review R8): it leaves
    /// the book only once its round is published or the chain proves it can
    /// never become a fixed beacon without a published round.
    #[error("the book holds draw {0}; it leaves the book only when published or proven unable to land or be fixed")]
    DrawHeld(u32),
    /// The registry's record of the draw is not the round's (a stale or
    /// mixed read, or another draw): the round is not opened, the draw
    /// stays in the book.
    #[error("the registry's draw is not the round's: {0}")]
    RegistryDrawMismatch(&'static str),
    /// A release proof that does not prove what it claims for the book's
    /// draw (review R8): the draw stays in the book.
    #[error("not a proof this draw may leave the book: {0}")]
    NoReleaseProof(&'static str),
    /// The round's sealed posts were already read (ROADMAP C11): once, from
    /// one block past `reveal_close`, and never again.
    #[error("the sealed posts of this round were already read")]
    SealedAlreadyRead,
    /// A sealed snapshot that cannot be this round's (ROADMAP C11): no log
    /// snapshot yet, a block not past `reveal_close`, slots of observers not
    /// drawn or out of order, a post whose key proof fails (the registry
    /// never holds one: a node that lies, C4-R3-2), another reveal key than
    /// the coordinator's, a dismissal that does not check, or an in-phase post
    /// neither revealed nor dismissed.
    #[error("sealed snapshot invalid: {0}")]
    BadSealedSnapshot(&'static str),
}

fn parse_hash(s: &str) -> Option<Hash32> {
    let b = hex::decode(s.strip_prefix("0x")?).ok()?;
    Some(Hash32(b.try_into().ok()?))
}

/// How many rounds that settle no value (`SourceUnavailable`, `NoResult`) a
/// dated or period fact may take before it is closed (SEC-R2-F1). Each such
/// round is a fresh draw of observers for a fact the first value would close
/// for good; without a cap a blocking minority could force re-draws until a
/// colluding majority settles its value.
pub const MAX_NON_VALUE_ROUNDS: u32 = 3;

/// The settled outcome of a round.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum ObservationOutcome {
    /// A strict majority of all observers agreed on this value.
    Value(Answer),
    /// A strict majority of all observers answered `Unavailable`.
    SourceUnavailable,
    /// No strict majority: nothing is published; only withholding is punished.
    NoResult,
}

/// What happened to one observer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum ObserverStatus {
    /// Agreed with the settled result: paid `reward`.
    Agreed,
    /// Revealed a value that disagrees with the settled value: `outlier_slash`.
    /// Numeric: more than twice the tolerance from the band center (L2R-F1).
    Outlier,
    /// Answered against the settled availability: `Unavailable` while a value
    /// settled, or a value while a majority found the source unavailable
    /// (L2R2-F4: a stale guess must never beat an honest `Unavailable`).
    /// `miss_slash`.
    Missed,
    /// Revealed, but nothing can be judged: no result (except a far number
    /// when a strict majority clusters, L2R2-F5); `Unavailable` while the
    /// majority found the source unavailable (L2R-F2: nobody is paid for an
    /// outage, so `Unavailable` is never a free answer); or a number between
    /// one and two tolerances from the center (honest drift, L2R-F1). No
    /// payment, no slash.
    NoCall,
    /// Committed, but no reveal reached the settlement. No payment, no slash
    /// (SIG-R1-F1): a missing reveal is the coordinator's word alone, and a
    /// coordinator can withhold an honest observer's signed reveal. A slash
    /// needs a public record that the observer did not reveal (TODO: reveals
    /// posted to the chain or a public log, or coordinator-countersigned
    /// receipts; THREATS SEC-R2-F1-P2).
    Unrevealed,
    /// No commit reached the settlement. No payment, no slash (SIG-R1-F1,
    /// as `Unrevealed`): a coordinator can drop a signed commit too.
    Absent,
}

/// Result of [`ObservationRound::settle`]. Fields are private and the type is
/// never decoded from bytes (L2R2-F1): only `settle` makes one, so a
/// settlement in hand is a settlement a round produced, never one a claim
/// submitter wrote.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize)]
pub struct ObservationSettlement {
    spec_id: Hash32,
    round_id: Hash32,
    outcome: ObservationOutcome,
    statuses: Vec<(MinerAddress, ObserverStatus)>,
    entries: Vec<(MinerAddress, i128)>,
}

impl ObservationSettlement {
    /// The feed.
    pub fn spec_id(&self) -> Hash32 {
        self.spec_id
    }

    /// The round (`ObservationRound::round_id`).
    pub fn round_id(&self) -> Hash32 {
        self.round_id
    }

    /// The outcome.
    pub fn outcome(&self) -> &ObservationOutcome {
        &self.outcome
    }

    /// Every assigned observer, ascending by address.
    pub fn statuses(&self) -> &[(MinerAddress, ObserverStatus)] {
        &self.statuses
    }

    /// Non-zero balance changes, ascending by address (rewards positive).
    pub fn entries(&self) -> &[(MinerAddress, i128)] {
        &self.entries
    }

    /// Keccak-256 of `"onyx-observe-result-v1" ‖ borsh(spec_id, round_id,
    /// outcome)`: what a level-2 claim (`VerificationMethod::ReObserved`)
    /// points at.
    pub fn result_hash(&self) -> Hash32 {
        result_hash_of(&self.spec_id, &self.round_id, &self.outcome)
    }

    /// Check that a level-2 claim is backed by this settlement (L2R-F3): `spec`
    /// is this round's feed (its id is checked, so the kind and scale come
    /// from the feed, never from the caller, L2R2-F6), the claim names this
    /// feed and this result, the round settled a value (not `NoResult` or
    /// `SourceUnavailable`), and the claimed value is that value (`Bytes` for
    /// an exact feed; for a numeric feed `Int` at scale 0, else `Fixed` at the
    /// feed's scale, mantissa equal to the settled number). Claims are accepted
    /// only through [`ObservationResults::accept_claim`], which looks the
    /// settlement up in the coordinator's own store.
    pub(crate) fn backs(
        &self,
        feed: &ObservationSpec,
        method: &VerificationMethod,
        value: &Value,
    ) -> Result<(), ObserveError> {
        if feed.id() != self.spec_id {
            return Err(ObserveError::ClaimNotBacked(
                "feed is not this round's feed",
            ));
        }
        let kind = feed.kind;
        let VerificationMethod::ReObserved { spec, result } = method else {
            return Err(ObserveError::ClaimNotBacked("not a level-2 claim"));
        };
        if *spec != self.spec_id || *result != self.result_hash() {
            return Err(ObserveError::ClaimNotBacked("other feed or other round"));
        }
        let ObservationOutcome::Value(settled) = &self.outcome else {
            return Err(ObserveError::ClaimNotBacked("the round settled no value"));
        };
        let same = match (kind, settled, value) {
            (ObservationKind::Exact { .. }, Answer::Bytes(s), Value::Bytes(v)) => s == v,
            (ObservationKind::Numeric { scale: 0, .. }, Answer::Number(n), Value::Int(v)) => n == v,
            (
                ObservationKind::Numeric { scale, .. },
                Answer::Number(n),
                Value::Fixed {
                    mantissa,
                    scale: vs,
                },
            ) => scale > 0 && *vs == scale && n == mantissa,
            _ => false,
        };
        if !same {
            return Err(ObserveError::ClaimNotBacked(
                "value differs from the settled value",
            ));
        }
        Ok(())
    }
}

/// The level-2 results this coordinator settled itself, and the one place a
/// claim is accepted (L2R2-F1). A result enters only through
/// [`ObservationResults::settle`], which settles the round here; nothing lets
/// a submitter hand in a settlement. Every path that takes in claims must go
/// through [`ObservationResults::accept_claim`].
///
/// Not `Clone` (L2R4-F2): a copy taken before settling would let a coordinator
/// settle competing observer sets in two copies and keep the one it likes.
/// Within one process the store is the single book; across coordinators
/// `OnyxRegistry` records one result per fact on chain (ADR-0030).
///
/// ```compile_fail
/// let (a, _admin) = onyx_coordinator::observe::ObservationResults::new();
/// let _b = a.clone();
/// ```
#[derive(Debug)]
pub struct ObservationResults {
    /// Which [`FeedAdmin`] may register templates here.
    id: u64,
    /// Metric -> its one canonical feed (L2SRC-R2-F1).
    templates: BTreeMap<MetricId, FeedTemplate>,
    /// Every registration, append-only.
    decisions: Vec<FeedDecision>,
    /// `result_hash` -> the round's feed and its settlement.
    by_result: BTreeMap<Hash32, (ObservationSpec, ObservationSettlement)>,
    /// `result_hash` -> the round's (pool hash, config hash), for
    /// `recordObservation`'s draw commitment (L2RND-R3).
    draw_of_result: BTreeMap<Hash32, (Hash32, Hash32)>,
    /// Rounds already settled here (never settled twice).
    rounds: BTreeSet<Hash32>,
    /// Facts already settled here (`fact_key`: metric, subject, window end):
    /// one result per fact, L2R3-F5 and L2R4-F1. Keyed on the fact a claim
    /// states, not the full spec id, so a feed that differs only in an
    /// irrelevant field (observer count, a selector spelling) is the same fact.
    facts: BTreeSet<Hash32>,
    /// Dated/period facts -> rounds settled here without a value (SEC-R2-F1).
    non_value: BTreeMap<Hash32, u32>,
    /// Facts -> draws that ended without a value (L2-F2): the next draw's
    /// attempt. Never reset, so a reopened fact never reuses an attempt whose
    /// beacon is already known.
    draws: BTreeMap<Hash32, u32>,
    /// The cap of [`MAX_NON_VALUE_ROUNDS`] (an admin may lower or raise it).
    non_value_cap: u32,
    /// Dated/period facts closed after `non_value_cap` rounds without a value.
    closed: BTreeSet<Hash32>,
    /// Every reopening of a closed fact and every cap change, append-only.
    fact_decisions: Vec<FactDecision>,
    /// (fact, attempt) -> its one announced draw (L2-F2).
    announcements: BTreeMap<(Hash32, u32), Announcement>,
    /// (fact, attempt) draws already opened (one round per announcement)
    /// -> the round id opened on it.
    opened: BTreeMap<(Hash32, u32), Hash32>,
    /// (fact, attempt) draws released from the book under a proof (the draw
    /// invariant, review R8: [`Self::release_draw`] is the only way a draw
    /// leaves the book): published, or shown on chain unable to land or to
    /// have its beacon fixed.
    released: BTreeSet<(Hash32, u32)>,
    /// Observer -> slash amounts committed to rounds opened and not settled.
    exposure: BTreeMap<MinerAddress, u128>,
    /// Fact -> its round a run opened and could not finish (review
    /// REVLOG-R3-2), saved with the book.
    unfinished: BTreeMap<Hash32, UnfinishedRound>,
    /// Facts whose kept round is a failed attempt waiting to be published
    /// (THREATS REVLOG-R4-5, ATT-R2-F3), saved with the book.
    failed: BTreeSet<Hash32>,
    /// Fact -> the draws the results store publishes for it (a round or a
    /// failed attempt each), as read from the results store before this run
    /// ([`Self::note_published_draws`], review R9-F1). Never saved with the
    /// book: the results store is the authority and is read again by every
    /// run. A fact absent here was not read, and no chain draw of it is
    /// adopted ([`Self::adopt_draw`]).
    results_read: BTreeMap<Hash32, Vec<DrawSeed>>,
}

/// One recorded reviewer action on a closed fact or the cap (SEC-R2-F1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactDecision {
    /// When (unix seconds, the caller's clock).
    pub at: u64,
    /// The fact reopened, or `None` for a cap change.
    pub fact: Option<Hash32>,
    /// The cap after this decision.
    pub cap: u32,
    /// Why.
    pub reason: String,
}

/// The right to register feed templates in one [`ObservationResults`]: issued
/// once, with the store, to the coordinator's admin path. Not `Clone`, no
/// public constructor.
///
/// ```compile_fail
/// let (_s, admin) = onyx_coordinator::observe::ObservationResults::new();
/// let _b = admin.clone();
/// ```
#[derive(Debug)]
pub struct FeedAdmin {
    store: u64,
}

/// One recorded feed registration (append-only, L2SRC-R2-F1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedDecision {
    /// When (unix seconds, the caller's clock).
    pub at: u64,
    /// The metric.
    pub metric: MetricId,
    /// [`FeedTemplate::id`] of the registered template.
    pub template: Hash32,
    /// Why (the review that cleared the source and the feed).
    pub reason: String,
}

static NEXT_STORE: AtomicU64 = AtomicU64::new(1);

/// Keccak-256 of `"onyx-observe-fact-v1" ‖ borsh(metric, subject, window_end)`:
/// the fact a level-2 claim states (L2R4-F1). Two feeds with the same key would
/// back two claims about the same thing at the same time. For a dated template
/// the subject already names the date, so the fact is `(metric, subject)`
/// whatever the window ([`fact_key_dated`], L2SRC-R2-F2).
pub fn fact_key(spec: &ObservationSpec) -> Hash32 {
    let mut b = b"onyx-observe-fact-v1".to_vec();
    b.extend_from_slice(
        &borsh::to_vec(&(&spec.metric, &spec.subject, spec.window_end)).unwrap_or_default(),
    );
    keccak256(&b)
}

/// Keccak-256 of `"onyx-observe-dated-fact-v1" ‖ borsh(metric, subject)`: one
/// accepted value per dated subject, whatever the window (L2SRC-R2-F2).
pub fn fact_key_dated(spec: &ObservationSpec) -> Hash32 {
    let mut b = b"onyx-observe-dated-fact-v1".to_vec();
    b.extend_from_slice(&borsh::to_vec(&(&spec.metric, &spec.subject)).unwrap_or_default());
    keccak256(&b)
}

impl ObservationResults {
    /// An empty store and the one capability that may register feed templates
    /// in it.
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> (Self, FeedAdmin) {
        let id = NEXT_STORE.fetch_add(1, Ordering::Relaxed);
        (
            ObservationResults {
                id,
                templates: BTreeMap::new(),
                decisions: Vec::new(),
                by_result: BTreeMap::new(),
                draw_of_result: BTreeMap::new(),
                rounds: BTreeSet::new(),
                facts: BTreeSet::new(),
                non_value: BTreeMap::new(),
                draws: BTreeMap::new(),
                non_value_cap: MAX_NON_VALUE_ROUNDS,
                closed: BTreeSet::new(),
                fact_decisions: Vec::new(),
                announcements: BTreeMap::new(),
                opened: BTreeMap::new(),
                released: BTreeSet::new(),
                exposure: BTreeMap::new(),
                unfinished: BTreeMap::new(),
                failed: BTreeSet::new(),
                results_read: BTreeMap::new(),
            },
            FeedAdmin { store: id },
        )
    }

    /// Register `template` as its metric's one canonical feed (L2SRC-R2-F1).
    /// A reviewer action: needs this store's [`FeedAdmin`], is recorded with
    /// `reason` and `now`, and is final (a metric's template is never
    /// replaced; a new feed is a new metric).
    pub fn register_feed(
        &mut self,
        admin: &FeedAdmin,
        template: FeedTemplate,
        reason: &str,
        now: u64,
    ) -> Result<(), ObserveError> {
        if admin.store != self.id {
            return Err(ObserveError::NotFeedAdmin);
        }
        template.validate()?;
        if self.templates.contains_key(&template.metric) {
            return Err(ObserveError::FeedAlreadyRegistered);
        }
        self.decisions.push(FeedDecision {
            at: now,
            metric: template.metric.clone(),
            template: template.id(),
            reason: reason.to_owned(),
        });
        self.templates.insert(template.metric.clone(), template);
        Ok(())
    }

    /// Reopen a dated or period fact closed after the cap of rounds without a
    /// value (SEC-R2-F1). A reviewer action: needs this store's
    /// [`FeedAdmin`], is recorded with `reason`; the fact's count restarts.
    pub fn reopen_fact(
        &mut self,
        admin: &FeedAdmin,
        fact: Hash32,
        reason: &str,
        now: u64,
    ) -> Result<(), ObserveError> {
        if admin.store != self.id {
            return Err(ObserveError::NotFeedAdmin);
        }
        if !self.closed.remove(&fact) {
            return Err(ObserveError::FactNotClosed);
        }
        self.non_value.remove(&fact);
        self.fact_decisions.push(FactDecision {
            at: now,
            fact: Some(fact),
            cap: self.non_value_cap,
            reason: reason.to_owned(),
        });
        Ok(())
    }

    /// Set the cap of rounds without a value per dated or period fact
    /// (default [`MAX_NON_VALUE_ROUNDS`]; at least 1). A reviewer action,
    /// recorded. Facts already at or over a lower cap close at their next
    /// round without a value.
    pub fn set_non_value_cap(
        &mut self,
        admin: &FeedAdmin,
        cap: u32,
        reason: &str,
        now: u64,
    ) -> Result<(), ObserveError> {
        if admin.store != self.id {
            return Err(ObserveError::NotFeedAdmin);
        }
        self.non_value_cap = cap.max(1);
        self.fact_decisions.push(FactDecision {
            at: now,
            fact: None,
            cap: self.non_value_cap,
            reason: reason.to_owned(),
        });
        Ok(())
    }

    /// Every reopening and cap change, in order (SEC-R2-F1).
    pub fn fact_decisions(&self) -> &[FactDecision] {
        &self.fact_decisions
    }

    /// The book of dated and period facts that settled rounds without a
    /// value, as JSON a runner keeps between restarts (AUD-2-F9, AUD-2R-F4):
    /// `{"version": 8, "cap": n, "facts": [{"fact": "0x..", "rounds": n,
    /// "closed": bool, "draws": n}], "decisions": [{"at", "fact", "cap", "reason"}],
    /// "unfinished": ["0x<borsh>"], "announced": ["0x<borsh>"],
    /// "failed": ["0x<fact>"]}`, facts sorted. The cap and the reviewer decisions travel with the
    /// counts, so a restart neither resets a reviewer's cap nor loses the
    /// record of a reopening. Under the same guard as [`Self::save_book`]
    /// (review R9-F3): refused while a settled round waits for its results
    /// to be written, or when the book would drop a draw it holds.
    pub fn non_value_book(&self) -> Result<serde_json::Value, String> {
        if self.awaiting_publication() {
            return Err(
                "a settled round waits for its results to be written; the book keeps it unsettled until then"
                    .into(),
            );
        }
        if let Some(why) = self.unwritable_draw() {
            return Err(format!(
                "{why}; the book would lose it (the draw invariant)"
            ));
        }
        Ok(self.book_json())
    }

    /// [`Self::non_value_book`] without the guard.
    fn book_json(&self) -> serde_json::Value {
        let facts: BTreeSet<&Hash32> = self
            .non_value
            .keys()
            .chain(&self.closed)
            .chain(self.draws.keys())
            .collect();
        serde_json::json!({
            "version": 8,
            "cap": self.non_value_cap,
            "facts": facts
                .into_iter()
                .map(|f| {
                    serde_json::json!({
                        "fact": f.to_string(),
                        "rounds": self.non_value.get(f).copied().unwrap_or(0),
                        "closed": self.closed.contains(f),
                        "draws": self.draws.get(f).copied().unwrap_or(0),
                    })
                })
                .collect::<Vec<_>>(),
            "decisions": self
                .fact_decisions
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "at": d.at,
                        "fact": d.fact.map(|f| f.to_string()),
                        "cap": d.cap,
                        "reason": d.reason,
                    })
                })
                .collect::<Vec<_>>(),
            // Review REVLOG-R3-2: unfinished rounds, Borsh as 0x-hex, each
            // re-checked when read back; since version 4 (review
            // REVLOG-R4-3) each reveal carries its arrival time.
            "unfinished": self
                .unfinished
                .values()
                .map(|u| format!("0x{}", hex::encode(borsh::to_vec(u).unwrap_or_default())))
                .collect::<Vec<_>>(),
            // Version 5 (review REVLOG-R5): each fact's latest draw announced
            // and not opened, so the next run resumes it; and the facts whose
            // kept round is a failed attempt not yet published (REVLOG-R4-5).
            "announced": self
                .pending_announcements()
                .into_iter()
                .map(|w| format!("0x{}", hex::encode(borsh::to_vec(&w).unwrap_or_default())))
                .collect::<Vec<_>>(),
            "failed": self.failed.iter().map(ToString::to_string).collect::<Vec<_>>(),
        })
    }

    /// Each fact's draw the book holds ([`Self::held`]) that is not kept
    /// as an opened round (review REVLOG-R5; review R8: an opened round not
    /// kept, a run stopped between the opening and the keeping, is written
    /// as announced, so the next run opens it again on the same draw rather
    /// than the book forgetting it).
    fn pending_announcements(&self) -> Vec<AnnouncedWire> {
        self.draws
            .keys()
            .filter_map(|fact| {
                let attempt = self.held(fact)?;
                if self.kept_at(fact, attempt) {
                    return None;
                }
                let a = self.announcements.get(&(*fact, attempt))?;
                Some(AnnouncedWire {
                    fact: *fact,
                    attempt,
                    spec_id: a.spec_id,
                    cfg: a.cfg,
                    pool: a.pool.clone(),
                    beacon_height: a.beacon_height,
                    full_bond: a.full_bond.iter().map(|(k, v)| (*k, *v)).collect(),
                })
            })
            .collect()
    }

    /// Take in a book written by [`Self::non_value_book`] (AUD-2-F9,
    /// AUD-2R-F4). Counts only tighten: a count rises to the book's count and
    /// a fact the book closed, or whose count reaches the cap, is closed.
    /// Nothing is reopened here; that stays the admin's recorded
    /// [`Self::reopen_fact`]. The book's cap is the last recorded reviewer
    /// setting and replaces the default; its decisions are kept as the
    /// history. A version-1 book (a bare array of facts) is still read.
    pub fn absorb_non_value_book(&mut self, book: &serde_json::Value) -> Result<(), ObserveError> {
        let bad = |m: &str| ObserveError::BadNonValueBook(m.to_owned());
        let mut unfinished = vec![];
        let mut announced: Vec<AnnouncedWire> = vec![];
        let mut failed: Vec<Hash32> = vec![];
        let rows = match book {
            serde_json::Value::Array(rows) => rows,
            serde_json::Value::Object(o) => {
                let version = o.get("version").and_then(serde_json::Value::as_u64);
                if !matches!(version, Some(2..=8)) {
                    return Err(bad("unknown book version"));
                }
                // Version 5 (review REVLOG-R5): draws announced and not
                // opened, and failed attempts not yet published.
                if matches!(version, Some(5..=8)) {
                    announced = o
                        .get("announced")
                        .and_then(serde_json::Value::as_array)
                        .ok_or_else(|| bad("announced is not an array"))?
                        .iter()
                        .map(|x| {
                            x.as_str()
                                .and_then(|h| hex::decode(h.strip_prefix("0x")?).ok())
                                .and_then(|b| AnnouncedWire::try_from_slice(&b).ok())
                                .ok_or_else(|| bad("an announced draw that does not decode"))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    failed = o
                        .get("failed")
                        .and_then(serde_json::Value::as_array)
                        .ok_or_else(|| bad("failed is not an array"))?
                        .iter()
                        .map(|x| {
                            x.as_str()
                                .and_then(parse_hash)
                                .ok_or_else(|| bad("a failed fact that is not a 0x hash"))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                }
                // Version 3 (review REVLOG-R3-2) carries unfinished rounds;
                // version 4 (review REVLOG-R4-3) their reveals' arrival times.
                // Version 6 (ROADMAP C11): a round's sealed snapshot.
                // Version 7 (ROADMAP C51): its time-lock read. Version 8
                // (ROADMAP C52): the read's far round; a version-7
                // time-locked round (posts without a far copy, never
                // deployed) is refused, any other version-7 round loads.
                if let Some(v @ (3..=8)) = version {
                    unfinished = o
                        .get("unfinished")
                        .and_then(serde_json::Value::as_array)
                        .ok_or_else(|| bad("unfinished is not an array"))?
                        .iter()
                        .map(|x| {
                            let b = x
                                .as_str()
                                .and_then(|h| hex::decode(h.strip_prefix("0x")?).ok());
                            b.and_then(|b| match v {
                                3 => UnfinishedWireV3::try_from_slice(&b).ok().map(Into::into),
                                4 | 5 => UnfinishedWireV5::try_from_slice(&b).ok().map(Into::into),
                                6 => UnfinishedWireV6::try_from_slice(&b).ok().map(Into::into),
                                7 => UnfinishedWireV7::try_from_slice(&b)
                                    .ok()
                                    .and_then(UnfinishedWireV7::upgrade),
                                _ => UnfinishedWire::try_from_slice(&b).ok(),
                            })
                            .ok_or_else(|| {
                                bad(if v == 7 {
                                    "an unfinished round that does not decode (a version-7 time-locked round has no far-date copy, ROADMAP C52: finish it with the build that wrote it)"
                                } else {
                                    "an unfinished round that does not decode"
                                })
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                }
                let cap = o
                    .get("cap")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n >= 1)
                    .ok_or_else(|| bad("cap is not a count of at least 1"))?;
                self.non_value_cap = cap;
                let decisions = o
                    .get("decisions")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| bad("decisions is not an array"))?;
                for d in decisions {
                    let fact = match &d["fact"] {
                        serde_json::Value::Null => None,
                        v => Some(
                            v.as_str()
                                .and_then(parse_hash)
                                .ok_or_else(|| bad("decision fact is not a 0x hash"))?,
                        ),
                    };
                    self.fact_decisions.push(FactDecision {
                        at: d["at"].as_u64().ok_or_else(|| bad("decision at"))?,
                        fact,
                        cap: d["cap"]
                            .as_u64()
                            .and_then(|n| u32::try_from(n).ok())
                            .ok_or_else(|| bad("decision cap"))?,
                        reason: d["reason"]
                            .as_str()
                            .ok_or_else(|| bad("decision reason"))?
                            .to_owned(),
                    });
                }
                o.get("facts")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| bad("facts is not an array"))?
            }
            _ => return Err(bad("not a book")),
        };
        // Review REVLOG-R4-2: unfinished rounds are restored BEFORE any fact
        // is closed, and a fact with one is not closed by its count here: it
        // was opened before a lower cap and closes at its next round without
        // a value ([`Self::settle`]), as it would have in the process that
        // saved the book. Only a fact the book itself closed stays closed.
        let mut pending = BTreeSet::new();
        for w in unfinished {
            pending.insert(self.restore_unfinished(w)?);
        }
        for f in failed {
            if !self.unfinished.contains_key(&f) {
                return Err(bad("a failed attempt without its kept round"));
            }
            self.failed.insert(f);
        }
        for w in announced {
            pending.insert(self.restore_announced(w)?);
        }
        for row in rows {
            let hex = row["fact"]
                .as_str()
                .ok_or_else(|| bad("fact is not a string"))?;
            let fact = parse_hash(hex).ok_or_else(|| bad("fact is not a 0x hash"))?;
            let rounds = row["rounds"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| bad("rounds is not a count"))?;
            let closed = row["closed"]
                .as_bool()
                .ok_or_else(|| bad("closed is not a bool"))?;
            // Draws used (L2-F2); a book written before the draw has none.
            let draws = match &row["draws"] {
                serde_json::Value::Null => rounds,
                v => v
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| bad("draws is not a count"))?,
            };
            let d = self.draws.entry(fact).or_insert(0);
            *d = (*d).max(draws);
            if rounds == 0 && !closed {
                continue;
            }
            let n = self.non_value.entry(fact).or_insert(0);
            *n = (*n).max(rounds);
            if closed || (*n >= self.non_value_cap && !pending.contains(&fact)) {
                self.closed.insert(fact);
            }
        }
        Ok(())
    }

    /// Whether a dated or period fact is closed after the cap of rounds
    /// without a value, and how many such rounds it has taken.
    pub fn fact_state(&self, fact: &Hash32) -> (bool, u32) {
        (
            self.closed.contains(fact),
            self.non_value.get(fact).copied().unwrap_or(0),
        )
    }

    /// The registered template of `metric`.
    pub fn template(&self, metric: &MetricId) -> Option<&FeedTemplate> {
        self.templates.get(metric)
    }

    /// Every feed registration, in order.
    pub fn feed_decisions(&self) -> &[FeedDecision] {
        &self.decisions
    }

    /// The template `spec` must equal, checked (L2SRC-R2-F1).
    fn template_of(&self, spec: &ObservationSpec) -> Result<&FeedTemplate, ObserveError> {
        let t = self
            .templates
            .get(&spec.metric)
            .ok_or(ObserveError::UnknownFeed)?;
        t.matches(spec)?;
        Ok(t)
    }

    /// Open a round, the only way to (L2SRC-R2-F1): `spec` must be exactly its
    /// metric's registered template instantiated for the spec's subject and
    /// window start, and a dated subject's date must be observable in that
    /// window.
    ///
    /// The observers are never chosen by the caller (L2-F2): they are the
    /// draw ([`draw_observers`]) of the pool snapshot fixed by
    /// [`announce_draw`](Self::announce_draw), with `beacon` (the chain's block
    /// hash at the announced height). Each drawn observer's bond must still
    /// cover the slash after the rounds it already sits on; the slash is then
    /// held until the round settles.
    pub fn open_round(
        &mut self,
        spec: ObservationSpec,
        cfg: ObserveConfig,
        pool: Vec<Observer>,
        chain: &dyn ChainView,
    ) -> Result<ObservationRound, ObserveError> {
        let (key, round) = self.prepare_open(spec, cfg, pool, chain)?;
        self.commit_open(key, &round);
        Ok(round)
    }

    /// [`Self::open_round`] on the registry's record `rec` of the draw, and
    /// the opened round kept in the book ([`Self::keep_unfinished`]) in the
    /// same step (review R8-1): the record is checked against the round
    /// BEFORE anything changes, so a mismatch (a stale or mixed read, or
    /// another draw) leaves the draw announced in the book, never opened and
    /// not kept. [`draw_and_open`] opens only through this.
    pub fn open_drawn(
        &mut self,
        spec: ObservationSpec,
        cfg: ObserveConfig,
        pool: Vec<Observer>,
        chain: &dyn ChainView,
        rec: &DrawRecord,
    ) -> Result<ObservationRound, ObserveError> {
        let (key, round) = self.prepare_open(spec, cfg, pool, chain)?;
        let d = round.draw();
        let why = if rec.attempt != d.attempt {
            Some("attempt")
        } else if rec.draw_count != d.attempt.saturating_add(1) {
            Some("drawCount (a later draw, or a read from another block)")
        } else if rec.commit != draw_commit(&d.pool, &round.spec_id, &cfg_hash(&round.cfg)) {
            Some("commitment")
        } else if rec.beacon_height != d.beacon_height {
            Some("beacon height")
        } else if rec.beacon != d.beacon {
            Some("beacon")
        } else if rec.announced_at >= d.beacon_height {
            Some("announced at or after its beacon height")
        } else {
            None
        };
        if let Some(why) = why {
            return Err(ObserveError::RegistryDrawMismatch(why));
        }
        self.commit_open(key, &round);
        self.unfinished.insert(
            key.0,
            UnfinishedRound {
                round: round.clone(),
                onchain_draw: *rec,
                reveals: vec![],
            },
        );
        self.failed.remove(&key.0);
        Ok(round)
    }

    /// Every check of [`Self::open_round`], changing nothing: the draw's
    /// key and the round.
    fn prepare_open(
        &self,
        spec: ObservationSpec,
        cfg: ObserveConfig,
        pool: Vec<Observer>,
        chain: &dyn ChainView,
    ) -> Result<((Hash32, u32), ObservationRound), ObserveError> {
        let (fact, _) = self.fact_of(&spec)?;
        // The draw opened is the fact's latest announced one (L2RND-R1-F2):
        // an earlier attempt is never opened after a later one was announced.
        let attempt = self
            .draws
            .get(&fact)
            .copied()
            .unwrap_or(0)
            .checked_sub(1)
            .ok_or(ObserveError::DrawNotAnnounced)?;
        if self.opened.contains_key(&(fact, attempt)) || self.released.contains(&(fact, attempt)) {
            return Err(ObserveError::DrawMismatch);
        }
        let ann = self
            .announcements
            .get(&(fact, attempt))
            .ok_or(ObserveError::DrawNotAnnounced)?;
        if ann.spec_id != spec.id() || ann.cfg != cfg || ann.pool != pool {
            return Err(ObserveError::DrawMismatch);
        }
        // The beacon is the chain's block hash at the announced height, read
        // here, never chosen by the caller (L2RND-R1-F1).
        let beacon = chain
            .block_hash(ann.beacon_height)
            .map_err(ObserveError::Chain)?
            .ok_or(ObserveError::BeaconNotYet)?;
        let draw = DrawSeed {
            pool: pool_hash(&pool),
            beacon_height: ann.beacon_height,
            beacon,
            attempt,
        };
        let full_bond = ann.full_bond.clone();
        let round = ObservationRound::open(spec, cfg, pool, draw)?;
        for o in &round.observers {
            let held = self.exposure.get(&o.address).copied().unwrap_or(0);
            let full = full_bond.get(&o.address).copied().unwrap_or(0);
            if held.saturating_add(cfg.outlier_slash) > full {
                return Err(ObserveError::ObserverExposed);
            }
        }
        Ok(((fact, attempt), round))
    }

    /// Record `round` opened on draw `key`: its observers' slashes held.
    fn commit_open(&mut self, key: (Hash32, u32), round: &ObservationRound) {
        for o in &round.observers {
            let e = self.exposure.entry(o.address).or_insert(0);
            *e = e.saturating_add(round.cfg.outlier_slash);
        }
        self.opened.insert(key, round.round_id);
    }

    /// The fact a spec's round settles and whether its subject is dated.
    /// Refuses an unregistered or mismatched spec, a closed fact, and a fact
    /// already settled.
    fn fact_of(&self, spec: &ObservationSpec) -> Result<(Hash32, bool), ObserveError> {
        let dated = self.template_of(spec)?.is_dated();
        let fact = if dated {
            fact_key_dated(spec)
        } else {
            fact_key(spec)
        };
        if self.closed.contains(&fact) {
            return Err(ObserveError::FactClosed);
        }
        if self.facts.contains(&fact) {
            return Err(ObserveError::FeedAlreadySettled);
        }
        Ok((fact, dated))
    }

    /// The fact a spec's round settles and the attempt its draw must name: 0
    /// for a fixed subject, the count of rounds without a value for a dated or
    /// period one. Refuses an unregistered or mismatched spec, a closed fact,
    /// and a fact already settled.
    fn fact_attempt(&self, spec: &ObservationSpec) -> Result<(Hash32, u32), ObserveError> {
        let (fact, _) = self.fact_of(spec)?;
        let attempt = self.draws.get(&fact).copied().unwrap_or(0);
        if attempt >= MAX_DRAW_ATTEMPTS {
            return Err(ObserveError::FactClosed);
        }
        Ok((fact, attempt))
    }

    /// The registry's fact arguments for `spec` (checked against its template).
    pub fn onchain_fact(&self, spec: &ObservationSpec) -> Result<OnchainFact, ObserveError> {
        let t = self.template_of(spec)?;
        Ok(OnchainFact {
            metric_key: onyx_core::onchain::metric_key(&spec.metric),
            subject_key: onyx_core::onchain::subject_key(&spec.subject),
            window_end: spec.window_end,
            dated: t.is_dated(),
            template: t.id(),
        })
    }

    /// Raise the fact's draw count to `onchain`, the registry's `drawCount`
    /// (L2RND-R1-F3): the chain is the authority, so a coordinator that
    /// announced on chain and crashed before saving its book never draws
    /// that attempt again. Counts only rise.
    pub fn sync_draws(&mut self, spec: &ObservationSpec, onchain: u32) -> Result<(), ObserveError> {
        let (fact, _) = self.fact_of(spec)?;
        // The draw invariant (review R8): a count past the book's held draw
        // would make it no longer the latest, so the book would forget it.
        // It is released first, under a proof ([`Self::release_draw`]).
        if let Some(a) = self.held(&fact) {
            if onchain > a.saturating_add(1) {
                return Err(ObserveError::DrawHeld(a));
            }
        }
        let d = self.draws.entry(fact).or_insert(0);
        *d = (*d).max(onchain);
        Ok(())
    }

    /// Raise a dated fact's rounds without a value to the chain's count
    /// (L2RND-R2-F3). A dated subject names one fact, so its `drawBudget`
    /// strikes since the last owner reopen are exactly the announced draws
    /// that did not end with a recorded value: a coordinator that saw a draw
    /// and dropped it (crash, restart, a lost book) cannot skip the count.
    /// The count is read from the chain, never from local memory. Closes the
    /// fact at the cap. Fixed subjects are bounded by the chain alone.
    pub fn sync_budget(
        &mut self,
        spec: &ObservationSpec,
        strikes: u32,
        allowance: u32,
    ) -> Result<(), ObserveError> {
        let (fact, dated) = self.fact_of(spec)?;
        let since_reopen = strikes.saturating_sub(allowance.saturating_sub(MAX_SUBJECT_DRAWS));
        if dated && since_reopen > 0 {
            let n = self.non_value.entry(fact).or_insert(0);
            *n = (*n).max(since_reopen);
            if *n >= self.non_value_cap {
                self.closed.insert(fact);
            }
        }
        Ok(())
    }

    /// The attempt of the draw the book holds for `fact` (the draw
    /// invariant, review R8): its latest announced draw, unless released
    /// under a proof ([`Self::release_draw`]). Opened or not, kept or not,
    /// settled or not: the book holds it until it is released.
    fn held(&self, fact: &Hash32) -> Option<u32> {
        let a = self.draws.get(fact)?.checked_sub(1)?;
        (self.announcements.contains_key(&(*fact, a)) && !self.released.contains(&(*fact, a)))
            .then_some(a)
    }

    /// The fact's kept round is on draw `attempt`.
    fn kept_at(&self, fact: &Hash32, attempt: u32) -> bool {
        self.unfinished
            .get(fact)
            .is_some_and(|u| u.round.draw.attempt == attempt)
    }

    /// The on-chain arguments of the draw the book holds for `spec`'s fact
    /// ([`Self::held`]), whatever its state (announced, opened, kept,
    /// settled; the fact closed or not): `None` when the book holds none.
    pub fn held_draw(&self, spec: &ObservationSpec) -> Option<OnchainDraw> {
        let fact = self.fact_key_of(spec).ok()?;
        let attempt = self.held(&fact)?;
        let ann = self.announcements.get(&(fact, attempt))?;
        Some(OnchainDraw {
            fact: self.onchain_fact(spec).ok()?,
            attempt,
            commit: draw_commit(&pool_hash(&ann.pool), &ann.spec_id, &cfg_hash(&ann.cfg)),
            beacon_height: ann.beacon_height,
        })
    }

    /// THE DRAW INVARIANT (review R8, ADR-0027 amendment 8 item 8f): the
    /// ONLY way a draw leaves the book. Once the book is saved with a draw
    /// (before `announceDraw` is sent), the draw stays in it, and in every
    /// book saved after, until one of:
    ///
    /// - [`Release::Published`]: its round, settled here, or its failed
    ///   attempt is durably written (the round id of the book's draw);
    /// - [`Release::NeverLands`]: a final block at or past its beacon height
    ///   shows no such draw at its attempt (none, or another one):
    ///   `announceDraw` reverts `BeaconNotFuture`/`BadAttempt` from there on;
    /// - [`Release::Unfixable`]: a final block past its beacon height plus
    ///   the 256-block `blockhash` window shows it with a zero beacon:
    ///   `fixBeacon` reverts `BeaconUnavailable` from there on;
    /// - [`Release::NeverSent`]: the save that held it failed and nothing
    ///   was sent ([`draw_and_open`] only).
    ///
    /// Each proof is checked here against the book's own draw (attempt,
    /// commitment, beacon height); anything short of one is refused
    /// ([`ObserveError::NoReleaseProof`]) and the draw stays. The chain
    /// proofs are built only from ONE final-block read
    /// ([`DrawChain::draw_of_final`], [`final_read`]). A draw that leaves
    /// can never become a fixed beacon without a published round, the state
    /// every client refuses for good.
    fn release_draw(&mut self, spec: &ObservationSpec, why: Release) -> Result<(), ObserveError> {
        let (fact, dated) = (self.fact_key_of(spec)?, self.template_of(spec)?.is_dated());
        let attempt = self.held(&fact).ok_or(ObserveError::DrawNotAnnounced)?;
        let key = (fact, attempt);
        let ann = self
            .announcements
            .get(&key)
            .ok_or(ObserveError::DrawNotAnnounced)?;
        let commit = draw_commit(&pool_hash(&ann.pool), &ann.spec_id, &cfg_hash(&ann.cfg));
        let ours = |r: &DrawRecord| {
            r.attempt == attempt && r.commit == commit && r.beacon_height == ann.beacon_height
        };
        let bh = ann.beacon_height;
        match why {
            Release::Published(round_id) => {
                let failed = self.failed.contains(&fact) && self.kept_at(&fact, attempt);
                let kept_id = self
                    .unfinished
                    .get(&fact)
                    .filter(|u| u.round.draw.attempt == attempt)
                    .map(|u| u.round.round_id);
                let opened_id = self.opened.get(&key).copied();
                let settled = self.rounds.contains(&round_id);
                let this =
                    kept_id == Some(round_id) || (kept_id.is_none() && opened_id == Some(round_id));
                if !this || !(settled || failed) {
                    return Err(ObserveError::NoReleaseProof(
                        "not the round of the book's draw, settled here or kept as a failed attempt",
                    ));
                }
                if kept_id.is_some() {
                    self.unfinished.remove(&fact);
                    self.failed.remove(&fact);
                }
                self.released.insert(key);
            }
            Release::NeverLands(r) => {
                // Another draw at its attempt (`BadAttempt` for ever), or
                // none at or past its beacon height (`BeaconNotFuture`).
                match &r.record {
                    Some(x) if ours(x) => {
                        return Err(ObserveError::NoReleaseProof("the chain holds the draw"))
                    }
                    None if r.block < bh => {
                        return Err(ObserveError::NoReleaseProof(
                            "the final block is below the draw's beacon height",
                        ))
                    }
                    _ => {}
                }
                if self.opened.contains_key(&key) || self.kept_at(&fact, attempt) {
                    return Err(ObserveError::NoReleaseProof("the draw was opened here"));
                }
                // Never on chain: its attempt is free again (L2RND-R2-F5).
                self.announcements.remove(&key);
                self.draws.insert(fact, attempt);
            }
            Release::Unfixable(r) => {
                let ok = r.block > bh.saturating_add(BLOCKHASH_WINDOW)
                    && r.record
                        .as_ref()
                        .is_some_and(|x| ours(x) && x.beacon == Hash32::ZERO);
                if !ok {
                    return Err(ObserveError::NoReleaseProof(
                        "no final block past the blockhash window showing the draw's beacon zero",
                    ));
                }
                if self.kept_at(&fact, attempt) {
                    return Err(ObserveError::NoReleaseProof("the draw's round is kept"));
                }
                // Counted (it was announced on chain): no beacon, no
                // committee; for a dated fact a round without a value.
                self.released.insert(key);
                if dated {
                    let n = self.non_value.entry(fact).or_insert(0);
                    *n = n.saturating_add(1);
                    if *n >= self.non_value_cap {
                        self.closed.insert(fact);
                    }
                }
            }
            Release::NeverSent => {
                if self.opened.contains_key(&key) || self.kept_at(&fact, attempt) {
                    return Err(ObserveError::NoReleaseProof("the draw was opened here"));
                }
                self.announcements.remove(&key);
                self.draws.insert(fact, attempt);
            }
        }
        Ok(())
    }

    /// Give up the book's draw of `spec` (it cannot open: a drawn
    /// observer's bond is committed elsewhere, too few eligible members, its
    /// beacon block left the `blockhash` window) only under the draw
    /// invariant ([`Self::release_draw`]): ONE final-block read
    /// ([`DrawChain::draw_of_final`], retried per `wait`) must show it can
    /// never land, or never have its beacon fixed. Otherwise Err and the
    /// draw stays in the book.
    pub fn abandon_draw(
        &mut self,
        spec: &ObservationSpec,
        chain: &dyn DrawChain,
        wait: &ChainWait,
    ) -> Result<(), String> {
        let draw = self
            .held_draw(spec)
            .ok_or_else(|| ObserveError::DrawNotAnnounced.to_string())?;
        let proof = gone_proof(chain, &draw, wait)?;
        self.release_draw(spec, proof).map_err(|e| e.to_string())
    }

    /// The fact's latest draw if it is announced and not opened (review
    /// REVLOG-R5): a run stopped between its announcement and the opening of
    /// its round. [`draw_and_open`] resumes it rather than announce another.
    pub fn pending_draw(&self, spec: &ObservationSpec) -> Option<OnchainDraw> {
        let (fact, _) = self.fact_of(spec).ok()?;
        let attempt = self.held(&fact)?;
        if self.opened.contains_key(&(fact, attempt)) {
            return None;
        }
        self.latest_draw(spec).ok()
    }

    /// The pool snapshot the fact's latest announcement fixed (review
    /// REVLOG-R5: a resumed draw opens with it).
    pub fn announced_pool(&self, spec: &ObservationSpec) -> Result<Vec<Observer>, ObserveError> {
        let (fact, _) = self.fact_of(spec)?;
        let attempt = self
            .draws
            .get(&fact)
            .copied()
            .unwrap_or(0)
            .checked_sub(1)
            .ok_or(ObserveError::DrawNotAnnounced)?;
        self.announcements
            .get(&(fact, attempt))
            .map(|a| a.pool.clone())
            .ok_or(ObserveError::DrawNotAnnounced)
    }

    /// The on-chain arguments of the fact's latest announced draw.
    pub fn latest_draw(&self, spec: &ObservationSpec) -> Result<OnchainDraw, ObserveError> {
        let (fact, _) = self.fact_of(spec)?;
        let attempt = self
            .draws
            .get(&fact)
            .copied()
            .unwrap_or(0)
            .checked_sub(1)
            .ok_or(ObserveError::DrawNotAnnounced)?;
        let ann = self
            .announcements
            .get(&(fact, attempt))
            .ok_or(ObserveError::DrawNotAnnounced)?;
        Ok(OnchainDraw {
            fact: self.onchain_fact(spec)?,
            attempt,
            commit: draw_commit(&pool_hash(&ann.pool), &ann.spec_id, &cfg_hash(&ann.cfg)),
            beacon_height: ann.beacon_height,
        })
    }

    /// The fact key a spec's round settles (dated or not, by its template).
    fn fact_key_of(&self, spec: &ObservationSpec) -> Result<Hash32, ObserveError> {
        Ok(if self.template_of(spec)?.is_dated() {
            fact_key_dated(spec)
        } else {
            fact_key(spec)
        })
    }

    /// Keep a round a run opened here and could not finish (review
    /// REVLOG-R3-2): it is saved with the book ([`Self::save_book`]), no new
    /// draw is announced for its fact ([`ObserveError::RoundUnfinished`]),
    /// and the next run takes a copy ([`Self::kept_round`]) to finish
    /// it on the same draw. Refused for a round not opened here. A round
    /// settled here is kept too (review REVLOG-R4-1): it stays until its
    /// results are written ([`Self::published`]).
    pub fn keep_unfinished(&mut self, u: UnfinishedRound) -> Result<(), ObserveError> {
        // Review R8: the fact key, whatever the fact's state (closed by the
        // cap, settled): a round of the draw the book holds is always kept.
        let fact = self.fact_key_of(&u.round.spec)?;
        let key = (fact, u.round.draw.attempt);
        // A round settled and published already (a retried finish, review
        // REVLOG-R3-3) has left the book for good: nothing to keep.
        if self.released.contains(&key)
            && self.opened.get(&key) == Some(&u.round.round_id)
            && self.rounds.contains(&u.round.round_id)
        {
            return Ok(());
        }
        if self.held(&fact) != Some(u.round.draw.attempt)
            || self.opened.get(&key) != Some(&u.round.round_id)
        {
            return Err(ObserveError::DrawNotAnnounced);
        }
        self.failed.remove(&fact);
        self.unfinished.insert(fact, u);
        Ok(())
    }

    /// Keep a round that failed before any reveal as a failed attempt
    /// waiting to be published (THREATS REVLOG-R4-5, ATT-R2-F3): saved with
    /// the book like an unfinished round (its draw gets no successor), but
    /// the next run publishes it as a failed attempt instead of finishing
    /// it. [`Self::attempt_published`] drops it once it is written.
    pub fn keep_failed(&mut self, u: UnfinishedRound) -> Result<(), ObserveError> {
        let fact = self.fact_key_of(&u.round.spec)?;
        self.keep_unfinished(u)?;
        self.failed.insert(fact);
        Ok(())
    }

    /// The kept failed attempt of `spec`'s fact, waiting to be published.
    pub fn failed_attempt(&self, spec: &ObservationSpec) -> Option<&UnfinishedRound> {
        let fact = self.fact_key_of(spec).ok()?;
        self.failed
            .contains(&fact)
            .then(|| self.unfinished.get(&fact))
            .flatten()
    }

    /// The failed attempt `round_id` of `spec`'s fact is durably written
    /// (published): drop it from the book (THREATS REVLOG-R4-5). Until then
    /// it stays kept, and the book on disk holds it, so a crash before the
    /// write publishes it again; idempotent.
    pub fn attempt_published(&mut self, spec: &ObservationSpec, round_id: &Hash32) {
        if self
            .failed_attempt(spec)
            .is_some_and(|u| u.round.round_id == *round_id)
        {
            // Only fails for another round: then nothing is released.
            let _ = self.release_draw(spec, Release::Published(*round_id));
        }
    }

    /// The round `round_id` of `spec`'s fact, settled here, has its results
    /// durably written (published): drop it from the book (review
    /// REVLOG-R4-1). Until then a settled round stays in the store, and
    /// [`Self::save_book`] refuses to write the book, so the book on disk
    /// keeps it UNSETTLED: a crash before the results are written makes the
    /// next run finish and publish the same round on the same draw (its
    /// receipts are on chain), never announce a new draw. Refused for a
    /// round not settled here.
    pub fn published(
        &mut self,
        spec: &ObservationSpec,
        round_id: &Hash32,
    ) -> Result<(), ObserveError> {
        if !self.rounds.contains(round_id) {
            return Err(ObserveError::RoundNotSettled);
        }
        let fact = self.fact_key_of(spec)?;
        // Idempotent: a round already released (or of an earlier draw)
        // changes nothing.
        let ours = self.held(&fact).is_some_and(|a| {
            self.opened.get(&(fact, a)) == Some(round_id)
                || self.kept_at(&fact, a)
                    && self.unfinished.get(&fact).map(|u| u.round.round_id) == Some(*round_id)
        });
        if ours {
            self.release_draw(spec, Release::Published(*round_id))?;
        }
        Ok(())
    }

    /// Whether the round `round_id` is settled in this store (its result is
    /// fixed: [`keep_taking_reveals`] takes no more reveals for it). A book
    /// never holds a round settled: one read back from it is unsettled
    /// (review REVLOG-R4-1).
    pub fn is_settled(&self, round_id: &Hash32) -> bool {
        self.rounds.contains(round_id)
    }

    /// Whether a kept round is settled here and waits for its results to be
    /// written ([`Self::published`], review REVLOG-R4-1).
    pub fn awaiting_publication(&self) -> bool {
        self.unfinished
            .values()
            .any(|u| self.rounds.contains(&u.round.round_id))
    }

    /// Why the book cannot be written without losing a draw it holds (the
    /// draw invariant, review R8), if so: a held draw that is not its
    /// fact's latest (a book holds one draw per fact), or a held draw whose
    /// round settled here and is neither kept nor published. `None`: every
    /// held draw is written ([`Self::non_value_book`]: kept rounds as
    /// unfinished, every other held draw as announced).
    fn unwritable_draw(&self) -> Option<String> {
        self.announcements.keys().find_map(|key @ (fact, a)| {
            if self.released.contains(key) {
                return None;
            }
            if self.held(fact) != Some(*a) {
                return Some(format!("draw {a} is held but is not its fact's latest"));
            }
            let settled = self
                .opened
                .get(key)
                .is_some_and(|r| self.rounds.contains(r));
            (settled && !self.kept_at(fact, *a))
                .then(|| format!("draw {a}'s round settled here, is not kept and is not published"))
        })
    }

    /// The unfinished round of `spec`'s fact, if one is kept.
    pub fn unfinished(&self, spec: &ObservationSpec) -> Option<&UnfinishedRound> {
        self.unfinished.get(&self.fact_key_of(spec).ok()?)
    }

    /// A copy of the unfinished round of `spec`'s fact (review
    /// REVLOG-R3-2), to finish it on its draw. The book keeps it (review
    /// R8: nothing but [`Self::release_draw`] drops a draw's round) until
    /// its results are written ([`Self::published`]); a newer state of it
    /// replaces it ([`Self::keep_unfinished`]).
    pub fn kept_round(&self, spec: &ObservationSpec) -> Option<UnfinishedRound> {
        self.unfinished.get(&self.fact_key_of(spec).ok()?).cloned()
    }

    /// [`Self::settle`], idempotent for a retry (review REVLOG-R3-3): a round
    /// already settled here with the same result (the same settlement,
    /// recomputed) returns the recorded settlement; one whose result would
    /// differ (different reveals) stays [`ObserveError::AlreadySettled`].
    pub fn settle_once(
        &mut self,
        round: &ObservationRound,
        now: u64,
    ) -> Result<ObservationSettlement, ObserveError> {
        if !self.rounds.contains(&round.round_id) {
            return self.settle(round, now);
        }
        let again = round.settle(now)?;
        match self.by_result.get(&again.result_hash()) {
            Some((_, s)) if *s == again => Ok(s.clone()),
            _ => Err(ObserveError::AlreadySettled),
        }
    }

    /// Put a saved unfinished round back (review REVLOG-R3-2), re-checked:
    /// its feed is this store's registered template, its round is the draw
    /// it names ([`ObservationRound::restore`]) and that draw is the one the
    /// registry recorded ([`check_commit_allowed`]). Its announcement and
    /// opening are restored, and its observers' slashes held again.
    fn restore_unfinished(&mut self, w: UnfinishedWire) -> Result<Hash32, ObserveError> {
        let round = ObservationRound::restore(w.round)?;
        let (fact, _) = self.fact_of(&round.spec)?;
        // ROADMAP C52: a time-locked snapshot's far round is the one the
        // registered template gives this round (its dismissals were checked
        // against the far round the book names, `check_dismissals`).
        if let Some(t) = round.sealed.as_ref().and_then(|s| s.timelock.as_ref()) {
            let want = crate::timelock::LockRounds::of_round(
                &crate::timelock::BeaconChain::quicknet(),
                self.template_of(&round.spec)?,
                &round.spec,
                round.cfg.reveal_close,
            )
            .map_err(|_| ObserveError::BadSealedSnapshot("no far round for this round"))?;
            if want.far() != t.far_round {
                return Err(ObserveError::BadSealedSnapshot(
                    "a far round that is not the registered template's",
                ));
            }
        }
        check_commit_allowed(
            &round.draw,
            &draw_commit(&round.draw.pool, &round.spec_id, &cfg_hash(&round.cfg)),
            &w.onchain_draw,
        )?;
        let key = (fact, round.draw.attempt);
        if let Some(a) = self.announcements.get(&key) {
            if a.spec_id != round.spec_id
                || a.cfg != round.cfg
                || a.pool != round.pool
                || a.beacon_height != round.draw.beacon_height
            {
                return Err(ObserveError::DrawMismatch);
            }
        } else {
            self.announcements.insert(
                key,
                Announcement {
                    spec_id: round.spec_id,
                    cfg: round.cfg,
                    pool: round.pool.clone(),
                    beacon_height: round.draw.beacon_height,
                    full_bond: round.pool.iter().map(|o| (o.address, o.bond)).collect(),
                },
            );
        }
        if self.opened.insert(key, round.round_id).is_none() {
            for o in &round.observers {
                let e = self.exposure.entry(o.address).or_insert(0);
                *e = e.saturating_add(round.cfg.outlier_slash);
            }
        }
        let d = self.draws.entry(fact).or_insert(0);
        *d = (*d).max(round.draw.attempt.saturating_add(1));
        self.unfinished.insert(
            fact,
            UnfinishedRound {
                round,
                onchain_draw: w.onchain_draw,
                reveals: w.reveals,
            },
        );
        Ok(fact)
    }

    /// Put a saved announced, unopened draw back (review REVLOG-R5): its
    /// announcement and its count. Opening it later checks it against the
    /// runner's spec, config and pool and the registry's record.
    fn restore_announced(&mut self, w: AnnouncedWire) -> Result<Hash32, ObserveError> {
        let key = (w.fact, w.attempt);
        if self.opened.contains_key(&key) || self.announcements.contains_key(&key) {
            return Err(ObserveError::BadNonValueBook(
                "an announced draw that is also opened or announced twice".into(),
            ));
        }
        check_pool(&w.pool)?;
        self.announcements.insert(
            key,
            Announcement {
                spec_id: w.spec_id,
                cfg: w.cfg,
                pool: w.pool,
                beacon_height: w.beacon_height,
                full_bond: w.full_bond.into_iter().collect(),
            },
        );
        let d = self.draws.entry(w.fact).or_insert(0);
        *d = (*d).max(w.attempt.saturating_add(1));
        Ok(w.fact)
    }

    /// Write the book ([`non_value_book`](Self::non_value_book)) to `path`
    /// atomically (AUD-2-F9, AUD-2R-F4): a temporary file in the same folder,
    /// synced, renamed over the old book, and the folder synced. A crash
    /// leaves the old book or the new one, never a torn file. Refused while
    /// a round settled here waits for its results to be written (review
    /// REVLOG-R4-1): the book on disk keeps that round unsettled until
    /// [`Self::published`], so the next run publishes it again rather than
    /// count it twice or lose it.
    pub fn save_book(&self, path: &std::path::Path) -> Result<(), String> {
        let book = self
            .non_value_book()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let text = serde_json::to_string_pretty(&book).map_err(|e| e.to_string())?;
        write_atomic(path, &format!("{text}\n"))
    }

    /// Read the book at `path` into the store, if the file exists (AUD-2-F9).
    /// A missing file is a first run; a file that does not parse (a torn or
    /// edited book) is an error that stops the start: counts are never
    /// silently reset (AUD-2R-F4).
    pub fn load_book(&mut self, path: &std::path::Path) -> Result<(), String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let book: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            format!(
                "{}: the book does not parse ({e}); refusing to start rather than reset its counts",
                path.display()
            )
        })?;
        self.absorb_non_value_book(&book)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Announce a round's draw (L2-F2), before its beacon exists: fixes the
    /// spec, config, pool snapshot and `beacon_height` (above `chain_height`,
    /// the deployment chain's current height). The snapshot's bonds are each
    /// member's bond minus the slashes it already has at stake, so a bond is
    /// never counted twice. The attempt is used here (L2RND-R1-F3): the book
    /// ([`to_json`](Self::to_json)) counts it from now on, so a coordinator
    /// that saw the beacon and restarts cannot draw the same attempt again;
    /// persist the book before the beacon height. The coordinator also
    /// announces the draw on chain (`OnyxRegistry.announceDraw`) before the
    /// beacon height; consumers check that announcement (`drawMatches`).
    pub fn announce_draw(
        &mut self,
        spec: &ObservationSpec,
        cfg: &ObserveConfig,
        members: &[Observer],
        beacon_height: u64,
        chain: &dyn ChainView,
    ) -> Result<Vec<Observer>, ObserveError> {
        let (fact, attempt) = self.fact_attempt(spec)?;
        // Review REVLOG-R3-2: a fact whose round is unfinished finishes that
        // round on its draw; a new draw would burn it and void the fact.
        if self.unfinished.contains_key(&fact) {
            return Err(ObserveError::RoundUnfinished);
        }
        // The draw invariant (review R8): the book's previous draw must have
        // left it under a proof ([`Self::release_draw`]) or settled here;
        // skipping a held draw would lose it.
        if attempt > 0 && self.blocks_next(&fact, attempt - 1) {
            return Err(ObserveError::DrawAlreadyAnnounced);
        }
        let chain_height = chain.height().map_err(ObserveError::Chain)?;
        if beacon_height <= chain_height {
            return Err(ObserveError::BeaconNotFuture);
        }
        let pool = self.pool_snapshot(spec, cfg, members)?;
        let full_bond = members.iter().map(|m| (m.address, m.bond)).collect();
        self.announcements.insert(
            (fact, attempt),
            Announcement {
                spec_id: spec.id(),
                cfg: *cfg,
                pool: pool.clone(),
                beacon_height,
                full_bond,
            },
        );
        self.draws.insert(fact, attempt + 1);
        Ok(pool)
    }

    /// The pool snapshot a draw of `spec` announces now: each member's bond
    /// minus the slashes it already has at stake ([`Self::announce_draw`]).
    fn pool_snapshot(
        &self,
        spec: &ObservationSpec,
        cfg: &ObserveConfig,
        members: &[Observer],
    ) -> Result<Vec<Observer>, ObserveError> {
        check_pool(members)?;
        cfg.validate(spec)?;
        Ok(members
            .iter()
            .map(|m| Observer {
                address: m.address,
                bond: m
                    .bond
                    .saturating_sub(self.exposure.get(&m.address).copied().unwrap_or(0)),
            })
            .collect())
    }

    /// The book's draw `prev` keeps the fact from a next draw (the draw
    /// invariant, review R8): announced, not released and not settled here.
    fn blocks_next(&self, fact: &Hash32, prev: u32) -> bool {
        let key = (*fact, prev);
        self.announcements.contains_key(&key)
            && !self.released.contains(&key)
            && !self
                .opened
                .get(&key)
                .is_some_and(|r| self.rounds.contains(r))
    }

    /// The attempt the fact's next announcement would take: the book's draw
    /// count (review R6, REVLOG-R5-2).
    pub fn next_attempt(&self, spec: &ObservationSpec) -> Result<u32, ObserveError> {
        Ok(self.fact_attempt(spec)?.1)
    }

    /// Review R9-F1: `draws`, the draws the results store publishes for
    /// `spec`'s fact (a round or a failed attempt each, as written), read
    /// from the results store by the runner before the run. A book that
    /// lacks a chain draw (a lost book) must never adopt one whose round is
    /// already published: the round it would open is a second, different
    /// entry for that draw, which the results store refuses, and the draw
    /// after it (fixable by anyone) would be left without a published
    /// round. Without this call no chain draw of the fact is adopted
    /// ([`Self::adopt_draw`]). Not saved with the book: the results store
    /// is the authority, read again by every run. Replaces an earlier note.
    pub fn note_published_draws(
        &mut self,
        spec: &ObservationSpec,
        draws: &[DrawSeed],
    ) -> Result<(), ObserveError> {
        let fact = self.fact_key_of(spec)?;
        let mut d = draws.to_vec();
        d.sort_by_key(|x| x.attempt);
        self.results_read.insert(fact, d);
        Ok(())
    }

    /// The draw at `attempt` the results store publishes for `spec`'s fact
    /// ([`Self::note_published_draws`]): `None` when the results store was
    /// not read, `Some(None)` when it publishes no draw at `attempt`.
    pub fn published_draw(&self, spec: &ObservationSpec, attempt: u32) -> Option<Option<DrawSeed>> {
        let fact = self.fact_key_of(spec).ok()?;
        let p = self.results_read.get(&fact)?;
        Some(p.iter().find(|d| d.attempt == attempt).copied())
    }

    /// Whether [`Self::adopt_draw`] would adopt `rec` now (nothing changes).
    pub fn adoptable(
        &self,
        spec: &ObservationSpec,
        cfg: &ObserveConfig,
        members: &[Observer],
        rec: &DrawRecord,
    ) -> Result<bool, ObserveError> {
        let (fact, attempt) = self.fact_attempt(spec)?;
        if rec.attempt != attempt
            || self.unfinished.contains_key(&fact)
            || (attempt > 0 && self.blocks_next(&fact, attempt - 1))
        {
            return Ok(false);
        }
        // Review R9-F1: never a draw the results store publishes (its round
        // is settled history), and never without the results store read.
        if self
            .results_read
            .get(&fact)
            .is_none_or(|p| p.iter().any(|d| d.attempt == attempt))
        {
            return Ok(false);
        }
        let pool = self.pool_snapshot(spec, cfg, members)?;
        Ok(draw_commit(&pool_hash(&pool), &spec.id(), &cfg_hash(cfg)) == rec.commit)
    }

    /// Adopt the registry's draw `rec` that the book lacks (THREATS
    /// REVLOG-R5-2: the process died after `announceDraw` landed and before
    /// the book was first saved), as [`Self::announce_draw`] would have
    /// recorded it, when it is this book's next attempt, the results store
    /// was read and publishes no draw at that attempt (review R9-F1,
    /// [`Self::note_published_draws`]), and its commitment equals
    /// [`draw_commit`] of the pool snapshot, spec and config this runner
    /// announces now; the beacon height is the registry's. `Ok(false)`
    /// (nothing changed) for any other draw. The commitment binds every input
    /// but the beacon, which the chain chose after the announcement, so
    /// adopting gives the coordinator no new choice (L2RND-R1-F3).
    /// [`draw_and_open`] adopts only a record read at ONE final block that
    /// shows it as the chain's last draw.
    pub fn adopt_draw(
        &mut self,
        spec: &ObservationSpec,
        cfg: &ObserveConfig,
        members: &[Observer],
        rec: &DrawRecord,
    ) -> Result<bool, ObserveError> {
        if !self.adoptable(spec, cfg, members, rec)? {
            return Ok(false);
        }
        let (fact, attempt) = self.fact_attempt(spec)?;
        let pool = self.pool_snapshot(spec, cfg, members)?;
        let full_bond = members.iter().map(|m| (m.address, m.bond)).collect();
        self.announcements.insert(
            (fact, attempt),
            Announcement {
                spec_id: spec.id(),
                cfg: *cfg,
                pool,
                beacon_height: rec.beacon_height,
                full_bond,
            },
        );
        self.draws.insert(fact, attempt + 1);
        Ok(true)
    }

    /// The slash amount `observer` has at stake in open rounds.
    pub fn exposure(&self, observer: &MinerAddress) -> u128 {
        self.exposure.get(observer).copied().unwrap_or(0)
    }

    /// Settle `round` at `now` and record its result. The only way to settle
    /// a round (`ObservationRound::settle` is crate-private, L2R3-F5). Refuses
    /// a round settled before, so its entries are produced once, and a second
    /// round for a fact already settled here (same metric, subject and window
    /// end, whatever the other spec fields, L2R4-F1), so a coordinator cannot
    /// run competing observer sets or competing feeds for one fact and keep
    /// the result it likes. One store is one coordinator's book; across
    /// coordinators `OnyxRegistry` records one result per fact on chain
    /// (ADR-0030).
    ///
    /// For a dated or period subject (one fact per date, whatever the window)
    /// only a round that settles a value consumes the fact (SEC-R1-F1): a
    /// `SourceUnavailable` or `NoResult` round (the source blocked the
    /// observers, a late filer had not filed yet, a disagreeing amended row)
    /// leaves the date open, so a later round can settle the real value. The
    /// first value still wins for good. A fixed subject's fact is its window,
    /// which no later round can observe again, so any outcome closes it. A
    /// dated fact takes at most [`MAX_NON_VALUE_ROUNDS`] rounds without a value
    /// (the store's cap) and is then closed until a reviewer reopens it
    /// (SEC-R2-F1): otherwise a blocking minority could force re-draws until a
    /// colluding majority settles.
    pub fn settle(
        &mut self,
        round: &ObservationRound,
        now: u64,
    ) -> Result<ObservationSettlement, ObserveError> {
        if self.rounds.contains(&round.round_id) {
            return Err(ObserveError::AlreadySettled);
        }
        let dated = self.template_of(&round.spec)?.is_dated();
        let fact = if dated {
            fact_key_dated(&round.spec)
        } else {
            fact_key(&round.spec)
        };
        if self.facts.contains(&fact) {
            return Err(ObserveError::FeedAlreadySettled);
        }
        if self.closed.contains(&fact) {
            return Err(ObserveError::FactClosed);
        }
        // Only a round opened here from its announced draw settles (L2-F2).
        let key = (fact, round.draw.attempt);
        let ann = self
            .announcements
            .get(&key)
            .ok_or(ObserveError::DrawNotAnnounced)?;
        if self.opened.get(&key) != Some(&round.round_id)
            || ann.spec_id != round.spec_id
            || ann.cfg != round.cfg
            || ann.pool != round.pool
            || ann.beacon_height != round.draw.beacon_height
        {
            return Err(ObserveError::DrawMismatch);
        }
        let s = round.settle(now)?;
        self.rounds.insert(round.round_id);
        // Review REVLOG-R4-1: a kept round stays kept until its results are
        // written ([`Self::published`]).
        for o in &round.observers {
            if let Some(e) = self.exposure.get_mut(&o.address) {
                *e = e.saturating_sub(round.cfg.outlier_slash);
            }
        }
        if !dated || matches!(s.outcome(), ObservationOutcome::Value(_)) {
            self.facts.insert(fact);
        } else {
            // A dated fact stays open after a round without a value, but only
            // `non_value_cap` times (SEC-R2-F1): then a reviewer must reopen it.
            // The next round is a new draw (L2-F2); its attempt was already
            // counted when it was announced (L2RND-R1-F3).
            let n = self.non_value.entry(fact).or_insert(0);
            *n = n.saturating_add(1);
            if *n >= self.non_value_cap {
                self.closed.insert(fact);
            }
        }
        self.by_result
            .insert(s.result_hash(), (round.spec.clone(), s.clone()));
        self.draw_of_result
            .insert(s.result_hash(), (round.draw.pool, cfg_hash(&round.cfg)));
        Ok(s)
    }

    /// The arguments of `OnyxRegistry.recordObservation` for the round settled
    /// under `result`, or `None` when it settled no value (SEC-R1-F1). The
    /// contract cannot see an outcome, only a result hash, so the coordinator
    /// records value outcomes only: a non-value record would occupy the fact
    /// key on chain. It could never mislead a consumer (`onchain_observation_check`
    /// refuses a transcript without a value), but it would block the date.
    pub fn onchain_record(&self, result: &Hash32) -> Option<OnchainObservationCheck> {
        let (spec, s) = self.by_result.get(result)?;
        if !matches!(s.outcome(), ObservationOutcome::Value(_)) {
            return None;
        }
        let (draw_pool, cfg_hash) = *self.draw_of_result.get(result)?;
        Some(OnchainObservationCheck {
            metric_key: onyx_core::onchain::metric_key(&spec.metric),
            subject_key: onyx_core::onchain::subject_key(&spec.subject),
            window_end: spec.window_end,
            spec_id: spec.id(),
            result: s.result_hash(),
            draw_pool,
            cfg_hash,
        })
    }

    /// The settlement recorded under `result`, if this coordinator settled it.
    pub fn get(&self, result: &Hash32) -> Option<&ObservationSettlement> {
        self.by_result.get(result).map(|(_, s)| s)
    }

    /// Accept a claim: it must validate against the registry, and a level-2
    /// claim must be backed by a result settled in this store and bound to
    /// that result's feed in every field a settled value could be reused for
    /// (L2R3-F1):
    /// - `method`: this feed's id and this result's hash, a round that settled
    ///   a value, and `value` equal to that value (`backs`);
    /// - `metric` and `subject`: exactly the feed's;
    /// - `source`: the feed's source id, snapshot `Document { published_unix:
    ///   window_end, content_hash: result_hash }` (the value is "as of" the end
    ///   of the window, and the snapshot names this result);
    /// - `rights`: exactly the feed's;
    /// - `label_set`: none (a level-2 value depends on no label set);
    /// - `evidence`: empty (the result hash is the evidence);
    /// - `confidence`: none (objective lane, already refused by `validate`).
    ///
    /// Returns `ProofLevel::ReObserved`. This is the level-2 intake only: a
    /// claim of any other method is refused with `NotLevelTwo` (L2R4-F4) and
    /// goes through its own lane (recompute, attestation, scoring), so this
    /// function never vouches for a level it did not check.
    pub fn accept_claim(
        &self,
        claim: &Claim,
        reg: &dyn Registry,
    ) -> Result<ProofLevel, ObserveError> {
        claim.validate(reg)?;
        let VerificationMethod::ReObserved { result, .. } = &claim.method else {
            return Err(ObserveError::NotLevelTwo);
        };
        {
            let (feed, s) = self
                .by_result
                .get(result)
                .ok_or(ObserveError::ClaimNotBacked(
                    "no round settled with this result",
                ))?;
            s.backs(feed, &claim.method, &claim.value)?;
            // The feed is still its metric's registered template.
            self.template_of(feed)?;
            if claim.metric != feed.metric {
                return Err(ObserveError::ClaimNotBacked("metric is not the feed's"));
            }
            if claim.subject != feed.subject {
                return Err(ObserveError::ClaimNotBacked("subject is not the feed's"));
            }
            if claim.source.source != feed.rights.source {
                return Err(ObserveError::ClaimNotBacked("source is not the feed's"));
            }
            let snapshot = Snapshot::Document {
                published_unix: feed.window_end,
                content_hash: s.result_hash(),
            };
            if claim.source.snapshot != snapshot {
                return Err(ObserveError::ClaimNotBacked(
                    "snapshot must be the feed's window end and this result",
                ));
            }
            if claim.rights != feed.rights {
                return Err(ObserveError::ClaimNotBacked(
                    "rights differ from the feed's",
                ));
            }
            if claim.label_set.is_some() {
                return Err(ObserveError::ClaimNotBacked(
                    "a level-2 value depends on no label set",
                ));
            }
            if !claim.evidence.is_empty() {
                return Err(ObserveError::ClaimNotBacked(
                    "a level-2 claim carries no evidence of its own",
                ));
            }
        }
        Ok(ProofLevel::ReObserved)
    }

    /// Decode a claim from canonical bytes and accept it (the level-2 ingest
    /// path; `Claim::decode` refuses level-2 claims, L2R3-F3).
    pub fn decode_claim(&self, bytes: &[u8], reg: &dyn Registry) -> Result<Claim, ObserveError> {
        let claim: Claim =
            from_canonical_bytes(bytes).map_err(|_: CanonicalError| ObserveError::NotCanonical)?;
        self.accept_claim(&claim, reg)?;
        Ok(claim)
    }
}

/// Strict majority of `members`.
pub fn quorum(members: usize) -> usize {
    members / 2 + 1
}

/// Lower median of a non-empty sorted slice.
fn lower_median(sorted: &[i128]) -> Option<i128> {
    sorted.get(sorted.len().checked_sub(1)? / 2).copied()
}

/// How a revealed answer compares with a settled value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Judgment {
    Agrees,
    /// Numeric only: beyond the tolerance but within twice it (L2R-F1). When
    /// honest answers drift apart within the window, the median can land near
    /// one honest edge; a minority could then pick which honest observer falls
    /// just outside the band. Nobody is slashed for that distance.
    Near,
    Disagrees,
}

/// Judge `answer` against a settled value under `kind`.
fn judge(kind: ObservationKind, settled: &Answer, answer: &Answer, band_center: i128) -> Judgment {
    match (kind, settled, answer) {
        (ObservationKind::Exact { .. }, Answer::Bytes(s), Answer::Bytes(a)) if s == a => {
            Judgment::Agrees
        }
        (ObservationKind::Numeric { tolerance, .. }, Answer::Number(_), Answer::Number(a)) => {
            let d = a.abs_diff(band_center);
            if d <= tolerance {
                Judgment::Agrees
            } else if d <= tolerance.saturating_mul(2) {
                Judgment::Near
            } else {
                Judgment::Disagrees
            }
        }
        _ => Judgment::Disagrees,
    }
}

/// The aggregation rule (pure). `members` is the number of ASSIGNED observers;
/// `answers` are the accepted reveals. Returns the outcome and, for a numeric
/// value, the band center (the median of all numeric reveals).
///
/// Exact: the most common byte string, if at least `quorum(members)` observers
/// revealed it. Numeric: the band is `median ± tolerance` around the lower
/// median of all numeric reveals; if at least `quorum(members)` numbers fall in
/// it, the value is the lower median of those numbers. If a strict majority's
/// numbers lie within `tolerance` of each other, the median of all numbers lies
/// between their extremes, so all of them are in the band. The settled value is
/// the median of in-band numbers, so a minority inside the band can move it,
/// but only within the honest majority's own range (L2R-F4). Unavailable: if at
/// least `quorum(members)` answered it. The three cannot overlap.
pub fn aggregate(
    kind: ObservationKind,
    members: usize,
    answers: &[Answer],
) -> (ObservationOutcome, i128) {
    let q = quorum(members);
    let unavailable = answers
        .iter()
        .filter(|a| **a == Answer::Unavailable)
        .count();
    if unavailable >= q {
        return (ObservationOutcome::SourceUnavailable, 0);
    }
    match kind {
        ObservationKind::Exact { .. } => {
            let mut counts: BTreeMap<&[u8], usize> = BTreeMap::new();
            for a in answers {
                if let Answer::Bytes(b) = a {
                    *counts.entry(b.as_slice()).or_default() += 1;
                }
            }
            match counts.into_iter().find(|(_, n)| *n >= q) {
                Some((b, _)) => (ObservationOutcome::Value(Answer::Bytes(b.to_vec())), 0),
                None => (ObservationOutcome::NoResult, 0),
            }
        }
        ObservationKind::Numeric { tolerance, .. } => {
            let mut nums: Vec<i128> = answers
                .iter()
                .filter_map(|a| match a {
                    Answer::Number(n) => Some(*n),
                    _ => None,
                })
                .collect();
            nums.sort_unstable();
            let Some(center) = lower_median(&nums) else {
                return (ObservationOutcome::NoResult, 0);
            };
            let band: Vec<i128> = nums
                .into_iter()
                .filter(|n| n.abs_diff(center) <= tolerance)
                .collect();
            match lower_median(&band) {
                Some(v) if band.len() >= q => {
                    (ObservationOutcome::Value(Answer::Number(v)), center)
                }
                _ => (ObservationOutcome::NoResult, center),
            }
        }
    }
}

/// One level-2 round for one feed. Serialisable (Borsh) so an unfinished
/// round is saved with the round book (review REVLOG-R3-2); it is read back
/// only through [`ObservationResults::load_book`], which re-checks it
/// ([`ObservationRound::restore`]).
#[derive(Clone, Debug, BorshSerialize)]
pub struct ObservationRound {
    spec: ObservationSpec,
    spec_id: Hash32,
    round_id: Hash32,
    cfg: ObserveConfig,
    pool: Vec<Observer>,
    draw: DrawSeed,
    observers: Vec<Observer>,
    commits: BTreeMap<MinerAddress, (Hash32, ObserverSig)>,
    reveals: BTreeMap<MinerAddress, (Observation, Hash32, ObserverSig)>,
    /// The transcript log as read once (review REVLOG-R3-1).
    log: Option<LogSnapshot>,
    /// The sealed posts as read once, past `reveal_close`, with the
    /// dismissals (ROADMAP C11; book version 6).
    sealed: Option<SealedSnapshot>,
}

/// [`ObservationRound`]'s Borsh layout, decoded only to be re-checked
/// ([`ObservationRound::restore`]): a round is never trusted from bytes.
#[derive(BorshDeserialize)]
struct RoundWire {
    spec: ObservationSpec,
    spec_id: Hash32,
    round_id: Hash32,
    cfg: ObserveConfig,
    pool: Vec<Observer>,
    draw: DrawSeed,
    observers: Vec<Observer>,
    commits: BTreeMap<MinerAddress, (Hash32, ObserverSig)>,
    reveals: BTreeMap<MinerAddress, (Observation, Hash32, ObserverSig)>,
    log: Option<LogSnapshot>,
    sealed: Option<SealedSnapshot>,
}

/// A round as book version 6 saved it: a sealed snapshot without a time-lock.
#[derive(BorshDeserialize)]
struct RoundWireV6 {
    spec: ObservationSpec,
    spec_id: Hash32,
    round_id: Hash32,
    cfg: ObserveConfig,
    pool: Vec<Observer>,
    draw: DrawSeed,
    observers: Vec<Observer>,
    commits: BTreeMap<MinerAddress, (Hash32, ObserverSig)>,
    reveals: BTreeMap<MinerAddress, (Observation, Hash32, ObserverSig)>,
    log: Option<LogSnapshot>,
    sealed: Option<crate::sealed::SealedSnapshotV6>,
}

impl From<RoundWireV6> for RoundWire {
    fn from(w: RoundWireV6) -> Self {
        RoundWire {
            spec: w.spec,
            spec_id: w.spec_id,
            round_id: w.round_id,
            cfg: w.cfg,
            pool: w.pool,
            draw: w.draw,
            observers: w.observers,
            commits: w.commits,
            reveals: w.reveals,
            log: w.log,
            sealed: w.sealed.map(Into::into),
        }
    }
}

/// A round as book version 7 saved it: its sealed snapshot's time-lock read
/// has no far round (ROADMAP C52); only a round without a time-locked
/// snapshot is taken back ([`Self::upgrade`]).
#[derive(BorshDeserialize)]
struct RoundWireV7 {
    spec: ObservationSpec,
    spec_id: Hash32,
    round_id: Hash32,
    cfg: ObserveConfig,
    pool: Vec<Observer>,
    draw: DrawSeed,
    observers: Vec<Observer>,
    commits: BTreeMap<MinerAddress, (Hash32, ObserverSig)>,
    reveals: BTreeMap<MinerAddress, (Observation, Hash32, ObserverSig)>,
    log: Option<LogSnapshot>,
    sealed: Option<crate::sealed::SealedSnapshotV7>,
}

impl RoundWireV7 {
    fn upgrade(self) -> Option<RoundWire> {
        let sealed = match self.sealed {
            None => None,
            Some(s) => Some(s.upgrade()?),
        };
        Some(RoundWire {
            spec: self.spec,
            spec_id: self.spec_id,
            round_id: self.round_id,
            cfg: self.cfg,
            pool: self.pool,
            draw: self.draw,
            observers: self.observers,
            commits: self.commits,
            reveals: self.reveals,
            log: self.log,
            sealed,
        })
    }
}

/// A round as books of versions 3 to 5 saved it: no sealed snapshot.
#[derive(BorshDeserialize)]
struct RoundWireV5 {
    spec: ObservationSpec,
    spec_id: Hash32,
    round_id: Hash32,
    cfg: ObserveConfig,
    pool: Vec<Observer>,
    draw: DrawSeed,
    observers: Vec<Observer>,
    commits: BTreeMap<MinerAddress, (Hash32, ObserverSig)>,
    reveals: BTreeMap<MinerAddress, (Observation, Hash32, ObserverSig)>,
    log: Option<LogSnapshot>,
}

impl From<RoundWireV5> for RoundWire {
    fn from(w: RoundWireV5) -> Self {
        RoundWire {
            spec: w.spec,
            spec_id: w.spec_id,
            round_id: w.round_id,
            cfg: w.cfg,
            pool: w.pool,
            draw: w.draw,
            observers: w.observers,
            commits: w.commits,
            reveals: w.reveals,
            log: w.log,
            sealed: None,
        }
    }
}

/// A round a run opened and could not finish (reviews REVLOG-R2, REVLOG-R3-2):
/// kept in the store ([`ObservationResults::keep_unfinished`]) and saved with
/// the round book, so the next run finishes it on the same draw instead of
/// announcing a new one.
#[derive(Clone, Debug, BorshSerialize)]
pub struct UnfinishedRound {
    /// The round as far as it got (commits, the log snapshot, reveals taken).
    pub round: ObservationRound,
    /// The draw as the registry recorded it.
    pub onchain_draw: DrawRecord,
    /// The observers' signed reveals, each with its arrival time
    /// ([`Self::receive_reveal`]; taking one again is harmless).
    pub reveals: Vec<ArrivedReveal>,
}

impl UnfinishedRound {
    /// Take in an observer's signed reveal as it arrives, at `now` by the
    /// coordinator's own clock (ADR-0027 amendment 8 item 4f, review
    /// REVLOG-R4-3). It is kept only when it arrives in the reveal phase
    /// (after `commit_close`, until `reveal_close`), from a drawn observer,
    /// signed by that observer, and is that observer's first; anything else
    /// is a non-reveal (`false`), never an error. Nothing is receipted here:
    /// the caller saves the round with its book after each kept reveal,
    /// BEFORE [`finish_reveals`] signs any receipt; that waits for the
    /// finalized log snapshot and checks each reveal (opens the commit the
    /// round then holds, passes the feed's checks) at its arrival time.
    pub fn receive_reveal(&mut self, reveal: SignedReveal, now: u64) -> bool {
        let r = &self.round;
        let (a, o, salt, sig) = &reveal;
        if now <= r.cfg.commit_close
            || now > r.cfg.reveal_close
            || !r.is_observer(a)
            || self.reveals.iter().any(|k| k.reveal.0 == *a)
            || !reveal_signed(&r.round_id, r.draw.attempt, a, o, salt, sig)
        {
            return false;
        }
        self.reveals.push(ArrivedReveal {
            reveal,
            arrived: now,
        });
        true
    }
}

#[derive(BorshDeserialize)]
struct UnfinishedWire {
    round: RoundWire,
    onchain_draw: DrawRecord,
    reveals: Vec<ArrivedReveal>,
}

/// A version-7 book's unfinished round (ROADMAP C51): taken back only when its
/// sealed snapshot is not time-locked ([`RoundWireV7::upgrade`]).
#[derive(BorshDeserialize)]
struct UnfinishedWireV7 {
    round: RoundWireV7,
    onchain_draw: DrawRecord,
    reveals: Vec<ArrivedReveal>,
}

impl UnfinishedWireV7 {
    fn upgrade(self) -> Option<UnfinishedWire> {
        Some(UnfinishedWire {
            round: self.round.upgrade()?,
            onchain_draw: self.onchain_draw,
            reveals: self.reveals,
        })
    }
}

/// A version-6 book's unfinished round: no time-lock in its sealed snapshot.
#[derive(BorshDeserialize)]
struct UnfinishedWireV6 {
    round: RoundWireV6,
    onchain_draw: DrawRecord,
    reveals: Vec<ArrivedReveal>,
}

impl From<UnfinishedWireV6> for UnfinishedWire {
    fn from(w: UnfinishedWireV6) -> Self {
        UnfinishedWire {
            round: w.round.into(),
            onchain_draw: w.onchain_draw,
            reveals: w.reveals,
        }
    }
}

/// A version-4 or version-5 book's unfinished round: no sealed snapshot.
#[derive(BorshDeserialize)]
struct UnfinishedWireV5 {
    round: RoundWireV5,
    onchain_draw: DrawRecord,
    reveals: Vec<ArrivedReveal>,
}

impl From<UnfinishedWireV5> for UnfinishedWire {
    fn from(w: UnfinishedWireV5) -> Self {
        UnfinishedWire {
            round: w.round.into(),
            onchain_draw: w.onchain_draw,
            reveals: w.reveals,
        }
    }
}

/// A version-3 book's unfinished round (review REVLOG-R3-2): its reveals
/// carry no arrival time. The version-3 runners took every reveal at
/// `commit_close + 1`, so that is the time they are read back with.
#[derive(BorshDeserialize)]
struct UnfinishedWireV3 {
    round: RoundWireV5,
    onchain_draw: DrawRecord,
    reveals: Vec<SignedReveal>,
}

impl From<UnfinishedWireV3> for UnfinishedWire {
    fn from(w: UnfinishedWireV3) -> Self {
        let arrived = w.round.cfg.commit_close.saturating_add(1);
        UnfinishedWire {
            round: w.round.into(),
            onchain_draw: w.onchain_draw,
            reveals: w
                .reveals
                .into_iter()
                .map(|reveal| ArrivedReveal { reveal, arrived })
                .collect(),
        }
    }
}

/// Write `text` to `path` atomically (AUD-2-F9, AUD-2R-F4; review
/// REVLOG-R4-1 for the results stores): a temporary file in the same folder,
/// synced, renamed over the old file, and the folder synced. A crash leaves
/// the old file or the new one, never a torn file.
pub fn write_atomic(path: &std::path::Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => std::path::Path::new("."),
    };
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into())
    ));
    let mut f = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(text.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    // Review REVLOG-R5 (info): the rename is durable only once the folder is
    // synced; a failure here is reported, never ignored.
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| {
            format!(
                "{}: syncing the folder after the rename: {e}",
                dir.display()
            )
        })
}

/// Review R9-F3: one runner per book. An exclusive advisory lock
/// (`flock`) on `<book>.lock`, held while the returned file is open: a
/// second runner on the same book (on this host) fails here instead of
/// racing the first (two runners on one book could each save a book that
/// lacks the other's draw). The OS drops the lock when the process dies, so
/// a crash never leaves a stale lock. It does not cover two hosts sharing
/// a network folder, nor a copy of the book elsewhere (operator error,
/// THREATS REVLOG-R9-F3).
pub fn lock_book(book: &std::path::Path) -> Result<std::fs::File, String> {
    let mut name = book.as_os_str().to_owned();
    name.push(".lock");
    let path = std::path::PathBuf::from(name);
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => Err(format!(
            "{}: another runner holds this book; one runner per book (THREATS REVLOG-R9-F3)",
            path.display()
        )),
        Err(std::fs::TryLockError::Error(e)) => Err(format!("{}: {e}", path.display())),
    }
}

/// `keccak256("onyx-observe-round-v2" ‖ spec_id ‖ borsh(cfg) ‖ borsh(observers)
/// ‖ draw_hash(draw))`: one id per round, so commits and results of one round
/// never count in another round of the same feed (L2R-F6), and the pool
/// snapshot, beacon and attempt the observers were drawn with are bound into
/// every commit and the result hash (L2-F2).
pub fn round_id(
    spec_id: &Hash32,
    cfg: &ObserveConfig,
    observers: &[Observer],
    draw: &DrawSeed,
) -> Hash32 {
    let mut b = b"onyx-observe-round-v2".to_vec();
    b.extend_from_slice(&spec_id.0);
    b.extend_from_slice(&borsh::to_vec(cfg).unwrap_or_default());
    b.extend_from_slice(&borsh::to_vec(observers).unwrap_or_default());
    b.extend_from_slice(&draw_hash(draw).0);
    keccak256(&b)
}

/// An observer's EIP-191 signature `r ‖ s ‖ v` over
/// [`observation_commit_message`] or [`observation_reveal_message`]. Only the
/// canonical form counts (`v` in {27, 28}, low `s`), as at level 3.
pub type ObserverSig = [u8; 65];

/// What an observer signs to post `commit` (ATT-R1-F3). The round id binds the
/// feed, config, observers and draw (pool hash, beacon, attempt); the attempt
/// is spelled out too, so a signature is never valid for another draw of the
/// same fact. The observer's address is the signing key's address: the same
/// address the pool snapshot lists, the draw picks and `drawCommit` binds on
/// chain, so a coordinator cannot post a commit for an observer it does not
/// hold the key of.
pub fn observation_commit_message(
    round_id: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
    commit: &Hash32,
) -> String {
    format!(
        "Onyx observe commit v1\nround: {round_id}\nattempt: {attempt}\nobserver: 0x{}\ncommit: {commit}",
        hex::encode(observer)
    )
}

/// What an observer signs to reveal (ATT-R1-F3): the round, attempt and
/// observer as in [`observation_commit_message`], the observation (its
/// answer, a value or `Unavailable`, with its time and page hash) as
/// `keccak256(borsh(observation))`, and the salt. A coordinator cannot swap
/// or forge an observer's answer: it cannot produce this signature. It CAN
/// withhold a signed reveal, which needs no signature (SIG-R1-F1): so a
/// missing reveal is never slashed. Withholding is caught instead by the
/// on-chain transcript log (ADR-0027 amendment 8): an observer posts its
/// commit (`OnyxRegistry.postCommit`) and the coordinator's
/// [`reveal_receipt_message`] (`postReceipt`), and a client refuses a
/// transcript that leaves either out (`transcriptComplete`).
pub fn observation_reveal_message(
    round_id: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
    observation: &Observation,
    salt: &Hash32,
) -> String {
    let o = keccak256(&borsh::to_vec(observation).unwrap_or_default());
    format!(
        "Onyx observe reveal v1\nround: {round_id}\nattempt: {attempt}\nobserver: 0x{}\nobservation: {o}\nsalt: {salt}",
        hex::encode(observer)
    )
}

/// Whether `sig` is `observer`'s canonical signature of its commit.
pub fn commit_signed(
    round_id: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
    commit: &Hash32,
    sig: &ObserverSig,
) -> bool {
    let msg = observation_commit_message(round_id, attempt, observer, commit);
    eip191_recover(msg.as_bytes(), sig) == Some(*observer)
}

/// Whether `sig` is `observer`'s canonical signature of its reveal.
pub fn reveal_signed(
    round_id: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
    observation: &Observation,
    salt: &Hash32,
    sig: &ObserverSig,
) -> bool {
    let msg = observation_reveal_message(round_id, attempt, observer, observation, salt);
    eip191_recover(msg.as_bytes(), sig) == Some(*observer)
}

/// Sign a commit with the observer's 32-byte secret key (`None`: not a valid
/// key). What an observer runs; the coordinator never holds this key.
pub fn sign_commit(
    secret: &[u8; 32],
    round_id: &Hash32,
    attempt: u32,
    commit: &Hash32,
) -> Option<ObserverSig> {
    let observer = secret_address(secret)?;
    let msg = observation_commit_message(round_id, attempt, &observer, commit);
    eip191_sign_with(secret, msg.as_bytes())
}

/// Sign a reveal with the observer's 32-byte secret key.
pub fn sign_reveal(
    secret: &[u8; 32],
    round_id: &Hash32,
    attempt: u32,
    observation: &Observation,
    salt: &Hash32,
) -> Option<ObserverSig> {
    let observer = secret_address(secret)?;
    let msg = observation_reveal_message(round_id, attempt, &observer, observation, salt);
    eip191_sign_with(secret, msg.as_bytes())
}

/// What the draw's receipt signer (`OnyxRegistry.drawReceiptSigner`, the
/// coordinator's hot key) signs when it accepts `observer`'s reveal opening
/// `commit` in `round_id` (ADR-0027 amendment 8, SIG-R1-F1). It names only
/// the salted commit, never the answer, so the observer can post it on chain
/// at once (`OnyxRegistry.postReceipt`); a transcript of the round must then
/// list that commit and its reveal (`transcriptComplete`). Same lines as
/// [`observation_commit_message`] under its own first line.
pub fn reveal_receipt_message(
    round_id: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
    commit: &Hash32,
) -> String {
    format!(
        "Onyx reveal receipt v1\nround: {round_id}\nattempt: {attempt}\nobserver: 0x{}\ncommit: {commit}",
        hex::encode(observer)
    )
}

/// Sign a reveal receipt with the receipt signer's 32-byte secret key.
pub fn sign_reveal_receipt(
    secret: &[u8; 32],
    round_id: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
    commit: &Hash32,
) -> Option<ObserverSig> {
    let msg = reveal_receipt_message(round_id, attempt, observer, commit);
    eip191_sign_with(secret, msg.as_bytes())
}

/// Whether `sig` is `signer`'s canonical signature of a reveal receipt.
pub fn reveal_receipt_signed(
    signer: &[u8; 20],
    round_id: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
    commit: &Hash32,
    sig: &ObserverSig,
) -> bool {
    let msg = reveal_receipt_message(round_id, attempt, observer, commit);
    eip191_recover(msg.as_bytes(), sig) == Some(*signer)
}

/// `keccak256("onyx-transcript-log-v1" ‖ fact ‖ attempt (4 bytes, big
/// endian) ‖ observer ‖ round)`: the on-chain slot of one observer's posts
/// for one round (`OnyxRegistry.logKey`). Keyed by round, so a post for
/// another round never fills this round's slot.
pub fn transcript_log_key(
    registry_fact: &Hash32,
    attempt: u32,
    observer: &MinerAddress,
    round_id: &Hash32,
) -> Hash32 {
    let mut b = b"onyx-transcript-log-v1".to_vec();
    b.extend_from_slice(&registry_fact.0);
    b.extend_from_slice(&attempt.to_be_bytes());
    b.extend_from_slice(observer);
    b.extend_from_slice(&round_id.0);
    keccak256(&b)
}

/// One drawn observer as a served transcript shows it: its commit
/// (`Hash32::ZERO` when none) and whether its reveal is served (or, for a
/// sealed round, listed as revealed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranscriptEntry {
    /// The drawn observer.
    pub observer: MinerAddress,
    /// Its served commit, zero when none.
    pub commit: Hash32,
    /// Its reveal is served (or listed as revealed).
    pub revealed: bool,
}

/// The arguments of `OnyxRegistry.transcriptComplete` for one served round
/// (ADR-0027 amendment 8): every drawn observer with its served commit and
/// reveal flag, and the round's commit phase. The chain answers false when
/// it logged a commit posted inside the commit phase that the transcript
/// does not serve, or a reveal receipt whose commit and reveal the
/// transcript does not serve: a withheld commit or reveal is caught.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptLogQuery {
    /// The on-chain fact key (`OnyxRegistry.factKeyOf`).
    pub registry_fact: Hash32,
    /// The draw attempt.
    pub attempt: u32,
    /// The round id.
    pub round_id: Hash32,
    /// The window start: commits posted before it are not the round's.
    pub commit_open: u64,
    /// `ObserveConfig::commit_close`.
    pub commit_close: u64,
    /// Every drawn observer, ascending by address.
    pub entries: Vec<TranscriptEntry>,
}

impl TranscriptLogQuery {
    /// The query for a round: `observers` drawn, `commits` served, `revealed`
    /// the observers whose reveal is served.
    #[allow(clippy::too_many_arguments)]
    pub fn of(
        registry_fact: Hash32,
        spec: &ObservationSpec,
        cfg: &ObserveConfig,
        draw: &DrawSeed,
        round_id: Hash32,
        observers: &[Observer],
        commits: &[(MinerAddress, Hash32, ObserverSig)],
        revealed: &BTreeSet<MinerAddress>,
    ) -> Self {
        let served: BTreeMap<MinerAddress, Hash32> =
            commits.iter().map(|(a, h, _)| (*a, *h)).collect();
        TranscriptLogQuery {
            registry_fact,
            attempt: draw.attempt,
            round_id,
            commit_open: spec.window_start,
            commit_close: cfg.commit_close,
            entries: observers
                .iter()
                .map(|o| TranscriptEntry {
                    observer: o.address,
                    commit: served.get(&o.address).copied().unwrap_or(Hash32::ZERO),
                    revealed: revealed.contains(&o.address),
                })
                .collect(),
        }
    }

    /// `entries` packed as the contract reads them: per observer, its 20
    /// bytes, the served commit (32) and the reveal flag (1).
    pub fn packed_entries(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(self.entries.len() * 53);
        for e in &self.entries {
            b.extend_from_slice(&e.observer);
            b.extend_from_slice(&e.commit.0);
            b.push(u8::from(e.revealed));
        }
        b
    }

    /// Calldata of `OnyxRegistry.transcriptComplete(fact, attempt, round,
    /// commitOpen, commitClose, entries)`.
    pub fn calldata(&self) -> Vec<u8> {
        let word = |v: u64| {
            let mut w = [0u8; 32];
            w[24..].copy_from_slice(&v.to_be_bytes());
            w
        };
        let mut d = keccak256(b"transcriptComplete(bytes32,uint32,bytes32,uint64,uint64,bytes)").0
            [..4]
            .to_vec();
        d.extend_from_slice(&self.registry_fact.0);
        d.extend_from_slice(&word(u64::from(self.attempt)));
        d.extend_from_slice(&self.round_id.0);
        d.extend_from_slice(&word(self.commit_open));
        d.extend_from_slice(&word(self.commit_close));
        d.extend_from_slice(&word(6 * 32));
        let packed = self.packed_entries();
        d.extend_from_slice(&word(packed.len() as u64));
        d.extend_from_slice(&packed);
        d.resize(d.len() + (32 - packed.len() % 32) % 32, 0);
        d
    }
}

/// The on-chain transcript log as Rust simulates it (`OnyxRegistry`'s
/// `postCommit`, `postReceipt`, `transcriptComplete`), for tests and the
/// Rust <-> Solidity vectors. Same rules, same keys.
#[derive(Clone, Debug, Default)]
pub struct SimulatedTranscriptLog {
    /// `(fact, attempt)` -> the draw's receipt signer.
    pub signers: BTreeMap<(Hash32, u32), [u8; 20]>,
    commits: BTreeMap<Hash32, (Hash32, u64, ObserverSig)>,
    receipts: BTreeMap<Hash32, Hash32>,
}

impl SimulatedTranscriptLog {
    /// `postCommit`, landing at block time `now`.
    #[allow(clippy::too_many_arguments)]
    pub fn post_commit(
        &mut self,
        fact: &Hash32,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
        now: u64,
    ) -> Result<(), &'static str> {
        if *commit == Hash32::ZERO || *round_id == Hash32::ZERO {
            return Err("ZeroValue");
        }
        if !self.signers.contains_key(&(*fact, attempt)) {
            return Err("NoDraw");
        }
        if !commit_signed(round_id, attempt, observer, commit, sig) {
            return Err("BadSignature");
        }
        let k = transcript_log_key(fact, attempt, observer, round_id);
        if self.commits.contains_key(&k) {
            return Err("AlreadyPosted");
        }
        self.commits.insert(k, (*commit, now, *sig));
        Ok(())
    }

    /// `commitLogOf` with the event's signature: the commit `observer`
    /// posted for `round_id`, if any.
    pub fn posted(
        &self,
        fact: &Hash32,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
    ) -> Option<PostedCommit> {
        self.commits
            .get(&transcript_log_key(fact, attempt, observer, round_id))
            .map(|(commit, posted_at, sig)| PostedCommit {
                commit: *commit,
                sig: *sig,
                posted_at: *posted_at,
            })
    }

    /// `receiptLog`: the commit a posted receipt names, if any.
    pub fn receipt(
        &self,
        fact: &Hash32,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
    ) -> Option<Hash32> {
        self.receipts
            .get(&transcript_log_key(fact, attempt, observer, round_id))
            .copied()
    }

    /// `postReceipt`.
    pub fn post_receipt(
        &mut self,
        fact: &Hash32,
        attempt: u32,
        observer: &MinerAddress,
        round_id: &Hash32,
        commit: &Hash32,
        sig: &ObserverSig,
    ) -> Result<(), &'static str> {
        if *commit == Hash32::ZERO || *round_id == Hash32::ZERO {
            return Err("ZeroValue");
        }
        let signer = self.signers.get(&(*fact, attempt)).ok_or("NoDraw")?;
        if !reveal_receipt_signed(signer, round_id, attempt, observer, commit, sig) {
            return Err("BadSignature");
        }
        let k = transcript_log_key(fact, attempt, observer, round_id);
        if self.receipts.contains_key(&k) {
            return Err("AlreadyPosted");
        }
        self.receipts.insert(k, *commit);
        Ok(())
    }

    /// `transcriptComplete`.
    pub fn complete(&self, q: &TranscriptLogQuery) -> bool {
        if !self.signers.contains_key(&(q.registry_fact, q.attempt)) {
            return false;
        }
        q.entries.iter().all(|e| {
            let k = transcript_log_key(&q.registry_fact, q.attempt, &e.observer, &q.round_id);
            let commit_ok = match self.commits.get(&k) {
                Some((c, at, _)) if (q.commit_open..=q.commit_close).contains(at) => *c == e.commit,
                _ => true,
            };
            let receipt_ok = self
                .receipts
                .get(&k)
                .is_none_or(|c| *c == e.commit && e.revealed);
            commit_ok && receipt_ok
        })
    }
}

/// A fresh commit salt from the operating system's random source
/// (`/dev/urandom`). Each observer draws its own and keeps it until it
/// reveals: a salt anyone can compute (from the round id, say) lets them open
/// a sealed commit by searching the few answers and seconds it can hold
/// (ATT-R2-F2).
pub fn random_salt() -> std::io::Result<Hash32> {
    use std::io::Read;
    let mut b = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(Hash32(b))
}

/// Deterministic FAKE observer keys for tests and recorded rehearsals. Every
/// key is `keccak256` of a public label, so anyone can recompute it: it is
/// not a real key and must never hold anything on any network.
pub mod test_keys {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// Key `keccak256("onyx test observer key " ‖ label ‖ " " ‖ i)`.
    pub fn secret(label: &str, i: u32) -> [u8; 32] {
        keccak256(format!("onyx test observer key {label} {i}").as_bytes()).0
    }

    /// Address of [`secret`]`(label, i)`.
    pub fn address(label: &str, i: u32) -> MinerAddress {
        secret_address(&secret(label, i)).unwrap_or_default()
    }

    /// A key whose address starts with byte `i`, so `ordered(1).1 <
    /// ordered(2).1 < ...` as with the synthetic `[i; 20]` addresses tests
    /// used before signatures: the first `secret("ordered", n)` (n = 0, 1, ...)
    /// whose address has first byte `i`. Cached per `i`.
    pub fn ordered(i: u8) -> ([u8; 32], MinerAddress) {
        type Keys = BTreeMap<u8, ([u8; 32], MinerAddress)>;
        static CACHE: OnceLock<Mutex<Keys>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
        if let Some(k) = cache.lock().ok().and_then(|m| m.get(&i).copied()) {
            return k;
        }
        let found = (0u32..)
            .map(|n| secret("ordered", n))
            .filter_map(|sk| secret_address(&sk).map(|a| (sk, a)))
            .find(|(_, a)| a[0] == i)
            .unwrap_or_default();
        if let Ok(mut m) = cache.lock() {
            m.insert(i, found);
        }
        found
    }

    /// One observer's signed commit and reveal of `observation` with `salt`
    /// in round `round_id` at draw `attempt`, as a transcript lists them.
    #[allow(clippy::type_complexity)]
    pub fn signed(
        secret: &[u8; 32],
        round_id: &Hash32,
        attempt: u32,
        observation: &Observation,
        salt: &Hash32,
    ) -> (
        (MinerAddress, Hash32, ObserverSig),
        (MinerAddress, Observation, Hash32, ObserverSig),
    ) {
        let a = secret_address(secret).unwrap_or_default();
        let h = observation_commit(round_id, &a, observation, salt);
        let cs = sign_commit(secret, round_id, attempt, &h).unwrap_or([0; 65]);
        let rs = sign_reveal(secret, round_id, attempt, observation, salt).unwrap_or([0; 65]);
        ((a, h, cs), (a, observation.clone(), *salt, rs))
    }

    /// The FAKE receipt key of rehearsals and tests (ADR-0027 amendment 8):
    /// `keccak256("onyx test receipt key")`. Public, so it proves nothing on
    /// a real deployment; a live coordinator signs with its own hot key.
    pub fn receipt_secret() -> [u8; 32] {
        keccak256(b"onyx test receipt key").0
    }

    /// Address of [`receipt_secret`].
    pub fn receipt_signer() -> [u8; 20] {
        secret_address(&receipt_secret()).unwrap_or_default()
    }

    /// The [`ordered`] key of `a`, if `a` is an ordered test address.
    pub fn ordered_secret(a: &MinerAddress) -> Option<[u8; 32]> {
        let (sk, x) = ordered(a[0]);
        (x == *a).then_some(sk)
    }
}

/// What a consumer passes to `OnyxRegistry.observationLevel`, built only from
/// its own copy of the registered template and the round's public transcript
/// (REG-R2-F3). Never read the spec id or the result back from the chain or
/// from an API: those are the coordinator's word, and comparing a record with
/// itself always passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnchainObservationCheck {
    /// `onchain::metric_key(template.metric)`.
    pub metric_key: Hash32,
    /// `onchain::subject_key(subject)`.
    pub subject_key: Hash32,
    /// The instantiated window end.
    pub window_end: u64,
    /// The instantiated spec's id.
    pub spec_id: Hash32,
    /// The result hash rebuilt from the verified reveals.
    pub result: Hash32,
    /// The draw's [`pool_hash`] (`recordObservation`'s `pool`).
    pub draw_pool: Hash32,
    /// [`cfg_hash`] of the round config (`recordObservation`'s `cfgHash`).
    pub cfg_hash: Hash32,
}

impl OnchainObservationCheck {
    /// [`draw_commit`] of the record: the commitment the fact's latest draw
    /// must have announced (`drawMatches`, `recordObservation`; L2RND-R3).
    pub fn draw_commit(&self) -> Hash32 {
        draw_commit(&self.draw_pool, &self.spec_id, &self.cfg_hash)
    }
}

/// Why a consumer refuses an on-chain level-2 record before asking the chain.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConsumerCheckError {
    /// The chain registered another template id, or another kind than the
    /// template's own (`MetricKind::of`, REG-R2-F2).
    #[error("the metric is not registered with this template and kind")]
    Registration,
    /// The template does not produce a spec for this subject and window.
    #[error(transparent)]
    Template(#[from] TemplateError),
    /// A reveal is from no assigned observer, appears twice, or does not open
    /// its commit.
    #[error("a reveal does not open an assigned observer's commit")]
    BadReveal,
    /// A commit is from no assigned observer, appears twice, or the commits
    /// are not in strictly ascending address order (L2API-R2-F3).
    #[error("a commit is not one an assigned observer could post")]
    BadCommit,
    /// A commit or reveal is not signed by its observer's key (ATT-R1-F3).
    #[error("a commit or reveal is not signed by its observer")]
    BadSignature,
    /// A revealed observation breaks the reveal-time rule
    /// (`Observation::check`: kind, size, time inside the window, page hash;
    /// L2API-R2-F1).
    #[error("a reveal no round could accept: {0}")]
    BadObservation(String),
    /// The reveals settle no value.
    #[error("the reveals settle no value")]
    NoValue,
    /// The config or observer set is not one a coordinator could open
    /// ([`check_round_params`], L2API-R1-F1).
    #[error("not a round a coordinator could open: {0}")]
    BadRound(String),
}

/// Rebuild, off chain, the arguments of `OnyxRegistry.observationLevel` for
/// `subject` observed from `window_start` (REG-R2-F3): check the on-chain
/// registration (`registered_kind`, `registered_template`) against the
/// consumer's own `template`, instantiate the spec, apply the rules a round
/// must meet to open ([`check_round_params`]), recompute the draw from
/// `pool` and `draw` and refuse observers that are not the drawn set
/// ([`check_draw`], L2-F2), recompute the round id from `cfg`, `observers`
/// and the draw, check every reveal against its commit, and
/// re-aggregate the answers into the result hash. The chain then only has to
/// say whether its one recorded fact equals these values.
#[allow(clippy::too_many_arguments)]
pub fn onchain_observation_check(
    template: &FeedTemplate,
    registered_kind: onyx_core::onchain::MetricKind,
    registered_template: &Hash32,
    subject: &onyx_core::Subject,
    window_start: u64,
    cfg: &ObserveConfig,
    pool: &[Observer],
    draw: &DrawSeed,
    observers: &[Observer],
    commits: &[(MinerAddress, Hash32, ObserverSig)],
    reveals: &[(MinerAddress, Observation, Hash32, ObserverSig)],
) -> Result<OnchainObservationCheck, ConsumerCheckError> {
    let (check, outcome) = rebuild_round(
        template,
        registered_kind,
        registered_template,
        subject,
        window_start,
        cfg,
        pool,
        draw,
        observers,
        commits,
        reveals,
    )?;
    if !matches!(outcome, ObservationOutcome::Value(_)) {
        return Err(ConsumerCheckError::NoValue);
    }
    Ok(check)
}

/// Rebuild a round from its public transcript exactly as
/// [`onchain_observation_check`] does, for ANY outcome: a published round
/// that settled no value (`SourceUnavailable` or `NoResult`) is checked the
/// same way (draw, commits, reveals, aggregation) and returns its outcome and
/// result hash, so a client sees every draw a fact used, not only the one
/// that reached a value (SEC-R2-F1, L2-F2).
#[allow(clippy::too_many_arguments)]
pub fn rebuild_round(
    template: &FeedTemplate,
    registered_kind: onyx_core::onchain::MetricKind,
    registered_template: &Hash32,
    subject: &onyx_core::Subject,
    window_start: u64,
    cfg: &ObserveConfig,
    pool: &[Observer],
    draw: &DrawSeed,
    observers: &[Observer],
    commits: &[(MinerAddress, Hash32, ObserverSig)],
    reveals: &[(MinerAddress, Observation, Hash32, ObserverSig)],
) -> Result<(OnchainObservationCheck, ObservationOutcome), ConsumerCheckError> {
    if registered_kind != onyx_core::onchain::MetricKind::of(template)
        || *registered_template != template.id()
    {
        return Err(ConsumerCheckError::Registration);
    }
    let spec = template.instantiate(subject, window_start)?;
    check_round_params(&spec, cfg, observers)
        .map_err(|e| ConsumerCheckError::BadRound(e.to_string()))?;
    check_draw(&spec, cfg, pool, draw, observers)
        .map_err(|e| ConsumerCheckError::BadRound(e.to_string()))?;
    let spec_id = spec.id();
    let rid = round_id(&spec_id, cfg, observers, draw);
    let assigned: BTreeSet<MinerAddress> = observers.iter().map(|o| o.address).collect();
    // Every commit is from an assigned observer, once, in strictly ascending
    // address order, as `ObservationRound::commit` keeps them (L2API-R2-F3).
    if commits.windows(2).any(|w| w[0].0 >= w[1].0)
        || commits.iter().any(|(a, _, _)| !assigned.contains(a))
    {
        return Err(ConsumerCheckError::BadCommit);
    }
    // ATT-R1-F3: every commit and reveal is signed by its observer's own key,
    // so whoever serves the transcript cannot make up a round (an
    // all-`Unavailable` one, say) the drawn observers never played.
    if commits
        .iter()
        .any(|(a, h, sig)| !commit_signed(&rid, draw.attempt, a, h, sig))
    {
        return Err(ConsumerCheckError::BadSignature);
    }
    let committed: BTreeMap<MinerAddress, Hash32> =
        commits.iter().map(|(a, h, _)| (*a, *h)).collect();
    let mut seen = BTreeSet::new();
    let mut answers = Vec::with_capacity(reveals.len());
    for (a, o, salt, sig) in reveals {
        let opens = committed.get(a) == Some(&observation_commit(&rid, a, o, salt));
        if !assigned.contains(a) || !seen.insert(*a) || !opens {
            return Err(ConsumerCheckError::BadReveal);
        }
        if !reveal_signed(&rid, draw.attempt, a, o, salt, sig) {
            return Err(ConsumerCheckError::BadSignature);
        }
        // The rule `ObservationRound::reveal` applies to every reveal: the
        // answer's kind and size, the observation time inside the window, a
        // page hash for a value (L2API-R2-F1).
        o.check(&spec)
            .map_err(|e| ConsumerCheckError::BadObservation(e.to_string()))?;
        answers.push(o.answer.clone());
    }
    let (outcome, _) = aggregate(spec.kind, observers.len(), &answers);
    let result = result_hash_of(&spec_id, &rid, &outcome);
    Ok((
        OnchainObservationCheck {
            metric_key: onyx_core::onchain::metric_key(&template.metric),
            subject_key: onyx_core::onchain::subject_key(subject),
            window_end: spec.window_end,
            spec_id,
            result,
            draw_pool: draw.pool,
            cfg_hash: cfg_hash(cfg),
        },
        outcome,
    ))
}

/// `ObservationSettlement::result_hash` of (spec id, round id, outcome). Public
/// so a consumer that rebuilt a round from its transcript can bind a served
/// value to the result hash: the hash of `Value(that value)` must equal the
/// result the reveals aggregate to.
pub fn result_hash_of(spec_id: &Hash32, round_id: &Hash32, outcome: &ObservationOutcome) -> Hash32 {
    let mut b = b"onyx-observe-result-v1".to_vec();
    b.extend_from_slice(&borsh::to_vec(&(*spec_id, *round_id, outcome)).unwrap_or_default());
    keccak256(&b)
}

/// Every rule [`ObservationRound::open`] applies before a round exists: the
/// spec is well formed and allowed, the config passes
/// [`ObserveConfig::validate`], there are at least `min_observers` and at most
/// `MAX_OBSERVERS` observers in strictly ascending (so distinct) address
/// order, and every bond covers `outlier_slash`. Public so the API (at load)
/// and clients (on every served round) refuse a round no coordinator could
/// have opened (L2API-R1-F1).
pub fn check_round_params(
    spec: &ObservationSpec,
    cfg: &ObserveConfig,
    observers: &[Observer],
) -> Result<(), ObserveError> {
    spec.validate()?;
    cfg.validate(spec)?;
    if observers.len() < usize::from(spec.min_observers) {
        return Err(ObserveError::TooFewObservers {
            have: observers.len(),
            need: spec.min_observers,
        });
    }
    if observers.len() > usize::from(onyx_core::proof::MAX_OBSERVERS)
        || !observers.windows(2).all(|w| w[0].address < w[1].address)
        || observers.iter().any(|o| o.bond < cfg.outlier_slash)
    {
        return Err(ObserveError::BadObservers);
    }
    Ok(())
}

impl ObservationRound {
    /// Open a round. Refuses a malformed or forbidden feed, bad timing, and
    /// fewer observers than the feed needs. Crate-private: rounds open through
    /// [`ObservationResults::open_round`], which checks the feed template.
    pub(crate) fn open(
        spec: ObservationSpec,
        cfg: ObserveConfig,
        pool: Vec<Observer>,
        draw: DrawSeed,
    ) -> Result<Self, ObserveError> {
        check_pool(&pool)?;
        if pool_hash(&pool) != draw.pool || draw.attempt >= MAX_DRAW_ATTEMPTS {
            return Err(ObserveError::BadPool);
        }
        let spec_id = spec.id();
        let observers = draw_observers(
            &spec_id,
            &pool,
            &draw,
            usize::from(spec.min_observers),
            cfg.outlier_slash,
        );
        if observers.is_empty() {
            return Err(ObserveError::TooFewObservers {
                have: pool.iter().filter(|o| o.bond >= cfg.outlier_slash).count(),
                need: spec.min_observers,
            });
        }
        check_round_params(&spec, &cfg, &observers)?;
        let round_id = round_id(&spec_id, &cfg, &observers, &draw);
        Ok(ObservationRound {
            spec,
            spec_id,
            round_id,
            cfg,
            pool,
            draw,
            observers,
            commits: BTreeMap::new(),
            reveals: BTreeMap::new(),
            log: None,
            sealed: None,
        })
    }

    /// A saved round (review REVLOG-R3-2), re-checked as if it were opened
    /// and played again: the observers are the draw of the pool, the ids
    /// are recomputed, every commit and reveal carries its observer's
    /// signature, every reveal opens its commit and passes the feed's
    /// checks, and a log snapshot names a block past `commit_close` and
    /// holds only drawn observers in order.
    fn restore(w: RoundWire) -> Result<Self, ObserveError> {
        let bad = ObserveError::BadUnfinished;
        check_draw(&w.spec, &w.cfg, &w.pool, &w.draw, &w.observers)?;
        let spec_id = w.spec.id();
        if w.spec_id != spec_id || w.round_id != round_id(&spec_id, &w.cfg, &w.observers, &w.draw) {
            return Err(bad("ids do not match the feed and draw"));
        }
        check_round_params(&w.spec, &w.cfg, &w.observers)?;
        let r = ObservationRound {
            spec: w.spec,
            spec_id,
            round_id: w.round_id,
            cfg: w.cfg,
            pool: w.pool,
            draw: w.draw,
            observers: w.observers,
            commits: w.commits,
            reveals: w.reveals,
            log: w.log,
            sealed: None,
        };
        for (a, (h, sig)) in &r.commits {
            if !r.is_observer(a) || !commit_signed(&r.round_id, r.draw.attempt, a, h, sig) {
                return Err(bad("a commit not signed by a drawn observer"));
            }
        }
        for (a, (o, salt, sig)) in &r.reveals {
            let (h, _) = r.commits.get(a).ok_or(bad("a reveal without a commit"))?;
            if observation_commit(&r.round_id, a, o, salt) != *h
                || !reveal_signed(&r.round_id, r.draw.attempt, a, o, salt, sig)
            {
                return Err(bad("a reveal that does not open its signed commit"));
            }
            o.check(&r.spec)?;
        }
        match &r.log {
            None if !r.reveals.is_empty() => {
                return Err(bad("reveals without a log snapshot"));
            }
            Some(l) => r.check_snapshot(l)?,
            None => {}
        }
        // ROADMAP C11: a saved sealed snapshot is re-checked as when it was
        // taken (shape, key proofs, every dismissal, every in-phase post
        // revealed or dismissed); its reveals are among `reveals`, checked above.
        let mut r = r;
        if let Some(snap) = w.sealed {
            if r.log.is_none() {
                return Err(bad("a sealed snapshot without a log snapshot"));
            }
            r.check_sealed(&snap)?;
            r.sealed = Some(snap);
        }
        Ok(r)
    }

    /// A sealed snapshot's shape (ROADMAP C11): read at a block past
    /// `reveal_close`, slots of drawn observers in order, each post's stored
    /// key proof valid for this round (the registry never holds another,
    /// C4-R3-2), dismissals in order, of in-phase posts only, each checking
    /// against the snapshot's reveal key, of observers with no reveal; and every
    /// in-phase post accounted for, as a reveal the round holds or a dismissal.
    fn check_sealed(&self, snap: &SealedSnapshot) -> Result<(), ObserveError> {
        self.check_sealed_slots(snap)?;
        self.check_dismissals(snap)
    }

    /// [`Self::check_sealed`]'s first half: block, slots and key proofs.
    fn check_sealed_slots(&self, snap: &SealedSnapshot) -> Result<(), ObserveError> {
        let bad = ObserveError::BadSealedSnapshot;
        if snap.block.time <= self.cfg.reveal_close {
            return Err(bad("the block is not past reveal close"));
        }
        if !snap.posts.windows(2).all(|w| w[0].0 < w[1].0)
            || !snap.posts.iter().all(|(a, _)| self.is_observer(a))
        {
            return Err(bad("slots of observers not drawn, or out of order"));
        }
        for (a, p) in &snap.posts {
            if !crate::sealed::key_proof_valid(
                &p.ephemeral,
                &self.round_id,
                self.draw.attempt,
                a,
                &p.key_proof,
            ) {
                return Err(bad(
                    "a post whose key proof fails (not one the registry holds)",
                ));
            }
        }
        Ok(())
    }

    /// [`Self::check_sealed`]'s second half: every dismissal, and every
    /// in-phase post accounted for.
    fn check_dismissals(&self, snap: &SealedSnapshot) -> Result<(), ObserveError> {
        let bad = ObserveError::BadSealedSnapshot;
        let (cc, rc) = (self.cfg.commit_close, self.cfg.reveal_close);
        if !snap.dismissals.windows(2).all(|w| w[0].0 < w[1].0) {
            return Err(bad("dismissals out of order"));
        }
        // ROADMAP C51: a time-locked snapshot's lock round is the window's, its
        // signature the pinned chain's, its openings in order and each of a
        // dismissal (checked with it below). ROADMAP C52: its far round comes
        // after the lock round (the store compares it with the template's when
        // it restores a book, `restore_unfinished`).
        let tl = match &snap.timelock {
            None => None,
            Some(t) => {
                let q = crate::timelock::QuicknetTimeLock::pinned()
                    .map_err(|_| bad("the pinned beacon key"))?;
                use crate::timelock::TimeLock as _;
                if crate::timelock::lock_round(q.chain(), rc) != Some(t.lock_round) {
                    return Err(bad("a time-lock read for another lock round"));
                }
                let rounds = crate::timelock::LockRounds::new(t.lock_round, t.far_round)
                    .map_err(|_| bad("a far round that is not after the lock round"))?;
                if !q.verify_signature(t.lock_round, &t.signature) {
                    return Err(bad("a lock-round signature that does not verify"));
                }
                if !t.openings.windows(2).all(|w| w[0].0 < w[1].0)
                    || !t
                        .openings
                        .iter()
                        .all(|(a, _)| snap.dismissals.iter().any(|(d, _)| d == a))
                {
                    return Err(bad("time-lock openings out of order or of no dismissal"));
                }
                Some((q, t, rounds))
            }
        };
        let in_phase: BTreeMap<&MinerAddress, &PostedSealed> =
            snap.in_phase(cc, rc).map(|(a, p)| (a, p)).collect();
        for (a, d) in &snap.dismissals {
            let p = in_phase
                .get(a)
                .ok_or(bad("a dismissal without an in-phase post"))?;
            if self.reveals.contains_key(a) {
                return Err(bad(
                    "a dismissal of an observer whose reveal the round holds",
                ));
            }
            let ctx = crate::sealed::RevealContext {
                round_id: self.round_id,
                attempt: self.draw.attempt,
                spec: &self.spec,
                commit: self.commits.get(a).map(|(h, _)| *h),
            };
            match &tl {
                None => {
                    crate::sealed::check_dismissal_posted(&snap.reveal_key, a, p, &ctx, d)
                        .map_err(|_| bad("a dismissal that does not check"))?;
                }
                Some((q, t, rounds)) => {
                    let opening = t.openings.iter().find(|(o, _)| o == a).map(|(_, o)| *o);
                    crate::timelock::check_timelocked_dismissal(
                        q,
                        &snap.reveal_key,
                        a,
                        p,
                        &ctx,
                        *rounds,
                        Some(&t.signature),
                        &crate::timelock::TimedDismissal {
                            dismissal: *d,
                            opening,
                        },
                    )
                    .map_err(|_| bad("a time-locked dismissal that does not check"))?;
                }
            }
        }
        let dismissed: BTreeSet<&MinerAddress> = snap.dismissals.iter().map(|(a, _)| a).collect();
        if in_phase
            .keys()
            .any(|a| !self.reveals.contains_key(*a) && !dismissed.contains(a))
        {
            return Err(bad("an in-phase post neither revealed nor dismissed"));
        }
        Ok(())
    }

    /// Open the round's sealed posts as read ONCE (ROADMAP C11, ADR-0027
    /// amendment 10 item 5; [`finish_reveals_sealed`] reads them): `posts` is
    /// every drawn observer's filled slot at `block` (its time past
    /// `reveal_close`, at or below the finalized block), `reveal_key` the
    /// draw's `drawRevealKey`, which must be the public point of
    /// `reveal_secret`. Each post in the reveal phase (`commit_close`,
    /// `reveal_close`] of an observer whose reveal the round does not hold yet
    /// (one taken from the spool is kept) is opened through
    /// [`crate::sealed::open_posted`] (key proof re-checked, C4-R3-2): a valid
    /// reveal is taken as arriving at the post's block time; anything else is
    /// dismissed with a proof, kept in the snapshot. A post outside the phase is
    /// ignored (C4-5). Needs the log snapshot (commits absorbed) first; refused
    /// once a sealed snapshot is kept; all or nothing. Returns the observers
    /// revealed and dismissed here.
    pub fn absorb_sealed(
        &mut self,
        block: LogBlock,
        reveal_key: crate::sealed::Point,
        posts: Vec<(MinerAddress, PostedSealed)>,
        reveal_secret: &[u8; 32],
    ) -> Result<(Vec<MinerAddress>, Vec<MinerAddress>), ObserveError> {
        self.absorb_sealed_locked(block, reveal_key, posts, reveal_secret, None)
    }

    /// [`Self::absorb_sealed`] for a draw that may be TIME-LOCKED (ROADMAP C51,
    /// ADR-0027 amendment 14): with `lock` (the signature of the round's
    /// [`crate::timelock::lock_round`], which must verify against the pinned
    /// quicknet key, there is no other way in, and the round's
    /// [`crate::timelock::LockRounds`], whose far round the caller derived from
    /// the registered template, ROADMAP C52) every in-phase post is opened
    /// through [`crate::timelock::open_timelocked`] (`TL ‖ TF ‖ w ‖ body`, the
    /// time-lock kept only when it re-encrypts byte for byte, the far copy
    /// only when it is exactly the unwrapped body key's), and each dismissal
    /// keeps its time-lock opening when the time-lock opened. The snapshot then
    /// carries the read ([`crate::timelock::TimeLockRead`]). `None`: amendment
    /// 10 posts, as [`Self::absorb_sealed`].
    pub fn absorb_sealed_locked(
        &mut self,
        block: LogBlock,
        reveal_key: crate::sealed::Point,
        posts: Vec<(MinerAddress, PostedSealed)>,
        reveal_secret: &[u8; 32],
        lock: Option<(&[u8], crate::timelock::LockRounds)>,
    ) -> Result<(Vec<MinerAddress>, Vec<MinerAddress>), ObserveError> {
        let bad = ObserveError::BadSealedSnapshot;
        if self.sealed.is_some() {
            return Err(ObserveError::SealedAlreadyRead);
        }
        if self.log.is_none() {
            return Err(bad(
                "the transcript log was not read: no sealed post before the commits",
            ));
        }
        if crate::sealed::public_point(reveal_secret).ok() != Some(reveal_key) {
            return Err(bad("the draw's reveal key is not this coordinator's"));
        }
        let mut next = self.clone();
        let (cc, rc) = (self.cfg.commit_close, self.cfg.reveal_close);
        let locked = match lock {
            None => None,
            Some((sig, rounds)) => {
                use crate::timelock::TimeLock as _;
                let q = crate::timelock::QuicknetTimeLock::pinned()
                    .map_err(|_| bad("the pinned beacon key"))?;
                let rho = crate::timelock::lock_round(q.chain(), rc)
                    .ok_or(bad("no lock round for this window"))?;
                if rounds.lock() != rho {
                    return Err(bad("lock rounds for another window"));
                }
                if !q.verify_signature(rho, sig) {
                    return Err(bad("not the pinned chain's signature of the lock round"));
                }
                Some((q, rounds, sig.to_vec()))
            }
        };
        let mut snap = SealedSnapshot {
            block,
            reveal_key,
            posts,
            dismissals: vec![],
            timelock: locked
                .as_ref()
                .map(|(_, rounds, sig)| crate::timelock::TimeLockRead {
                    lock_round: rounds.lock(),
                    far_round: rounds.far(),
                    signature: sig.clone(),
                    openings: vec![],
                }),
        };
        // Shape and key proofs first: no post is opened before every one checks.
        next.check_sealed_slots(&snap)?;
        let mut revealed = vec![];
        let in_phase: Vec<(MinerAddress, PostedSealed)> = snap.in_phase(cc, rc).cloned().collect();
        for (a, p) in in_phase {
            if next.reveals.contains_key(&a) {
                continue;
            }
            let ctx = crate::sealed::RevealContext {
                round_id: next.round_id,
                attempt: next.draw.attempt,
                spec: &next.spec,
                commit: next.commits.get(&a).map(|(h, _)| *h),
            };
            let Some((q, rounds, sig)) = &locked else {
                match crate::sealed::open_posted(reveal_secret, &a, &p, &ctx)
                    .map_err(|_| bad("a post that cannot be opened"))?
                {
                    crate::sealed::Opened::Reveal((o, obs, salt, sig)) => {
                        next.reveal(p.posted_at, o, obs, salt, sig)?;
                        revealed.push(a);
                    }
                    crate::sealed::Opened::Dismissed(d) => snap.dismissals.push((a, d)),
                }
                continue;
            };
            match crate::timelock::open_timelocked(
                q,
                reveal_secret,
                Some(sig),
                &a,
                &p,
                &ctx,
                *rounds,
            )
            .map_err(|_| bad("a time-locked post that cannot be opened"))?
            {
                crate::timelock::TimedOpened::Reveal((o, obs, salt, sig)) => {
                    next.reveal(p.posted_at, o, obs, salt, sig)?;
                    revealed.push(a);
                }
                crate::timelock::TimedOpened::Dismissed(d) => {
                    snap.dismissals.push((a, d.dismissal));
                    if let (Some(o), Some(t)) = (d.opening, snap.timelock.as_mut()) {
                        t.openings.push((a, o));
                    }
                }
            }
        }
        let dismissed = snap.dismissals.iter().map(|(a, _)| *a).collect();
        next.check_sealed(&snap)?;
        next.sealed = Some(snap);
        *self = next;
        Ok((revealed, dismissed))
    }

    /// The round's sealed snapshot, once read (ROADMAP C11).
    pub fn sealed_snapshot(&self) -> Option<&SealedSnapshot> {
        self.sealed.as_ref()
    }

    /// The `revealLogComplete` / `attemptMatches` query of this round as it is
    /// published (ROADMAP C11): every drawn observer, its commit, and its
    /// status (revealed, dismissed in the sealed snapshot, or neither), the
    /// window of the round's config and its round id.
    pub fn reveal_log_query(&self, registry_fact: Hash32) -> crate::sealed::RevealLogQuery {
        let dismissed: BTreeSet<MinerAddress> = self
            .sealed
            .iter()
            .flat_map(|s| s.dismissals.iter().map(|(a, _)| *a))
            .collect();
        crate::sealed::reveal_log_query(
            registry_fact,
            &self.spec,
            &self.cfg,
            self.draw.attempt,
            self.round_id,
            &self.observers,
            &self.signed_commits(),
            &self.reveals.keys().copied().collect(),
            &dismissed,
        )
    }

    /// The log snapshot's own shape (review REVLOG-R3-1).
    fn check_snapshot(&self, snap: &LogSnapshot) -> Result<(), ObserveError> {
        let bad = ObserveError::BadLogSnapshot;
        if snap.block.is_some_and(|b| b.time <= self.cfg.commit_close) {
            return Err(bad("the block is not past commit close"));
        }
        if snap.block.is_none() && !snap.posted.is_empty() {
            return Err(bad("posts without a block"));
        }
        if !snap.posted.windows(2).all(|w| w[0].0 < w[1].0)
            || !snap.posted.iter().all(|(a, _)| self.is_observer(a))
        {
            return Err(bad("slots of observers not drawn, or out of order"));
        }
        Ok(())
    }

    /// Absorb the transcript log as read ONCE, from one block (review
    /// REVLOG-R3-1; [`absorb_posted_commits`] reads it): every commit posted
    /// inside the commit phase replaces the observer's own
    /// ([`absorb_posted_commit`](Self::absorb_posted_commit)), one posted
    /// outside it is ignored (`transcriptComplete` ignores it too), and the
    /// snapshot is kept in the round. Refused once a snapshot is kept
    /// ([`ObserveError::LogAlreadyRead`]) or after a reveal was taken; all or
    /// nothing. Returns the observers whose posted commit was absorbed.
    pub fn absorb_log(&mut self, snap: LogSnapshot) -> Result<Vec<MinerAddress>, ObserveError> {
        if self.log.is_some() {
            return Err(ObserveError::LogAlreadyRead);
        }
        if !self.reveals.is_empty() {
            return Err(ObserveError::BadLogSnapshot(
                "reveals were taken before the log was read",
            ));
        }
        self.check_snapshot(&snap)?;
        let mut next = self.clone();
        let mut absorbed = vec![];
        for (a, p) in &snap.posted {
            match next.absorb_posted_commit(p.posted_at, *a, p.commit, p.sig) {
                Ok(()) => absorbed.push(*a),
                Err(ObserveError::WrongPhase) => {}
                Err(e) => return Err(e),
            }
        }
        next.log = Some(snap);
        *self = next;
        Ok(absorbed)
    }

    /// The round's log snapshot, once read (review REVLOG-R3-1).
    pub fn log_snapshot(&self) -> Option<&LogSnapshot> {
        self.log.as_ref()
    }

    /// The observers whose post in `snap` lies in the commit phase.
    fn absorbed_of(&self, snap: &LogSnapshot) -> Vec<MinerAddress> {
        snap.posted
            .iter()
            .filter(|(_, p)| {
                p.posted_at >= self.spec.window_start && p.posted_at <= self.cfg.commit_close
            })
            .map(|(a, _)| *a)
            .collect()
    }

    /// The feed id.
    pub fn spec_id(&self) -> Hash32 {
        self.spec_id
    }

    /// The round id: what every commit of this round is bound to.
    pub fn round_id(&self) -> Hash32 {
        self.round_id
    }

    /// The drawn observers, ascending by address.
    pub fn observers(&self) -> &[Observer] {
        &self.observers
    }

    /// The pool snapshot the observers were drawn from.
    pub fn pool(&self) -> &[Observer] {
        &self.pool
    }

    /// The draw's public inputs.
    pub fn draw(&self) -> DrawSeed {
        self.draw
    }

    /// The feed spec.
    pub fn spec(&self) -> &ObservationSpec {
        &self.spec
    }

    /// Timing and economics.
    pub fn config(&self) -> ObserveConfig {
        self.cfg
    }

    fn is_observer(&self, a: &MinerAddress) -> bool {
        self.observers
            .binary_search_by(|o| o.address.cmp(a))
            .is_ok()
    }

    /// Every commit posted, ascending by observer: observer, hash, signature.
    pub fn signed_commits(&self) -> Vec<(MinerAddress, Hash32, ObserverSig)> {
        self.commits
            .iter()
            .map(|(a, (h, s))| (*a, *h, *s))
            .collect()
    }

    /// Every reveal accepted, ascending by observer: observer, observation,
    /// salt, signature.
    pub fn signed_reveals(&self) -> Vec<(MinerAddress, Observation, Hash32, ObserverSig)> {
        self.reveals
            .iter()
            .map(|(a, (o, salt, sig))| (*a, o.clone(), *salt, *sig))
            .collect()
    }

    /// Take a commit the observer posted on chain (`OnyxRegistry.postCommit`,
    /// ADR-0027 amendment 8) at block time `posted_at`. One posted inside the
    /// commit phase (from the window start to `commit_close`) must be in the
    /// transcript, so it replaces any commit the observer sent off chain
    /// (and drops a reveal of that one). The coordinator reads the log once
    /// the chain's time passes `commit_close`, before it takes reveals.
    pub fn absorb_posted_commit(
        &mut self,
        posted_at: u64,
        observer: MinerAddress,
        hash: Hash32,
        sig: ObserverSig,
    ) -> Result<(), ObserveError> {
        if self.log.is_some() {
            return Err(ObserveError::LogAlreadyRead);
        }
        if !self.is_observer(&observer) {
            return Err(ObserveError::NotObserver);
        }
        if posted_at < self.spec.window_start || posted_at > self.cfg.commit_close {
            return Err(ObserveError::WrongPhase);
        }
        if !commit_signed(&self.round_id, self.draw.attempt, &observer, &hash, &sig) {
            return Err(ObserveError::BadSignature);
        }
        if self.commits.get(&observer).map(|(h, _)| *h) != Some(hash) {
            self.reveals.remove(&observer);
        }
        self.commits.insert(observer, (hash, sig));
        Ok(())
    }

    /// The receipt for `observer`'s accepted reveal, signed with the draw's
    /// receipt key ([`reveal_receipt_message`]); `None` when its reveal was
    /// not accepted. The coordinator hands it back at once; the observer
    /// posts it on chain (`postReceipt`), and from then on a transcript
    /// without that reveal is refused.
    pub fn reveal_receipt(
        &self,
        receipt_key: &[u8; 32],
        observer: &MinerAddress,
    ) -> Option<ObserverSig> {
        self.reveals.get(observer)?;
        let (commit, _) = self.commits.get(observer)?;
        sign_reveal_receipt(
            receipt_key,
            &self.round_id,
            self.draw.attempt,
            observer,
            commit,
        )
    }

    /// The `transcriptComplete` query of this round as it would be published
    /// with every accepted reveal (`registry_fact`: the on-chain fact key).
    pub fn transcript_query(&self, registry_fact: Hash32) -> TranscriptLogQuery {
        TranscriptLogQuery::of(
            registry_fact,
            &self.spec,
            &self.cfg,
            &self.draw,
            self.round_id,
            &self.observers,
            &self.signed_commits(),
            &self.reveals.keys().copied().collect(),
        )
    }

    /// Post a commit (from the window start until `commit_close`), signed by
    /// the observer's key ([`observation_commit_message`], ATT-R1-F3). The
    /// same hash posted by two observers is not refused: the address is inside
    /// the commit, so a copied hash can never be revealed by the copier, and
    /// refusing it would let an observer who saw a hash first block its owner
    /// (L2R-F5).
    pub fn commit(
        &mut self,
        now: u64,
        observer: MinerAddress,
        hash: Hash32,
        sig: ObserverSig,
    ) -> Result<(), ObserveError> {
        if !self.is_observer(&observer) {
            return Err(ObserveError::NotObserver);
        }
        if now < self.spec.window_start || now > self.cfg.commit_close {
            return Err(ObserveError::WrongPhase);
        }
        if self.commits.contains_key(&observer) {
            return Err(ObserveError::AlreadyCommitted);
        }
        if !commit_signed(&self.round_id, self.draw.attempt, &observer, &hash, &sig) {
            return Err(ObserveError::BadSignature);
        }
        self.commits.insert(observer, (hash, sig));
        Ok(())
    }

    /// Reveal (after `commit_close`, until `reveal_close`). Nobody's answer is
    /// visible before every commit is closed.
    pub fn reveal(
        &mut self,
        now: u64,
        observer: MinerAddress,
        observation: Observation,
        salt: Hash32,
        sig: ObserverSig,
    ) -> Result<(), ObserveError> {
        if now <= self.cfg.commit_close || now > self.cfg.reveal_close {
            return Err(ObserveError::WrongPhase);
        }
        let (commit, _) = self.commits.get(&observer).ok_or(ObserveError::NoCommit)?;
        if self.reveals.contains_key(&observer) {
            return Err(ObserveError::AlreadyRevealed);
        }
        if observation_commit(&self.round_id, &observer, &observation, &salt) != *commit {
            return Err(ObserveError::HashMismatch);
        }
        if !reveal_signed(
            &self.round_id,
            self.draw.attempt,
            &observer,
            &observation,
            &salt,
            &sig,
        ) {
            return Err(ObserveError::BadSignature);
        }
        observation.check(&self.spec)?;
        self.reveals.insert(observer, (observation, salt, sig));
        Ok(())
    }

    /// Settle after `reveal_close`. Crate-private (L2R3-F5): rounds settle only
    /// through [`ObservationResults::settle`], which records each result once.
    pub(crate) fn settle(&self, now: u64) -> Result<ObservationSettlement, ObserveError> {
        if now <= self.cfg.reveal_close {
            return Err(ObserveError::WrongPhase);
        }
        let answers: Vec<Answer> = self
            .reveals
            .values()
            .map(|(o, _, _)| o.answer.clone())
            .collect();
        let (outcome, center) = aggregate(self.spec.kind, self.observers.len(), &answers);
        let mut statuses = Vec::with_capacity(self.observers.len());
        let mut entries = Vec::new();
        for o in &self.observers {
            let status = match (self.reveals.get(&o.address).map(|(r, _, _)| r), &outcome) {
                (None, _) if self.commits.contains_key(&o.address) => ObserverStatus::Unrevealed,
                (None, _) => ObserverStatus::Absent,
                // L2R-F2: an outage pays nobody. Saying `Unavailable` costs no
                // fetch and leaves no evidence, so paying it would make it a
                // free answer for a lazy majority.
                (Some(r), ObservationOutcome::SourceUnavailable) => {
                    if r.answer == Answer::Unavailable
                        || corroborated(self.spec.kind, &answers, &r.answer)
                    {
                        // L2R3-F4: a value at least one other observer
                        // matched (within tolerance) is evidence the source
                        // was up, so it is not slashed: a lazy majority saying
                        // `Unavailable` cannot slash the honest minority that
                        // fetched. Still nobody is paid for an outage (L2R-F2).
                        ObserverStatus::NoCall
                    } else {
                        // L2R2-F4: a lone value while the majority found the
                        // source down (a stale guess) costs the small slash,
                        // so guessing never beats an honest `Unavailable`.
                        ObserverStatus::Missed
                    }
                }
                (Some(r), ObservationOutcome::NoResult) => {
                    if far_from_cluster(
                        self.spec.kind,
                        self.observers.len(),
                        &answers,
                        center,
                        &r.answer,
                    ) {
                        ObserverStatus::Outlier
                    } else {
                        ObserverStatus::NoCall
                    }
                }
                (Some(r), ObservationOutcome::Value(v)) => {
                    if r.answer == Answer::Unavailable {
                        ObserverStatus::Missed
                    } else {
                        match judge(self.spec.kind, v, &r.answer, center) {
                            Judgment::Agrees => ObserverStatus::Agreed,
                            Judgment::Near => ObserverStatus::NoCall,
                            Judgment::Disagrees => ObserverStatus::Outlier,
                        }
                    }
                }
            };
            let delta: i128 = match status {
                ObserverStatus::Agreed => to_i128(self.cfg.reward),
                ObserverStatus::Outlier => -to_i128(self.cfg.outlier_slash.min(o.bond)),
                ObserverStatus::Missed => -to_i128(self.cfg.miss_slash.min(o.bond)),
                // SIG-R1-F1: never a slash on the coordinator's word alone.
                ObserverStatus::NoCall | ObserverStatus::Unrevealed | ObserverStatus::Absent => 0,
            };
            statuses.push((o.address, status));
            if delta != 0 {
                entries.push((o.address, delta));
            }
        }
        Ok(ObservationSettlement {
            spec_id: self.spec_id,
            round_id: self.round_id,
            outcome,
            statuses,
            entries,
        })
    }
}

/// L2R3-F4: `answer` is a value and at least one OTHER revealed value agrees
/// with it (identical bytes, or a number within `tolerance`). `answers` holds
/// every reveal, including `answer` itself.
fn corroborated(kind: ObservationKind, answers: &[Answer], answer: &Answer) -> bool {
    let matching = answers
        .iter()
        .filter(|a| match (kind, *a, answer) {
            (ObservationKind::Exact { .. }, Answer::Bytes(x), Answer::Bytes(y)) => x == y,
            (ObservationKind::Numeric { tolerance, .. }, Answer::Number(x), Answer::Number(y)) => {
                x.abs_diff(*y) <= tolerance
            }
            _ => false,
        })
        .count();
    // `answer` matches itself, so a second match is another observer's value.
    matching >= 2
}

/// L2R2-F5: in a numeric round with no result, a far number is still judged
/// when the honest cluster is identifiable: a strict majority of ALL assigned
/// observers revealed numbers within twice the tolerance of the center. A
/// number beyond twice the tolerance is then an outlier, so a minority cannot
/// force `NoResult` for free when honest answers drift between one and two
/// tolerances apart. With no such majority (an honest split) nobody is judged.
/// Exact feeds and `Unavailable` answers are never judged here.
fn far_from_cluster(
    kind: ObservationKind,
    members: usize,
    answers: &[Answer],
    center: i128,
    answer: &Answer,
) -> bool {
    let ObservationKind::Numeric { tolerance, .. } = kind else {
        return false;
    };
    let Answer::Number(a) = answer else {
        return false;
    };
    let near = tolerance.saturating_mul(2);
    let cluster = answers
        .iter()
        .filter(|x| matches!(x, Answer::Number(n) if n.abs_diff(center) <= near))
        .count();
    cluster >= quorum(members) && a.abs_diff(center) > near
}

/// Amounts are checked at open to fit `i128` (`ObserveConfig::validate`), so
/// this never saturates on a valid round.
fn to_i128(v: u128) -> i128 {
    i128::try_from(v).unwrap_or(i128::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_core::{DataRights, ObservationKind, Redistribution, SourceId};

    const WINDOW: (u64, u64) = (1_000, 1_050);
    const SALT: Hash32 = Hash32([7; 32]);

    fn spec(kind: ObservationKind) -> ObservationSpec {
        ObservationSpec {
            feed: "example.page.value".into(),
            metric: onyx_core::MetricId("example.page.value".into()),
            subject: onyx_core::Subject::Entity("entity:example-exchange".into()),
            fetcher: keccak256(b"fetcher"),
            url: "https://example.org/page.json".into(),
            selector: "/data/value".into(),
            kind,
            window_start: WINDOW.0,
            window_end: WINDOW.1,
            min_observers: 3,
            rights: DataRights {
                license: "CC0-1.0".into(),
                redistribution: Redistribution::Allowed,
                source: SourceId("example.page".into()),
                attribution: None,
                terms: String::new(),
            },
        }
    }

    const NUM: ObservationKind = ObservationKind::Numeric {
        scale: 2,
        tolerance: 50,
    };
    const EXACT: ObservationKind = ObservationKind::Exact { max_len: 16 };

    /// A store with `spec(kind)`'s template registered (L2SRC-R2-F1).
    fn store_for(kind: ObservationKind) -> ObservationResults {
        store_for_n(kind, 3)
    }

    /// A store whose template draws `n` observers.
    fn store_for_n(kind: ObservationKind, n: u16) -> ObservationResults {
        let mut s = spec(kind);
        s.min_observers = n;
        let (mut store, admin) = ObservationResults::new();
        store
            .register_feed(&admin, template_of_spec(&s), "test feed", 0)
            .expect("register");
        store
    }

    fn template_of_spec(s: &ObservationSpec) -> FeedTemplate {
        FeedTemplate {
            feed: s.feed.clone(),
            metric: s.metric.clone(),
            fetcher: s.fetcher,
            url_template: s.url.clone(),
            selector: s.selector.clone(),
            kind: s.kind,
            window_secs: s.window_end - s.window_start,
            min_observers: s.min_observers,
            rights: s.rights.clone(),
            subject: onyx_core::SubjectRule::Fixed(s.subject.clone()),
        }
    }

    fn cfg() -> ObserveConfig {
        ObserveConfig {
            commit_close: 1_100,
            reveal_close: 1_200,
            reward: 10,
            outlier_slash: 100,
            miss_slash: 20,
        }
    }

    /// Test observer `i`: a fake key whose address starts with byte `i`
    /// (`test_keys::ordered`), so `addr(1) < addr(2) < ...`.
    fn addr(i: u8) -> MinerAddress {
        test_keys::ordered(i).1
    }

    /// Commit and reveal signed with the test key of the observer (an
    /// all-zero signature for an address that has none).
    impl ObservationRound {
        fn commit_as(
            &mut self,
            now: u64,
            observer: MinerAddress,
            hash: Hash32,
        ) -> Result<(), ObserveError> {
            let sig = test_keys::ordered_secret(&observer)
                .and_then(|sk| sign_commit(&sk, &self.round_id, self.draw.attempt, &hash))
                .unwrap_or([0; 65]);
            self.commit(now, observer, hash, sig)
        }

        fn reveal_as(
            &mut self,
            now: u64,
            observer: MinerAddress,
            observation: Observation,
            salt: Hash32,
        ) -> Result<(), ObserveError> {
            let sig = test_keys::ordered_secret(&observer)
                .and_then(|sk| {
                    sign_reveal(&sk, &self.round_id, self.draw.attempt, &observation, &salt)
                })
                .unwrap_or([0; 65]);
            self.reveal(now, observer, observation, salt, sig)
        }
    }

    fn beacon() -> Hash32 {
        keccak256(b"test beacon")
    }

    /// The draw of a pool that is exactly the intended observers.
    fn seed_of(pool: &[Observer]) -> DrawSeed {
        DrawSeed {
            pool: pool_hash(pool),
            beacon_height: 10,
            beacon: beacon(),
            attempt: 0,
        }
    }

    /// Open a round whose pool is `obs` (all drawn when the spec needs that many).
    fn open_all(
        spec: ObservationSpec,
        cfg: ObserveConfig,
        obs: Vec<Observer>,
    ) -> Result<ObservationRound, ObserveError> {
        let seed = seed_of(&obs);
        ObservationRound::open(spec, cfg, obs, seed)
    }

    /// Announce, then open, a round in `store` from pool `pool`.
    fn open_in(
        store: &mut ObservationResults,
        spec: ObservationSpec,
        cfg: ObserveConfig,
        pool: Vec<Observer>,
    ) -> Result<ObservationRound, ObserveError> {
        let snapshot = store.announce_draw(&spec, &cfg, &pool, 10, &SimulatedChain::at(0))?;
        store.open_round(spec, cfg, snapshot, &SimulatedChain::with_beacon(beacon()))
    }

    fn round(kind: ObservationKind, n: u8) -> ObservationRound {
        let obs = (1..=n)
            .map(|i| Observer {
                address: addr(i),
                bond: 1_000,
            })
            .collect();
        let mut s = spec(kind);
        s.min_observers = u16::from(n);
        open_all(s, cfg(), obs).expect("open")
    }

    fn obs(answer: Answer) -> Observation {
        let page_hash = if answer == Answer::Unavailable {
            Hash32::ZERO
        } else {
            keccak256(b"page")
        };
        Observation {
            answer,
            observed_at: 1_030,
            page_hash,
        }
    }

    /// Each observer commits at 1_070 and reveals at 1_150.
    fn play(r: &mut ObservationRound, answers: &[(u8, Answer)]) {
        for (i, a) in answers {
            let h = observation_commit(&r.round_id(), &addr(*i), &obs(a.clone()), &SALT);
            r.commit_as(1_070, addr(*i), h).expect("commit");
        }
        for (i, a) in answers {
            r.reveal_as(1_150, addr(*i), obs(a.clone()), SALT)
                .expect("reveal");
        }
    }

    fn status_of(s: &ObservationSettlement, i: u8) -> ObserverStatus {
        s.statuses()
            .iter()
            .find(|(a, _)| *a == addr(i))
            .map(|(_, st)| *st)
            .expect("status")
    }

    #[test]
    fn honest_majority_settles_the_median_and_slashes_the_minority_liar() {
        let mut r = round(NUM, 5);
        play(
            &mut r,
            &[
                (1, Answer::Number(6_412_345)),
                (2, Answer::Number(6_412_350)),
                (3, Answer::Number(6_412_340)),
                (4, Answer::Number(6_412_349)),
                (5, Answer::Number(9_000_000)), // liar
            ],
        );
        let s = r.settle(1_201).expect("settle");
        assert_eq!(
            *s.outcome(),
            ObservationOutcome::Value(Answer::Number(6_412_345))
        );
        for i in 1..=4 {
            assert_eq!(status_of(&s, i), ObserverStatus::Agreed);
        }
        assert_eq!(status_of(&s, 5), ObserverStatus::Outlier);
        assert_eq!(
            s.entries(),
            [
                (addr(1), 10),
                (addr(2), 10),
                (addr(3), 10),
                (addr(4), 10),
                (addr(5), -100)
            ]
        );
        assert_ne!(s.result_hash(), Hash32::ZERO);
    }

    #[test]
    fn exact_feed_needs_identical_bytes_from_a_strict_majority() {
        let mut r = round(EXACT, 3);
        play(
            &mut r,
            &[
                (1, Answer::Bytes(b"open".to_vec())),
                (2, Answer::Bytes(b"open".to_vec())),
                (3, Answer::Bytes(b"closed".to_vec())),
            ],
        );
        let s = r.settle(1_201).expect("settle");
        assert_eq!(
            *s.outcome(),
            ObservationOutcome::Value(Answer::Bytes(b"open".to_vec()))
        );
        assert_eq!(status_of(&s, 3), ObserverStatus::Outlier);
    }

    #[test]
    fn copier_cannot_reveal_a_copied_commit_or_change_its_answer_late() {
        let mut r = round(EXACT, 3);
        let honest = obs(Answer::Bytes(b"open".to_vec()));
        let h1 = observation_commit(&r.round_id(), &addr(1), &honest, &SALT);
        r.commit_as(1_070, addr(1), h1).expect("commit 1");
        // The copier posts observer 1's hash, hoping to reveal whatever wins.
        // Accepted (L2R-F5), but the hash binds observer 1's address, so the
        // copier can never open it.
        r.commit_as(1_071, addr(3), h1).expect("copied hash");
        let h2 = observation_commit(&r.round_id(), &addr(2), &honest, &SALT);
        r.commit_as(1_073, addr(2), h2).expect("commit 2");
        // No reveal is visible before commits close.
        assert_eq!(
            r.reveal_as(1_090, addr(1), honest.clone(), SALT),
            Err(ObserveError::WrongPhase)
        );
        r.reveal_as(1_150, addr(1), honest.clone(), SALT)
            .expect("reveal 1");
        r.reveal_as(1_150, addr(2), honest.clone(), SALT)
            .expect("reveal 2");
        // Having seen the answer, the copier tries to reveal it: it does not
        // match its commit, and after reveal_close nothing is accepted at all.
        assert_eq!(
            r.reveal_as(1_160, addr(3), honest.clone(), SALT),
            Err(ObserveError::HashMismatch)
        );
        assert_eq!(
            r.reveal_as(1_201, addr(3), honest.clone(), SALT),
            Err(ObserveError::WrongPhase)
        );
        let s = r.settle(1_201).expect("settle");
        assert_eq!(status_of(&s, 3), ObserverStatus::Unrevealed);
        // SIG-R1-F1: no slash on the coordinator's word alone.
        assert!(s.entries().iter().all(|(a, _)| *a != addr(3)));
    }

    #[test]
    fn source_unavailable_consensus_slashes_nobody_who_said_so() {
        let mut r = round(NUM, 5);
        play(
            &mut r,
            &[
                (1, Answer::Unavailable),
                (2, Answer::Unavailable),
                (3, Answer::Unavailable),
                (4, Answer::Number(6_412_345)),
            ],
        );
        let s = r.settle(1_201).expect("settle");
        assert_eq!(*s.outcome(), ObservationOutcome::SourceUnavailable);
        // L2R-F2: nobody is paid for an outage; nobody who said so is slashed.
        for i in 1..=3 {
            assert_eq!(status_of(&s, i), ObserverStatus::NoCall);
        }
        // L2R2-F4: a value while the majority found the source down costs the
        // small slash. A silent member is absent, not slashed (SIG-R1-F1).
        assert_eq!(status_of(&s, 4), ObserverStatus::Missed);
        assert_eq!(status_of(&s, 5), ObserverStatus::Absent);
        assert_eq!(s.entries(), [(addr(4), -20)]);
    }

    #[test]
    fn unavailable_while_a_value_settles_costs_the_small_slash() {
        let mut r = round(EXACT, 3);
        play(
            &mut r,
            &[
                (1, Answer::Bytes(b"open".to_vec())),
                (2, Answer::Bytes(b"open".to_vec())),
                (3, Answer::Unavailable),
            ],
        );
        let s = r.settle(1_201).expect("settle");
        assert_eq!(status_of(&s, 3), ObserverStatus::Missed);
        assert!(s.entries().contains(&(addr(3), -20)));
    }

    #[test]
    fn a_tie_or_a_plurality_is_no_result_and_judges_no_revealer() {
        // 4 observers, 2 vs 2: no strict majority of 4.
        let mut r = round(EXACT, 4);
        play(
            &mut r,
            &[
                (1, Answer::Bytes(b"open".to_vec())),
                (2, Answer::Bytes(b"open".to_vec())),
                (3, Answer::Bytes(b"closed".to_vec())),
                (4, Answer::Bytes(b"closed".to_vec())),
            ],
        );
        let s = r.settle(1_201).expect("settle");
        assert_eq!(*s.outcome(), ObservationOutcome::NoResult);
        assert!(s
            .statuses()
            .iter()
            .all(|(_, st)| *st == ObserverStatus::NoCall));
        assert!(s.entries().is_empty());
    }

    #[test]
    fn silent_observers_never_let_a_minority_decide() {
        // 5 assigned, only 2 reveal (and agree): 2 < quorum(5) = 3.
        let mut r = round(NUM, 5);
        play(
            &mut r,
            &[(1, Answer::Number(100)), (2, Answer::Number(100))],
        );
        let s = r.settle(1_201).expect("settle");
        assert_eq!(*s.outcome(), ObservationOutcome::NoResult);
        assert_eq!(status_of(&s, 1), ObserverStatus::NoCall);
        assert_eq!(status_of(&s, 3), ObserverStatus::Absent);
        // SIG-R1-F1: silence is not slashed on the coordinator's word.
        assert!(s.entries().is_empty());
    }

    #[test]
    fn too_few_observers_never_open() {
        let obs = vec![
            Observer {
                address: addr(1),
                bond: 1_000,
            },
            Observer {
                address: addr(2),
                bond: 1_000,
            },
        ];
        assert_eq!(
            open_all(spec(NUM), cfg(), obs).err(),
            Some(ObserveError::TooFewObservers { have: 2, need: 3 })
        );
    }

    #[test]
    fn open_checks_order_bonds_and_timing() {
        let three = |bond| -> Vec<Observer> {
            (1..=3)
                .map(|i| Observer {
                    address: addr(i),
                    bond,
                })
                .collect()
        };
        assert_eq!(
            open_all(spec(NUM), cfg(), three(99)).err(),
            Some(ObserveError::TooFewObservers { have: 0, need: 3 })
        );
        let mut unsorted = three(1_000);
        unsorted.swap(0, 1);
        assert_eq!(
            open_all(spec(NUM), cfg(), unsorted).err(),
            Some(ObserveError::BadPool)
        );
        let bad_cfg = |f: &dyn Fn(&mut ObserveConfig)| {
            let mut c = cfg();
            f(&mut c);
            open_all(spec(NUM), c, three(1_000)).is_err()
        };
        assert!(bad_cfg(&|c| c.commit_close = WINDOW.1 - 1));
        assert!(bad_cfg(
            &|c| c.commit_close = WINDOW.1 + MAX_COMMIT_GRACE_SECS + 1
        ));
        assert!(bad_cfg(
            &|c| c.reveal_close = c.commit_close + MIN_REVEAL_SECS - 1
        ));
        assert!(bad_cfg(&|c| c.miss_slash = 0));
        assert!(bad_cfg(&|c| c.outlier_slash = c.miss_slash - 1));
        // L2R-F7: every amount must fit a ledger delta; no silent clamp.
        assert!(bad_cfg(&|c| c.reward = u128::MAX));
        assert!(bad_cfg(&|c| c.reward = i128::MAX.unsigned_abs() + 1));
        assert!(!bad_cfg(&|c| c.reward = i128::MAX.unsigned_abs()));
        assert!(bad_cfg(&|c| {
            c.miss_slash = u128::MAX;
            c.outlier_slash = u128::MAX;
        }));
        let mut forbidden = spec(NUM);
        forbidden.rights.redistribution = Redistribution::Forbidden;
        assert!(open_all(forbidden, cfg(), three(1_000)).is_err());
    }

    #[test]
    fn phases_members_and_reveal_shape_are_enforced() {
        let mut r = round(EXACT, 3);
        let o = obs(Answer::Bytes(b"open".to_vec()));
        let h = observation_commit(&r.round_id(), &addr(1), &o, &SALT);
        assert_eq!(r.commit_as(999, addr(1), h), Err(ObserveError::WrongPhase));
        assert_eq!(
            r.commit_as(1_101, addr(1), h),
            Err(ObserveError::WrongPhase)
        );
        assert_eq!(
            r.commit_as(1_070, addr(9), h),
            Err(ObserveError::NotObserver)
        );
        r.commit_as(1_070, addr(1), h).expect("commit");
        assert_eq!(
            r.commit_as(1_071, addr(1), keccak256(b"x")),
            Err(ObserveError::AlreadyCommitted)
        );
        assert_eq!(
            r.reveal_as(1_150, addr(2), o.clone(), SALT),
            Err(ObserveError::NoCommit)
        );
        // A reveal outside the window is refused even when it matches its commit.
        let late = Observation {
            observed_at: 1_061,
            ..o.clone()
        };
        let hl = observation_commit(&r.round_id(), &addr(2), &late, &SALT);
        r.commit_as(1_070, addr(2), hl).expect("commit 2");
        assert!(matches!(
            r.reveal_as(1_150, addr(2), late, SALT),
            Err(ObserveError::Observation(ObservationError::OutsideWindow))
        ));
        r.reveal_as(1_150, addr(1), o.clone(), SALT)
            .expect("reveal");
        assert_eq!(
            r.reveal_as(1_151, addr(1), o, SALT),
            Err(ObserveError::AlreadyRevealed)
        );
        assert_eq!(r.settle(1_200).err(), Some(ObserveError::WrongPhase));
    }

    /// The round of `round(EXACT, 3)` with every observer committed and
    /// revealed, and a simulated log whose draw has the test receipt signer.
    fn logged_round() -> (ObservationRound, SimulatedTranscriptLog, Hash32) {
        let mut r = round(EXACT, 3);
        let rid = r.round_id();
        for i in 1..=3 {
            let o = obs(Answer::Bytes(b"open".to_vec()));
            r.commit_as(
                1_070,
                addr(i),
                observation_commit(&rid, &addr(i), &o, &SALT),
            )
            .expect("commit");
            r.reveal_as(1_150, addr(i), o, SALT).expect("reveal");
        }
        let fact = keccak256(b"registry fact");
        let mut log = SimulatedTranscriptLog::default();
        log.signers
            .insert((fact, r.draw.attempt), test_keys::receipt_signer());
        (r, log, fact)
    }

    #[test]
    fn a_withheld_commit_or_receipted_reveal_is_caught() {
        // ADR-0027 amendment 8 (SIG-R1-F1).
        let (r, mut log, fact) = logged_round();
        let rid = r.round_id();
        let full = r.transcript_query(fact);
        assert!(log.complete(&full), "nothing logged: complete");
        let (a1, c1, s1) = r.signed_commits()[0];
        // Observer 1 posts its commit inside the commit phase.
        log.post_commit(&fact, 0, &a1, &rid, &c1, &s1, 1_080)
            .expect("post");
        assert_eq!(
            log.post_commit(&fact, 0, &a1, &rid, &c1, &s1, 1_080),
            Err("AlreadyPosted")
        );
        assert!(log.complete(&full));
        // A server that leaves the posted commit out is caught.
        let mut q = full.clone();
        q.entries[0].commit = Hash32::ZERO;
        q.entries[0].revealed = false;
        assert!(!log.complete(&q), "withheld commit");
        // Receipt: the coordinator's key says it holds observer 1's reveal.
        let receipt = r
            .reveal_receipt(&test_keys::receipt_secret(), &a1)
            .expect("receipt");
        assert!(reveal_receipt_signed(
            &test_keys::receipt_signer(),
            &rid,
            0,
            &a1,
            &c1,
            &receipt
        ));
        log.post_receipt(&fact, 0, &a1, &rid, &c1, &receipt)
            .expect("post receipt");
        assert!(log.complete(&full));
        let mut q = full.clone();
        q.entries[0].revealed = false;
        assert!(!log.complete(&q), "withheld reveal");
        // Only the draw's receipt signer counts; only the observer posts its commit.
        let (a2, c2, _) = r.signed_commits()[1];
        let forged =
            sign_reveal_receipt(&test_keys::ordered(2).0, &rid, 0, &a2, &c2).expect("sign");
        assert_eq!(
            log.post_receipt(&fact, 0, &a2, &rid, &c2, &forged),
            Err("BadSignature")
        );
        assert_eq!(
            log.post_commit(&fact, 0, &a2, &rid, &c2, &s1, 1_080),
            Err("BadSignature")
        );
        assert_eq!(
            log.post_commit(&fact, 1, &a1, &rid, &c1, &s1, 1_080),
            Err("NoDraw")
        );
    }

    #[test]
    fn posts_outside_the_round_never_veto_it() {
        // An observer cannot block a round by posting: a commit posted
        // before the window or after commit close, or for another round, is
        // not the round's; a commit posted in time is the one the round uses
        // (`absorb_posted_commit`), so the coordinator can always serve it.
        let (mut r, mut log, fact) = logged_round();
        let rid = r.round_id();
        let sk3 = test_keys::ordered(3).0;
        let a3 = addr(3);
        let other = keccak256(b"another commit");
        let sig = sign_commit(&sk3, &rid, 0, &other).expect("sign");
        let late = sig;
        // Late (after commit close): ignored by the chain's check and refused by the round.
        log.post_commit(&fact, 0, &a3, &rid, &other, &late, 1_101)
            .expect("post");
        assert!(log.complete(&r.transcript_query(fact)));
        assert_eq!(
            r.absorb_posted_commit(1_101, a3, other, late),
            Err(ObserveError::WrongPhase)
        );
        assert_eq!(
            r.absorb_posted_commit(WINDOW.0 - 1, a3, other, late),
            Err(ObserveError::WrongPhase)
        );
        // For another round: another slot, never this round's.
        let rid2 = keccak256(b"other round");
        let sig2 = sign_commit(&sk3, &rid2, 0, &other).expect("sign");
        log.post_commit(&fact, 0, &a3, &rid2, &other, &sig2, 1_080)
            .expect("post");
        assert!(log.complete(&r.transcript_query(fact)));
        // In time, for this round, but another commit than the one sent off
        // chain (an observer that equivocates): the posted one replaces it
        // and its reveal (of the old commit) is dropped.
        let mut log2 = SimulatedTranscriptLog::default();
        log2.signers.insert((fact, 0), test_keys::receipt_signer());
        log2.post_commit(&fact, 0, &a3, &rid, &other, &sig, 1_090)
            .expect("post");
        assert!(!log2.complete(&r.transcript_query(fact)));
        r.absorb_posted_commit(1_090, a3, other, sig)
            .expect("absorb");
        let q = r.transcript_query(fact);
        assert!(log2.complete(&q), "the round now serves the posted commit");
        assert_eq!(q.entries[2].commit, other);
        assert!(!q.entries[2].revealed);
        assert!(r
            .reveal_receipt(&test_keys::receipt_secret(), &a3)
            .is_none());
        assert_eq!(
            r.absorb_posted_commit(1_090, [9; 20], other, sig),
            Err(ObserveError::NotObserver)
        );
    }

    /// Review REVLOG HIGH-1 (the reviewer's `test_PocObserverVeto`, silent
    /// variant): observer 3 posts an in-phase commit on chain and never
    /// commits off chain. Before the fix nothing read the log, so the honest
    /// transcript failed `transcriptComplete` for good. Now the coordinator
    /// absorbs the posted commit once the chain is past commit close, the
    /// griefer is a non-reveal, and the round is complete on chain.
    #[test]
    fn a_silent_griefer_post_is_absorbed_never_a_veto() {
        let mut r = round(EXACT, 3);
        let rid = r.round_id();
        let fact = OnchainFact {
            metric_key: keccak256(b"metric"),
            subject_key: keccak256(b"subject"),
            window_end: WINDOW.1,
            dated: false,
            template: keccak256(b"template"),
        };
        let k = fact.registry_key();
        let mut reg = SimulatedRegistry::new();
        reg.log.signers.insert((k, 0), test_keys::receipt_signer());
        let o = obs(Answer::Bytes(b"open".to_vec()));
        for i in 1..=2 {
            r.commit_as(
                1_070,
                addr(i),
                observation_commit(&rid, &addr(i), &o, &SALT),
            )
            .expect("commit");
        }
        let a3 = addr(3);
        let c3 = keccak256(b"posted only");
        let s3 = sign_commit(&test_keys::ordered(3).0, &rid, 0, &c3).expect("sign");
        reg.log
            .post_commit(&k, 0, &a3, &rid, &c3, &s3, 1_080)
            .expect("post");
        // A non-drawn key's post for this round is never asked for.
        let (sk9, a9) = test_keys::ordered(9);
        let s9 = sign_commit(&sk9, &rid, 0, &c3).expect("sign");
        reg.log
            .post_commit(&k, 0, &a9, &rid, &c3, &s9, 1_080)
            .expect("post");
        let before = r.clone();
        assert!(
            !reg.log.complete(&before.transcript_query(k)),
            "the old behaviour: refused for good"
        );
        // Not while a post could still land inside the commit phase.
        reg.time = r.cfg.commit_close;
        let e = absorb_posted_commits(&mut r, &reg, &fact, &ChainWait::NOW).expect_err("too early");
        assert!(e.contains("not past commit close"), "{e}");
        reg.time = r.cfg.commit_close + 1;
        assert_eq!(
            absorb_posted_commits(&mut r, &reg, &fact, &ChainWait::NOW),
            Ok(vec![a3])
        );
        for i in 1..=2 {
            let sk = test_keys::ordered(i).0;
            let sig = sign_reveal(&sk, &rid, 0, &o, &SALT).expect("sign");
            let took = take_reveal(
                &mut r,
                &mut reg,
                &fact,
                &test_keys::receipt_secret(),
                1_150,
                addr(i),
                o.clone(),
                SALT,
                sig,
            );
            assert_eq!(took, Ok(true));
        }
        // The griefer's reveal of anything else is a non-reveal, no receipt.
        let sig = sign_reveal(&test_keys::ordered(3).0, &rid, 0, &o, &SALT).expect("sign");
        let took = take_reveal(
            &mut r,
            &mut reg,
            &fact,
            &test_keys::receipt_secret(),
            1_150,
            a3,
            o.clone(),
            SALT,
            sig,
        );
        assert_eq!(took, Ok(false));
        assert_eq!(reg.log.receipt(&k, 0, &a3, &rid), None);
        assert_eq!(check_transcript_logged(&r, &reg, &fact), Ok(()));
        let q = r.transcript_query(k);
        assert_eq!(q.entries[2].commit, c3);
        assert!(!q.entries[2].revealed);
        // A server that then drops the absorbed commit is caught.
        let mut dropped = q.clone();
        dropped.entries[2].commit = Hash32::ZERO;
        assert!(!reg.log.complete(&dropped));
    }

    /// Review REVLOG-R2 MEDIUM-2 (the reviewer's
    /// `poc_reveal_error_after_receipts_voids_the_attempt`): nothing an
    /// observer sends stops the round once receipts are on chain. A second
    /// reveal is ignored (the first stays, its receipt is posted again as
    /// success), and a badly signed, mismatched or late reveal is a
    /// non-reveal without a receipt; the round still settles and is complete
    /// on chain.
    #[test]
    fn an_observer_cannot_stop_the_round_after_receipts() {
        let mut r = round(EXACT, 3);
        let rid = r.round_id();
        let fact = OnchainFact {
            metric_key: keccak256(b"metric"),
            subject_key: keccak256(b"subject"),
            window_end: WINDOW.1,
            dated: false,
            template: keccak256(b"template"),
        };
        let k = fact.registry_key();
        let mut reg = SimulatedRegistry::new();
        reg.log.signers.insert((k, 0), test_keys::receipt_signer());
        let o = obs(Answer::Bytes(b"open".to_vec()));
        for i in 1..=3 {
            r.commit_as(
                1_070,
                addr(i),
                observation_commit(&rid, &addr(i), &o, &SALT),
            )
            .expect("commit");
        }
        reg.time = r.cfg.commit_close + 1;
        assert_eq!(
            absorb_posted_commits(&mut r, &reg, &fact, &ChainWait::NOW),
            Ok(vec![])
        );
        let rk = test_keys::receipt_secret();
        let sig = |i: u8| sign_reveal(&test_keys::ordered(i).0, &rid, 0, &o, &SALT).expect("sign");
        let mut take = |r: &mut ObservationRound, now, i: u8, salt, s| {
            take_reveal(r, &mut reg, &fact, &rk, now, addr(i), o.clone(), salt, s)
        };
        assert_eq!(take(&mut r, 1_150, 1, SALT, sig(1)), Ok(true));
        // The same reveal again, and another one: ignored, never an error.
        assert_eq!(take(&mut r, 1_150, 1, SALT, sig(1)), Ok(true));
        assert_eq!(take(&mut r, 1_150, 1, keccak256(b"x"), sig(1)), Ok(true));
        // Observer 2 signs with another key, then a reveal of another salt:
        // non-reveals; its valid reveal is still taken afterwards.
        assert_eq!(take(&mut r, 1_150, 2, SALT, sig(3)), Ok(false));
        assert_eq!(take(&mut r, 1_150, 2, keccak256(b"y"), sig(2)), Ok(false));
        assert_eq!(take(&mut r, 1_150, 2, SALT, sig(2)), Ok(true));
        // Observer 3 reveals after the reveal phase: a non-reveal.
        let late = r.cfg.reveal_close + 1;
        assert_eq!(take(&mut r, late, 3, SALT, sig(3)), Ok(false));
        assert_eq!(reg.log.receipt(&k, 0, &addr(3), &rid), None);
        assert_eq!(
            reg.log.receipt(&k, 0, &addr(1), &rid),
            Some(observation_commit(&rid, &addr(1), &o, &SALT))
        );
        assert_eq!(r.signed_reveals().len(), 2);
        assert_eq!(check_transcript_logged(&r, &reg, &fact), Ok(()));
    }

    #[test]
    fn transcript_complete_calldata_layout() {
        let (r, _, fact) = logged_round();
        let q = r.transcript_query(fact);
        let d = q.calldata();
        // selector, 5 words, offset, length, 3 * 53 bytes padded to 160.
        assert_eq!(d.len(), 4 + 7 * 32 + 160);
        assert_eq!(d[4 + 6 * 32 - 1], 0xc0);
        assert_eq!(d[4 + 7 * 32 - 1], 159);
        assert_eq!(
            &d[4 + 7 * 32..4 + 7 * 32 + 159],
            q.packed_entries().as_slice()
        );
    }

    #[test]
    fn commits_and_reveals_need_the_observers_own_signature() {
        // ATT-R1-F3: the coordinator cannot post or open a commit for an
        // observer whose key it does not hold.
        let mut r = round(EXACT, 3);
        let rid = r.round_id();
        let o = obs(Answer::Bytes(b"open".to_vec()));
        let h = observation_commit(&rid, &addr(1), &o, &SALT);
        let sk1 = test_keys::ordered(1).0;
        let sk2 = test_keys::ordered(2).0;
        let good = sign_commit(&sk1, &rid, 0, &h).expect("sign");
        for bad in [
            [0u8; 65],
            sign_commit(&sk2, &rid, 0, &h).expect("sign"), // another observer's key
            sign_commit(&sk1, &rid, 1, &h).expect("sign"), // another attempt
            sign_commit(&sk1, &keccak256(b"other round"), 0, &h).expect("sign"),
            sign_commit(&sk1, &rid, 0, &keccak256(b"other")).expect("sign"),
        ] {
            assert_eq!(
                r.commit(1_070, addr(1), h, bad),
                Err(ObserveError::BadSignature)
            );
        }
        // A high-s twin of a valid signature is refused (canonical form only).
        let mut twin = good;
        let n = hex::decode("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141")
            .expect("hex");
        let mut borrow = 0i16;
        for i in (0..32).rev() {
            let d = i16::from(n[i]) - i16::from(good[32 + i]) - borrow;
            twin[32 + i] = d.rem_euclid(256) as u8;
            borrow = i16::from(d < 0);
        }
        twin[64] = if good[64] == 27 { 28 } else { 27 };
        assert_eq!(
            r.commit(1_070, addr(1), h, twin),
            Err(ObserveError::BadSignature)
        );
        r.commit(1_070, addr(1), h, good).expect("signed commit");
        assert_eq!(r.signed_commits(), vec![(addr(1), h, good)]);
        let rgood = sign_reveal(&sk1, &rid, 0, &o, &SALT).expect("sign");
        let other = obs(Answer::Bytes(b"shut".to_vec()));
        for bad in [
            [0u8; 65],
            sign_reveal(&sk2, &rid, 0, &o, &SALT).expect("sign"),
            sign_reveal(&sk1, &rid, 1, &o, &SALT).expect("sign"),
            sign_reveal(&sk1, &rid, 0, &other, &SALT).expect("sign"),
            sign_reveal(&sk1, &rid, 0, &o, &keccak256(b"salt")).expect("sign"),
            good,
        ] {
            assert_eq!(
                r.reveal(1_150, addr(1), o.clone(), SALT, bad),
                Err(ObserveError::BadSignature)
            );
        }
        r.reveal(1_150, addr(1), o.clone(), SALT, rgood)
            .expect("signed reveal");
        assert_eq!(r.signed_reveals(), vec![(addr(1), o, SALT, rgood)]);
    }

    #[test]
    fn a_tolerant_majority_is_always_inside_the_band() {
        // Property: if a strict majority's numbers are within `tolerance` of each
        // other, every one of them agrees, whatever the minority reveals.
        let tol: u128 = 50;
        let kind = ObservationKind::Numeric {
            scale: 0,
            tolerance: tol,
        };
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % m
        };
        for _ in 0..2_000 {
            let members = 3 + next(9) as usize;
            let q = quorum(members);
            let base = next(10_000) as i128;
            let mut answers: Vec<Answer> = (0..q)
                .map(|_| Answer::Number(base + next(51) as i128))
                .collect();
            let honest = answers.clone();
            for _ in q..members {
                answers.push(match next(3) {
                    0 => Answer::Unavailable,
                    1 => Answer::Number(next(1_000_000) as i128 - 500_000),
                    _ => Answer::Number(base + next(200) as i128 - 100),
                });
            }
            let (outcome, center) = aggregate(kind, members, &answers);
            let ObservationOutcome::Value(v) = outcome else {
                panic!("honest majority got no value: {answers:?}");
            };
            for h in &honest {
                assert_eq!(
                    judge(kind, &v, h, center),
                    Judgment::Agrees,
                    "{h:?} outside band {center}"
                );
            }
        }
    }

    #[test]
    fn settlement_is_independent_of_reveal_order() {
        let answers = [
            (1, Answer::Number(100)),
            (2, Answer::Number(120)),
            (3, Answer::Number(90)),
            (4, Answer::Number(5_000)),
        ];
        let mut a = round(NUM, 4);
        play(&mut a, &answers);
        let mut rev = answers.clone();
        rev.reverse();
        let mut b = round(NUM, 4);
        play(&mut b, &rev);
        assert_eq!(a.settle(1_201), b.settle(1_201));
    }

    /// L2R-F1: honest answers drift apart within the window (0, 30, 60 with a
    /// tolerance of 50). Whichever side a 2-of-5 minority joins, the honest
    /// observer at the far edge is between one and two tolerances from the
    /// center: no call, never slashed; the minority gains nothing over honesty.
    #[test]
    fn a_minority_cannot_choose_which_honest_observer_is_slashed() {
        for (attack, edge) in [(60i128, 1u8), (0, 3)] {
            let mut r = round(
                ObservationKind::Numeric {
                    scale: 0,
                    tolerance: 50,
                },
                5,
            );
            play(
                &mut r,
                &[
                    (1, Answer::Number(0)),
                    (2, Answer::Number(30)),
                    (3, Answer::Number(60)),
                    (4, Answer::Number(attack)),
                    (5, Answer::Number(attack)),
                ],
            );
            let s = r.settle(1_201).expect("settle");
            assert_eq!(status_of(&s, edge), ObserverStatus::NoCall);
            assert!(!s.entries().iter().any(|(a, d)| *a == addr(edge) && *d < 0));
            assert!(s
                .statuses
                .iter()
                .all(|(_, st)| *st != ObserverStatus::Outlier));
        }
        // A real liar, beyond twice the tolerance, is still slashed.
        let mut r = round(
            ObservationKind::Numeric {
                scale: 0,
                tolerance: 50,
            },
            5,
        );
        play(
            &mut r,
            &[
                (1, Answer::Number(0)),
                (2, Answer::Number(30)),
                (3, Answer::Number(60)),
                (4, Answer::Number(30)),
                (5, Answer::Number(131)),
            ],
        );
        let s = r.settle(1_201).expect("settle");
        assert_eq!(status_of(&s, 5), ObserverStatus::Outlier);
    }

    /// L2R-F2 and L2R3-F4: a lazy majority answering `Unavailable` is paid
    /// nothing, and observers who fetched and corroborate each other (two or
    /// more values within the tolerance) are not slashed: a lazy cartel cannot
    /// censor a feed and slash the honest minority for free.
    #[test]
    fn an_unavailable_majority_is_paid_nothing() {
        let mut r = round(NUM, 5);
        play(
            &mut r,
            &[
                (1, Answer::Unavailable),
                (2, Answer::Unavailable),
                (3, Answer::Unavailable),
                (4, Answer::Number(100)),
                (5, Answer::Number(130)),
            ],
        );
        let s = r.settle(1_201).expect("settle");
        assert_eq!(*s.outcome(), ObservationOutcome::SourceUnavailable);
        assert!(s.entries().is_empty(), "{:?}", s.entries());
        assert_eq!(status_of(&s, 4), ObserverStatus::NoCall);
        assert_eq!(status_of(&s, 5), ObserverStatus::NoCall);
    }

    /// L2R2-F4 kept under L2R3-F4: a LONE value in an outage (no other value
    /// within the tolerance) is a stale guess and pays the small slash, so
    /// guessing never beats an honest `Unavailable`. Two lone values that
    /// disagree corroborate nothing.
    #[test]
    fn a_lone_value_in_an_outage_costs_the_small_slash() {
        let mut r = round(NUM, 5);
        play(
            &mut r,
            &[
                (1, Answer::Unavailable),
                (2, Answer::Unavailable),
                (3, Answer::Unavailable),
                (4, Answer::Number(100)),
                (5, Answer::Number(900)),
            ],
        );
        let s = r.settle(1_201).expect("settle");
        assert_eq!(*s.outcome(), ObservationOutcome::SourceUnavailable);
        assert_eq!(s.entries(), [(addr(4), -20), (addr(5), -20)]);
        let mut r = round(EXACT, 5);
        play(
            &mut r,
            &[
                (1, Answer::Unavailable),
                (2, Answer::Unavailable),
                (3, Answer::Unavailable),
                (4, Answer::Bytes(b"open".to_vec())),
                (5, Answer::Bytes(b"open".to_vec())),
            ],
        );
        let s = r.settle(1_201).expect("settle");
        assert!(s.entries().is_empty(), "identical bytes corroborate");
    }

    /// L2R-F4: a minority inside the band moves the settled value, but only
    /// within the honest majority's own range.
    #[test]
    fn a_minority_in_the_band_stays_inside_the_honest_range() {
        let kind = ObservationKind::Numeric {
            scale: 0,
            tolerance: 50,
        };
        // Honest 100, 100, 150; minority 150, 150.
        let (out, _) = aggregate(kind, 5, &[100, 100, 150, 150, 150].map(Answer::Number));
        let ObservationOutcome::Value(Answer::Number(v)) = out else {
            panic!("no value");
        };
        assert!((100..=150).contains(&v));
    }

    /// L2R-F5: an observer who saw another's commit hash first and posted it
    /// cannot block the owner, and cannot reveal it either.
    #[test]
    fn front_running_a_seen_commit_hash_fails() {
        let mut r = round(NUM, 3);
        let o = obs(Answer::Number(5));
        let h1 = observation_commit(&r.round_id(), &addr(1), &o, &SALT);
        r.commit_as(1_050, addr(3), h1)
            .expect("griefer posts the hash");
        r.commit_as(1_051, addr(1), h1)
            .expect("the owner still commits");
        r.reveal_as(1_150, addr(1), o.clone(), SALT)
            .expect("owner reveals");
        assert_eq!(
            r.reveal_as(1_150, addr(3), o, SALT),
            Err(ObserveError::HashMismatch)
        );
        let s = r.settle(1_201).expect("settle");
        assert_eq!(status_of(&s, 3), ObserverStatus::Unrevealed);
    }

    /// SIG-R1-F1 (known limit, THREATS SEC-R2-F1-P2): a coordinator that
    /// withholds two of three honest signed reveals turns a unanimous round
    /// into `NoResult` (a re-draw). The withheld observers show `Unrevealed`
    /// and are neither paid nor slashed: a missing reveal is the coordinator's
    /// word alone. `att_r1_attempts::a_withheld_receipted_reveal_is_caught_on_chain`
    /// is the same attack end to end.
    #[test]
    fn a_withheld_honest_reveal_is_never_slashed() {
        let mut r = round(NUM, 3);
        let o = obs(Answer::Number(5));
        for i in 1..=3 {
            let h = observation_commit(&r.round_id(), &addr(i), &o, &SALT);
            r.commit_as(1_050, addr(i), h).expect("commit");
        }
        r.reveal_as(1_150, addr(1), o, SALT).expect("reveal");
        let s = r.settle(1_201).expect("settle");
        assert_eq!(s.outcome(), &ObservationOutcome::NoResult);
        for i in 2..=3 {
            assert_eq!(status_of(&s, i), ObserverStatus::Unrevealed);
            assert!(s.entries().iter().all(|(a, _)| *a != addr(i)));
        }
    }

    /// L2R-F6: two rounds of the same feed have different round ids; a reveal
    /// copied from round A never opens a commit in round B.
    #[test]
    fn a_reveal_from_another_round_of_the_same_feed_never_counts() {
        let three: Vec<Observer> = (1..=3)
            .map(|i| Observer {
                address: addr(i),
                bond: 1_000,
            })
            .collect();
        let a = open_all(spec(NUM), cfg(), three.clone()).expect("a");
        let mut later = cfg();
        later.reveal_close += 1;
        let mut b = open_all(spec(NUM), later, three).expect("b");
        assert_eq!(a.spec_id(), b.spec_id());
        assert_ne!(a.round_id(), b.round_id());
        let o = obs(Answer::Number(77));
        // Observer 1 opened its commit in round A; round B was committed to
        // round A's hash (a copy). It does not open in B.
        let ha = observation_commit(&a.round_id(), &addr(1), &o, &SALT);
        b.commit_as(1_070, addr(1), ha).expect("commit");
        assert_eq!(
            b.reveal_as(1_150, addr(1), o, SALT),
            Err(ObserveError::HashMismatch)
        );
    }

    /// L2R-F3: a level-2 claim is backed only by the settlement it names, only
    /// when that round settled a value, and only for that exact value.
    #[test]
    fn a_level_two_claim_needs_the_settled_value_of_its_round() {
        let mut r = round(NUM, 3);
        play(
            &mut r,
            &[
                (1, Answer::Number(6_412_345)),
                (2, Answer::Number(6_412_345)),
                (3, Answer::Number(6_412_350)),
            ],
        );
        let s = r.settle(1_201).expect("settle");
        let method = VerificationMethod::ReObserved {
            spec: s.spec_id(),
            result: s.result_hash(),
        };
        let value = Value::Fixed {
            mantissa: 6_412_345,
            scale: 2,
        };
        assert_eq!(s.backs(&spec(NUM), &method, &value), Ok(()));
        let refused = |m: &VerificationMethod, v: &Value| s.backs(&spec(NUM), m, v).is_err();
        // Another value, another scale, another kind.
        assert!(refused(
            &method,
            &Value::Fixed {
                mantissa: 6_412_350,
                scale: 2
            }
        ));
        assert!(refused(
            &method,
            &Value::Fixed {
                mantissa: 6_412_345,
                scale: 3
            }
        ));
        assert!(refused(&method, &Value::Int(6_412_345)));
        // A pointer to another result, or to nothing.
        assert!(refused(
            &VerificationMethod::ReObserved {
                spec: s.spec_id(),
                result: keccak256(b"made up"),
            },
            &value
        ));
        assert!(refused(&VerificationMethod::OfficialSourceMatch, &value));
        // A round that settled no value backs no claim, whatever it names.
        let mut down = round(NUM, 3);
        play(
            &mut down,
            &[
                (1, Answer::Unavailable),
                (2, Answer::Unavailable),
                (3, Answer::Unavailable),
            ],
        );
        let d = down.settle(1_201).expect("settle");
        let m = VerificationMethod::ReObserved {
            spec: d.spec_id(),
            result: d.result_hash(),
        };
        assert_eq!(
            d.backs(&spec(NUM), &m, &value),
            Err(ObserveError::ClaimNotBacked("the round settled no value"))
        );
        // Exact feeds compare bytes.
        let mut e = round(EXACT, 3);
        play(
            &mut e,
            &[
                (1, Answer::Bytes(b"open".to_vec())),
                (2, Answer::Bytes(b"open".to_vec())),
                (3, Answer::Bytes(b"open".to_vec())),
            ],
        );
        let es = e.settle(1_201).expect("settle");
        let em = VerificationMethod::ReObserved {
            spec: es.spec_id(),
            result: es.result_hash(),
        };
        assert_eq!(
            es.backs(&spec(EXACT), &em, &Value::Bytes(b"open".to_vec())),
            Ok(())
        );
        assert!(es
            .backs(&spec(EXACT), &em, &Value::Bytes(b"shut".to_vec()))
            .is_err());
    }

    fn registry() -> onyx_core::StaticRegistry {
        let mut r = onyx_core::StaticRegistry::new();
        r.add_metric(
            "example.page.value",
            onyx_core::MetricInfo {
                value_kind: onyx_core::ValueKind::Fixed { scale: 2 },
                needs_label_set: false,
            },
        )
        .expect("metric");
        r.add_entity("entity:example-exchange", onyx_core::EntityKind::Exchange)
            .expect("entity");
        r.add_source(spec(NUM).rights).expect("source");
        let mut other = spec(NUM).rights;
        other.source = SourceId("other.page".into());
        r.add_source(other).expect("other source");
        r
    }

    fn claim(method: VerificationMethod, mantissa: i128, source: &str) -> Claim {
        let mut rights = spec(NUM).rights;
        rights.source = SourceId(source.into());
        let content_hash = match &method {
            VerificationMethod::ReObserved { result, .. } => *result,
            _ => keccak256(b"page"),
        };
        Claim {
            metric: onyx_core::claim::MetricId("example.page.value".into()),
            subject: onyx_core::claim::Subject::Entity("entity:example-exchange".into()),
            value: Value::Fixed { mantissa, scale: 2 },
            source: onyx_core::SourceRef {
                source: SourceId(source.into()),
                snapshot: onyx_core::Snapshot::Document {
                    published_unix: WINDOW.1,
                    content_hash,
                },
            },
            method,
            label_set: None,
            rights,
            evidence: vec![],
            confidence: None,
        }
    }

    fn settled_into(
        store: &mut ObservationResults,
        answers: &[(u8, Answer)],
    ) -> ObservationSettlement {
        let n = answers.len() as u8;
        let pool: Vec<Observer> = (1..=n)
            .map(|i| Observer {
                address: addr(i),
                bond: 1_000,
            })
            .collect();
        let mut sp = spec(NUM);
        sp.min_observers = u16::from(n);
        let mut r = open_in(store, sp, cfg(), pool).expect("open in the store");
        play(&mut r, answers);
        store.settle(&r, 1_201).expect("settle into the store")
    }

    /// L2R2-F1: a claim is accepted only through the store, and only for a
    /// result this coordinator settled itself: a settlement from anywhere else,
    /// an unknown pointer, another value, a round with no value, or another
    /// source's rights are all refused.
    #[test]
    fn claims_are_accepted_only_for_results_settled_in_this_store() {
        let reg = registry();
        let mut store = store_for(NUM);
        let good = settled_into(
            &mut store,
            &[
                (1, Answer::Number(12_345)),
                (2, Answer::Number(12_345)),
                (3, Answer::Number(12_350)),
            ],
        );
        let m = |s: &ObservationSettlement| VerificationMethod::ReObserved {
            spec: s.spec_id(),
            result: s.result_hash(),
        };
        assert_eq!(
            store.accept_claim(&claim(m(&good), 12_345, "example.page"), &reg),
            Ok(onyx_core::ProofLevel::ReObserved)
        );
        // Another value of the same result.
        assert!(store
            .accept_claim(&claim(m(&good), 12_350, "example.page"), &reg)
            .is_err());
        // A pointer this store never settled: made up, or settled by another
        // coordinator's store (a "forged" settlement never reaches this one).
        let mut elsewhere = store_for_n(NUM, 4);
        let foreign = settled_into(
            &mut elsewhere,
            &[
                (1, Answer::Number(999)),
                (2, Answer::Number(999)),
                (3, Answer::Number(999)),
                (4, Answer::Number(999)),
            ],
        );
        assert_eq!(
            store.accept_claim(&claim(m(&foreign), 999, "example.page"), &reg),
            Err(ObserveError::ClaimNotBacked(
                "no round settled with this result"
            ))
        );
        let made_up = VerificationMethod::ReObserved {
            spec: good.spec_id(),
            result: keccak256(b"made up"),
        };
        assert!(store
            .accept_claim(&claim(made_up, 12_345, "example.page"), &reg)
            .is_err());
        // A round that settled no value backs nothing (its own store: this
        // store already holds a result for the feed, L2R3-F5).
        let mut downs = store_for_n(NUM, 5);
        let down = settled_into(
            &mut downs,
            &[
                (1, Answer::Unavailable),
                (2, Answer::Unavailable),
                (3, Answer::Unavailable),
                (4, Answer::Unavailable),
                (5, Answer::Unavailable),
            ],
        );
        assert_eq!(
            downs.accept_claim(&claim(m(&down), 0, "example.page"), &reg),
            Err(ObserveError::ClaimNotBacked("the round settled no value"))
        );
        // Registered rights, but not the feed's source.
        assert_eq!(
            store.accept_claim(&claim(m(&good), 12_345, "other.page"), &reg),
            Err(ObserveError::ClaimNotBacked("source is not the feed's"))
        );
        // A malformed claim fails validation first.
        let zero = VerificationMethod::ReObserved {
            spec: good.spec_id(),
            result: Hash32::ZERO,
        };
        assert!(matches!(
            store.accept_claim(&claim(zero, 12_345, "example.page"), &reg),
            Err(ObserveError::Claim(_))
        ));
        assert_eq!(store.get(&good.result_hash()), Some(&good));
    }

    /// L2SRC-R2-F1: rounds open only for a registered metric and only as its
    /// template; registration needs this store's admin, is recorded, and is
    /// final.
    #[test]
    fn feeds_open_only_as_their_registered_template() {
        let (mut store, admin) = ObservationResults::new();
        let three = || {
            (1..=3)
                .map(|i| Observer {
                    address: addr(i),
                    bond: 1_000,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            open_in(&mut store, spec(NUM), cfg(), three()).err(),
            Some(ObserveError::UnknownFeed)
        );
        let (_other, other_admin) = ObservationResults::new();
        let t = template_of_spec(&spec(NUM));
        assert_eq!(
            store.register_feed(&other_admin, t.clone(), "x", 1),
            Err(ObserveError::NotFeedAdmin)
        );
        store
            .register_feed(&admin, t.clone(), "reviewed", 7)
            .expect("register");
        assert_eq!(
            store.register_feed(&admin, t.clone(), "again", 8),
            Err(ObserveError::FeedAlreadyRegistered)
        );
        assert_eq!(store.feed_decisions().len(), 1);
        assert_eq!(store.feed_decisions()[0].at, 7);
        assert_eq!(store.feed_decisions()[0].template, t.id());
        assert!(open_in(&mut store, spec(NUM), cfg(), three()).is_ok());
        for (name, f) in [
            ("fetcher", {
                let mut f = spec(NUM);
                f.fetcher = keccak256(b"another fetcher");
                f
            }),
            ("url", {
                let mut f = spec(NUM);
                f.url = "https://evil.example/page.json".into();
                f
            }),
            ("selector", {
                let mut f = spec(NUM);
                f.selector = "/data/other".into();
                f
            }),
            ("window", {
                let mut f = spec(NUM);
                f.window_end += 1;
                f
            }),
            ("observers", {
                let mut f = spec(NUM);
                f.min_observers = 2;
                f
            }),
        ] {
            assert!(
                matches!(
                    open_in(&mut store, f, cfg(), three()),
                    Err(ObserveError::Template(_))
                ),
                "{name}"
            );
        }
    }

    /// L2R2-F1: a round is settled into the store once, so its entries are
    /// produced once.
    #[test]
    fn a_round_settles_into_the_store_once() {
        let mut store = store_for(NUM);
        let pool: Vec<Observer> = (1..=3)
            .map(|i| Observer {
                address: addr(i),
                bond: 1_000,
            })
            .collect();
        let mut r = open_in(&mut store, spec(NUM), cfg(), pool).expect("open");
        play(
            &mut r,
            &[
                (1, Answer::Number(5)),
                (2, Answer::Number(5)),
                (3, Answer::Number(5)),
            ],
        );
        store.settle(&r, 1_201).expect("first");
        assert_eq!(store.settle(&r, 1_202), Err(ObserveError::AlreadySettled));
    }

    /// Review REVLOG-R3-3: a retry after the round settled (the run died
    /// after `settle`) gets the recorded settlement back from
    /// `settle_once`; the same round id with other reveals stays refused.
    #[test]
    fn settle_once_is_idempotent_only_for_the_same_result() {
        let mut store = store_for(NUM);
        let pool: Vec<Observer> = (1..=3)
            .map(|i| Observer {
                address: addr(i),
                bond: 1_000,
            })
            .collect();
        let mut r = open_in(&mut store, spec(NUM), cfg(), pool).expect("open");
        play(
            &mut r,
            &[
                (1, Answer::Number(5)),
                (2, Answer::Number(5)),
                (3, Answer::Number(7)),
            ],
        );
        let first = store.settle_once(&r, 1_201).expect("first");
        assert_eq!(store.settle_once(&r, 1_201), Ok(first.clone()), "a retry");
        let mut other = r.clone();
        other.reveals.remove(&addr(3));
        assert_eq!(
            store.settle_once(&other, 1_201),
            Err(ObserveError::AlreadySettled),
            "another result for the same round"
        );
    }

    /// Review REVLOG-R3-2: an unfinished round travels with the book
    /// (version 3) into a fresh store, which then refuses a new draw for its
    /// fact and hands the same round back; a tampered round is refused at
    /// load, never trusted from bytes.
    #[test]
    fn an_unfinished_round_travels_with_the_book_and_is_rechecked() {
        let mut store = store_for(NUM);
        let pool: Vec<Observer> = (1..=3)
            .map(|i| Observer {
                address: addr(i),
                bond: 1_000,
            })
            .collect();
        let mut r = open_in(&mut store, spec(NUM), cfg(), pool.clone()).expect("open");
        play(&mut r, &[(1, Answer::Number(5))]);
        r.log = Some(LogSnapshot {
            block: Some(LogBlock {
                number: 40,
                time: r.cfg.commit_close + 1,
            }),
            posted: vec![],
        });
        let draw = DrawRecord {
            chain_id: 31_337,
            registry: [0; 20],
            attempt: 0,
            draw_count: 1,
            announced_at: 1,
            commit: draw_commit(&r.draw.pool, &r.spec_id, &cfg_hash(&r.cfg)),
            beacon_height: r.draw.beacon_height,
            beacon: r.draw.beacon,
        };
        store
            .keep_unfinished(UnfinishedRound {
                round: r.clone(),
                onchain_draw: draw,
                reveals: vec![],
            })
            .expect("kept");
        assert_eq!(
            store
                .announce_draw(&spec(NUM), &cfg(), &pool, 99, &SimulatedChain::at(0))
                .err(),
            Some(ObserveError::RoundUnfinished),
            "no new draw while the round is unfinished"
        );
        let book = store.non_value_book().expect("writable");
        assert_eq!(book["version"], 8);
        let mut fresh = store_for(NUM);
        fresh.absorb_non_value_book(&book).expect("loads");
        let back = fresh.kept_round(&spec(NUM)).expect("the same round");
        assert_eq!(back.round.round_id, r.round_id);
        assert_eq!(back.round.log, r.log);
        assert_eq!(back.round.signed_reveals(), r.signed_reveals());
        // The restored round settles in the fresh store (its draw restored).
        fresh
            .settle(&back.round, 1_201)
            .expect("settles after a restart");
        // Tampered: one byte of the saved round flipped.
        let mut bad = book.clone();
        let h = bad["unfinished"][0].as_str().expect("hex").to_owned();
        let mut b = hex::decode(&h[2..]).expect("hex");
        let n = b.len();
        b[n - 40] ^= 1;
        bad["unfinished"][0] = serde_json::json!(format!("0x{}", hex::encode(b)));
        assert!(store_for(NUM).absorb_non_value_book(&bad).is_err());
        // Review REVLOG-R4-3: a version-3 book is still read; its reveals
        // carry no arrival time and come back at `commit_close + 1`, the time
        // the version-3 runners took every reveal at.
        let sr = r.signed_reveals();
        assert_eq!(sr.len(), 1);
        let mut v3 = book.clone();
        v3["version"] = 3.into();
        let mut bytes = borsh::to_vec(&r).expect("round");
        // ROADMAP C11: a version-6 round ends with its sealed snapshot
        // (`None`, one byte); books up to version 5 have none.
        assert_eq!(bytes.pop(), Some(0));
        let v5_round = bytes.clone();
        let v7_round = bytes.clone();
        bytes.extend(borsh::to_vec(&draw).expect("draw"));
        bytes.extend(borsh::to_vec(&sr).expect("reveals"));
        v3["unfinished"][0] = serde_json::json!(format!("0x{}", hex::encode(bytes)));
        // A version-5 book (no sealed snapshot) still loads, unchanged.
        let mut v5 = book.clone();
        v5["version"] = 5.into();
        let mut b5 = v5_round;
        b5.extend(borsh::to_vec(&draw).expect("draw"));
        b5.extend(borsh::to_vec(&Vec::<ArrivedReveal>::new()).expect("reveals"));
        v5["unfinished"][0] = serde_json::json!(format!("0x{}", hex::encode(b5)));
        let mut five = store_for(NUM);
        five.absorb_non_value_book(&v5)
            .expect("a version-5 book loads");
        let back5 = five.kept_round(&spec(NUM)).expect("kept");
        assert_eq!(back5.round.round_id, r.round_id);
        assert!(back5.round.sealed.is_none());
        // ROADMAP C52: a version-7 book loads (a round without a time-lock
        // has the same bytes), but a version-7 TIME-LOCKED snapshot (its
        // posts carry no far-date copy; never deployed) is refused.
        let mut v7 = book.clone();
        v7["version"] = 7.into();
        store_for(NUM)
            .absorb_non_value_book(&v7)
            .expect("a version-7 book without a time-lock loads");
        #[derive(BorshSerialize)]
        struct V7Read {
            lock_round: u64,
            signature: Vec<u8>,
            openings: Vec<(MinerAddress, crate::timelock::TlockOpening)>,
        }
        #[derive(BorshSerialize)]
        struct V7Snap {
            block: LogBlock,
            reveal_key: crate::sealed::Point,
            posts: Vec<(MinerAddress, PostedSealed)>,
            dismissals: Vec<(MinerAddress, crate::sealed::Dismissal)>,
            timelock: Option<V7Read>,
        }
        let snap7 = |timelock| V7Snap {
            block: LogBlock {
                number: 9,
                time: r.cfg.reveal_close + 1,
            },
            reveal_key: [7; 64],
            posts: vec![],
            dismissals: vec![],
            timelock,
        };
        for (timelock, loads) in [
            (None, true),
            (
                Some(V7Read {
                    lock_round: 1,
                    signature: vec![1; 48],
                    openings: vec![],
                }),
                false,
            ),
        ] {
            let mut b7 = v7_round.clone();
            b7.push(1);
            b7.extend(borsh::to_vec(&snap7(timelock)).expect("snapshot"));
            b7.extend(borsh::to_vec(&draw).expect("draw"));
            b7.extend(borsh::to_vec(&Vec::<ArrivedReveal>::new()).expect("reveals"));
            let mut book7 = v7.clone();
            book7["unfinished"][0] = serde_json::json!(format!("0x{}", hex::encode(b7)));
            let got = store_for(NUM).absorb_non_value_book(&book7);
            if loads {
                // It decodes; the snapshot's own checks then judge it (a
                // made-up key and no log here, so it is refused there).
                assert!(!format!("{got:?}").contains("far-date copy"), "{got:?}");
            } else {
                let e = got.expect_err("a version-7 time-locked round is refused");
                assert!(format!("{e}").contains("far-date copy"), "{e}");
            }
        }
        let mut old = store_for(NUM);
        old.absorb_non_value_book(&v3)
            .expect("a version-3 book loads");
        let back = old.kept_round(&spec(NUM)).expect("kept");
        assert_eq!(
            back.reveals,
            vec![ArrivedReveal {
                reveal: sr[0].clone(),
                arrived: r.cfg.commit_close + 1,
            }]
        );
    }

    /// A drawn round (observers 1..=3, each committed at 1_070 to `Number(5)`)
    /// kept unfinished in `store`, and each observer's signed reveal.
    fn kept_round(store: &mut ObservationResults) -> (UnfinishedRound, Vec<SignedReveal>) {
        let pool: Vec<Observer> = (1..=3)
            .map(|i| Observer {
                address: addr(i),
                bond: 1_000,
            })
            .collect();
        let mut r = open_in(store, spec(NUM), cfg(), pool).expect("open");
        let mut reveals = vec![];
        for i in 1..=3 {
            let o = obs(Answer::Number(5));
            let h = observation_commit(&r.round_id(), &addr(i), &o, &SALT);
            r.commit_as(1_070, addr(i), h).expect("commit");
            let sk = test_keys::ordered_secret(&addr(i)).expect("key");
            let sig = sign_reveal(&sk, &r.round_id, r.draw.attempt, &o, &SALT).expect("sig");
            reveals.push((addr(i), o, SALT, sig));
        }
        let u = UnfinishedRound {
            onchain_draw: DrawRecord {
                chain_id: 31_337,
                registry: [0; 20],
                attempt: 0,
                draw_count: 1,
                announced_at: 1,
                commit: draw_commit(&r.draw.pool, &r.spec_id, &cfg_hash(&r.cfg)),
                beacon_height: r.draw.beacon_height,
                beacon: r.draw.beacon,
            },
            round: r,
            reveals: vec![],
        };
        store.keep_unfinished(u.clone()).expect("kept");
        (u, reveals)
    }

    /// Review REVLOG-R4-3: a reveal is stamped and phase-checked when it
    /// ARRIVES (the coordinator's clock), so a receipt signed long after
    /// `reveal_close` (the finalized-block wait) still takes it; a reveal
    /// arriving outside the reveal phase, unsigned, from someone not drawn,
    /// or a second one is a non-reveal.
    #[test]
    fn a_reveal_is_checked_at_its_arrival_not_at_its_receipt() {
        let mut store = store_for(NUM);
        let (mut u, reveals) = kept_round(&mut store);
        let (close, end) = (cfg().commit_close, cfg().reveal_close);
        assert!(
            !u.receive_reveal(reveals[0].clone(), close),
            "still the commit phase"
        );
        assert!(
            !u.receive_reveal(reveals[0].clone(), end + 1),
            "after reveal_close"
        );
        let mut forged = reveals[0].clone();
        forged.3 = reveals[1].3;
        assert!(
            !u.receive_reveal(forged, close + 1),
            "not the observer's signature"
        );
        let mut stranger = reveals[0].clone();
        stranger.0 = addr(9);
        assert!(!u.receive_reveal(stranger, close + 1), "not drawn");
        assert!(u.receive_reveal(reveals[0].clone(), close + 1));
        assert!(
            !u.receive_reveal(reveals[0].clone(), close + 2),
            "a second one"
        );
        assert!(
            u.receive_reveal(reveals[1].clone(), end),
            "on the last second"
        );
        assert_eq!(
            u.reveals.iter().map(|a| a.arrived).collect::<Vec<_>>(),
            vec![close + 1, end]
        );
        // The receipts wait for a finalized block 1,178 s past the commit
        // phase (Base Sepolia, measured): taken at their arrival, both count.
        let mut reg = SimulatedRegistry::new();
        let fact = store.onchain_fact(&spec(NUM)).expect("fact");
        reg.log
            .signers
            .insert((fact.registry_key(), 0), test_keys::receipt_signer());
        reg.time = close + 1_178;
        let mut round = u.round.clone();
        finish_reveals(
            &mut round,
            &mut reg,
            &fact,
            &test_keys::receipt_secret(),
            &u.reveals,
            &ChainWait::NOW,
            &mut |_| Ok(()),
        )
        .expect("finished");
        assert_eq!(
            round.reveals.len(),
            2,
            "both reveals taken after reveal_close"
        );
        // Taken at the receipt's time instead, the same reveal is late.
        let mut late = u.round.clone();
        late.log = round.log.clone();
        let (a, o, salt, sig) = reveals[2].clone();
        assert_eq!(
            take_reveal(
                &mut late,
                &mut reg,
                &fact,
                &test_keys::receipt_secret(),
                close + 1_178,
                a,
                o,
                salt,
                sig
            ),
            Ok(false)
        );
    }

    /// A test inbox: each reveal reaches the coordinator at its time.
    struct Timed(Vec<(u64, SignedReveal)>);

    impl RevealInbox for Timed {
        fn take(&mut self, _: &Hash32, _: &[MinerAddress], now: u64) -> Vec<SignedReveal> {
            let (due, later) = std::mem::take(&mut self.0)
                .into_iter()
                .partition(|(t, _)| *t <= now);
            self.0 = later;
            due.into_iter().map(|(_, r)| r).collect()
        }
    }

    /// `store` saved to a book and read back into a fresh store (a
    /// restart), with its kept round.
    fn restarted(store: &ObservationResults) -> (ObservationResults, UnfinishedRound) {
        let book = store.non_value_book().expect("writable");
        let mut fresh = store_for(NUM);
        fresh.absorb_non_value_book(&book).expect("the book loads");
        let back = fresh.kept_round(&spec(NUM)).expect("kept");
        (fresh, back)
    }

    /// THREATS REVLOG-R5-1d (ROADMAP B5): a coordinator restarted while the
    /// round's reveal window is open keeps taking reveals. The window (the
    /// round's config) and the reveal taken before the crash come back from
    /// the book; reveals sent after the restart are taken by the clock until
    /// `reveal_close`, each kept and saved before the next; the intake stops
    /// once every drawn observer revealed. A replayed reveal keeps the first
    /// (and its arrival time); a forged one is a non-reveal.
    #[test]
    fn a_restarted_round_keeps_taking_reveals_while_its_window_is_open() {
        let mut store = store_for(NUM);
        let (mut u, reveals) = kept_round(&mut store);
        let (close, end) = (cfg().commit_close, cfg().reveal_close);
        assert!(u.receive_reveal(reveals[0].clone(), close + 1));
        store.keep_unfinished(u).expect("kept");
        let (mut fresh, mut back) = restarted(&store);
        assert_eq!(back.round.config(), cfg(), "the window comes back");
        assert_eq!(back.reveals.len(), 1, "the reveal taken before the crash");
        let mut forged = reveals[2].clone();
        forged.3 = reveals[1].3;
        let mut inbox = Timed(vec![
            (close + 30, reveals[0].clone()), // a replay
            (close + 30, forged),
            (close + 30, reveals[1].clone()),
            // Read on the poll step (every REVEAL_POLL_SECS from close + 30).
            (end - 6, reveals[2].clone()),
        ]);
        let clock = ReplayClock::at(close + 30);
        let mut saved = vec![];
        let taken = keep_taking_reveals(&mut fresh, &mut back, &mut inbox, &clock, &mut |s| {
            saved.push(s.kept_round(&spec(NUM)).map_or(0, |k| k.reveals.len()));
            Ok(())
        })
        .expect("the intake runs");
        assert_eq!(taken, 2);
        assert_eq!(saved, vec![2, 3], "each reveal saved before the next");
        assert_eq!(
            back.reveals.iter().map(|a| a.arrived).collect::<Vec<_>>(),
            vec![close + 1, close + 30, end - 6],
            "the replay kept the first arrival"
        );
        assert_eq!(
            clock.now(),
            end - 6,
            "every drawn observer revealed: no wait"
        );
        assert_eq!(
            fresh.kept_round(&spec(NUM)).map(|k| k.reveals),
            Some(back.reveals.clone()),
            "the store keeps what was taken"
        );
        // The receipts come later: every reveal counts at its arrival.
        let mut reg = SimulatedRegistry::new();
        let fact = fresh.onchain_fact(&spec(NUM)).expect("fact");
        reg.log
            .signers
            .insert((fact.registry_key(), 0), test_keys::receipt_signer());
        reg.time = close + 1_178;
        let mut round = back.round.clone();
        finish_reveals(
            &mut round,
            &mut reg,
            &fact,
            &test_keys::receipt_secret(),
            &back.reveals,
            &ChainWait::NOW,
            &mut |_| Ok(()),
        )
        .expect("finished");
        assert_eq!(round.reveals.len(), 3, "no reveal lost to the restart");
    }

    /// Review B5-R2-1: the book's reveals are not re-checked when it loads
    /// (`restore_unfinished`), and the intake refuses a second reveal from
    /// an observer and stops once every drawn observer has one. A saved
    /// entry that is not a reveal (a corrupted or edited book: a forged
    /// signature, an arrival outside the window) must not hold that
    /// observer's slot: after a restart the observer's real reveal, sent
    /// again inside the window, is taken, and the book keeps only checked
    /// reveals.
    #[test]
    fn a_bad_reveal_in_the_book_does_not_hold_the_observers_slot() {
        let mut store = store_for(NUM);
        let (mut u, reveals) = kept_round(&mut store);
        let (close, end) = (cfg().commit_close, cfg().reveal_close);
        assert!(u.receive_reveal(reveals[0].clone(), close + 1));
        let mut forged = reveals[1].clone();
        forged.3 = reveals[0].3;
        u.reveals.push(ArrivedReveal {
            reveal: forged,
            arrived: close + 1,
        });
        u.reveals.push(ArrivedReveal {
            reveal: reveals[2].clone(),
            arrived: end + 5,
        });
        store.keep_unfinished(u).expect("kept");
        let (mut fresh, mut back) = restarted(&store);
        assert_eq!(back.reveals.len(), 3, "the book loads as saved");
        let mut inbox = Timed(vec![
            (close + 30, reveals[1].clone()),
            (close + 30, reveals[2].clone()),
        ]);
        let clock = ReplayClock::at(close + 30);
        let mut saved = vec![];
        let taken = keep_taking_reveals(&mut fresh, &mut back, &mut inbox, &clock, &mut |s| {
            saved.push(s.kept_round(&spec(NUM)).map_or(0, |k| k.reveals.len()));
            Ok(())
        })
        .expect("the intake runs");
        assert_eq!(taken, 2, "both observers' real reveals are taken");
        assert_eq!(saved, vec![1, 2, 3], "the bad entries leave the book first");
        assert_eq!(
            back.reveals
                .iter()
                .map(|a| (a.reveal.clone(), a.arrived))
                .collect::<Vec<_>>(),
            vec![
                (reveals[0].clone(), close + 1),
                (reveals[1].clone(), close + 30),
                (reveals[2].clone(), close + 30),
            ]
        );
    }

    /// REVLOG-R5-1d, bounded: an observer that never reveals (a griefer)
    /// holds the intake only until `reveal_close`; a reveal reaching the
    /// coordinator after it is a non-reveal. A restart after the window took
    /// nothing and does not wait.
    #[test]
    fn the_restarted_intake_ends_at_reveal_close() {
        let mut store = store_for(NUM);
        let (u, reveals) = kept_round(&mut store);
        let (close, end) = (cfg().commit_close, cfg().reveal_close);
        store.keep_unfinished(u).expect("kept");
        let (mut fresh, mut back) = restarted(&store);
        let mut inbox = Timed(vec![
            (close + 10, reveals[0].clone()),
            (end, reveals[1].clone()),
            (end + 1, reveals[2].clone()),
        ]);
        let clock = ReplayClock::at(close + 10);
        let taken = keep_taking_reveals(&mut fresh, &mut back, &mut inbox, &clock, &mut |_| Ok(()))
            .expect("the intake runs");
        assert_eq!(taken, 2, "on time: the first two; the last one late");
        assert_eq!(clock.now(), end + 1, "waited out the window, no longer");
        assert_eq!(
            back.reveals.iter().map(|a| a.arrived).collect::<Vec<_>>(),
            vec![close + 10, end]
        );
        // After the window: nothing taken, no wait.
        let (mut again, mut late) = restarted(&store);
        let clock = ReplayClock::at(end + 50);
        let mut inbox = vec![reveals[0].clone()];
        assert_eq!(
            keep_taking_reveals(&mut again, &mut late, &mut inbox, &clock, &mut |_| Ok(())),
            Ok(0)
        );
        assert_eq!(clock.now(), end + 50);
        assert!(late.reveals.is_empty());
    }

    /// REVLOG-R5-1d: a round settled in this store (waiting for its results
    /// to be written, REVLOG-R4-1) takes no more reveals, whatever the clock:
    /// its result is fixed. A failed save stops the intake with the reveal
    /// not counted as saved (the caller keeps the round unfinished).
    #[test]
    fn a_settled_round_takes_no_more_reveals_and_a_failed_save_stops_the_intake() {
        let mut store = store_for(NUM);
        let (mut u, reveals) = kept_round(&mut store);
        let close = cfg().commit_close;
        assert!(u.receive_reveal(reveals[0].clone(), close + 1));
        assert!(u.receive_reveal(reveals[1].clone(), close + 1));
        let mut r = u.round.clone();
        for ar in &u.reveals {
            let (a, o, s, sig) = ar.reveal.clone();
            r.reveal(close + 1, a, o, s, sig).expect("reveal");
        }
        store.settle(&r, cfg().reveal_close + 1).expect("settles");
        u.round = r;
        let mut inbox = vec![reveals[2].clone()];
        let clock = ReplayClock::at(close + 2);
        assert_eq!(
            keep_taking_reveals(&mut store, &mut u, &mut inbox, &clock, &mut |_| Ok(())),
            Ok(0)
        );
        assert_eq!(inbox.len(), 1, "the inbox is not read for a settled round");
        // A failed save: Err, so the run keeps the round unfinished.
        let mut store = store_for(NUM);
        let (u, reveals) = kept_round(&mut store);
        store.keep_unfinished(u).expect("kept");
        let (mut fresh, mut back) = restarted(&store);
        let mut inbox = vec![reveals[0].clone()];
        let e = keep_taking_reveals(
            &mut fresh,
            &mut back,
            &mut inbox,
            &ReplayClock::at(close + 1),
            &mut |_| Err("disk full".into()),
        )
        .expect_err("the save failed");
        assert!(e.contains("disk full"), "{e}");
    }

    /// Review B5-M1: once the round holds its log snapshot (saved before the
    /// first receipt), a restart takes no reveal, even with the clock back
    /// inside the window (a replayed clock starts at 0, a wall clock can step
    /// back): its receipts, settlement or on-chain record may exist. Review
    /// B5-L2: an inbox that cannot deliver is not waited on.
    #[test]
    fn a_round_with_its_log_snapshot_takes_no_reveal_after_a_restart() {
        let mut store = store_for(NUM);
        let (mut u, reveals) = kept_round(&mut store);
        let close = cfg().commit_close;
        assert!(u.receive_reveal(reveals[0].clone(), close + 1));
        u.round.log = Some(LogSnapshot {
            block: None,
            posted: vec![],
        });
        store.keep_unfinished(u).expect("kept");
        let (mut fresh, mut back) = restarted(&store);
        assert!(
            back.round.log_snapshot().is_some(),
            "the snapshot is in the book"
        );
        let clock = ReplayClock::at(close + 1);
        let mut inbox = vec![reveals[2].clone()];
        assert_eq!(
            keep_taking_reveals(&mut fresh, &mut back, &mut inbox, &clock, &mut |_| Ok(())),
            Ok(0)
        );
        assert_eq!(inbox.len(), 1, "not read");
        assert_eq!(back.reveals.len(), 1);
        // NoInbox: nothing can come, so the window is not waited out.
        let mut store = store_for(NUM);
        let (u, _) = kept_round(&mut store);
        store.keep_unfinished(u).expect("kept");
        let (mut fresh, mut back) = restarted(&store);
        let clock = ReplayClock::at(close + 1);
        assert_eq!(
            keep_taking_reveals(&mut fresh, &mut back, &mut NoInbox, &clock, &mut |_| Ok(())),
            Ok(0)
        );
        assert_eq!(
            clock.now(),
            close + 1,
            "no wait for an inbox that cannot deliver"
        );
    }

    /// Review REVLOG-R4-1: a settled round stays kept until its results are
    /// written; the book is not saved meanwhile (on disk it holds the round
    /// unsettled); only a settled round is dropped as published.
    #[test]
    fn a_settled_round_leaves_the_book_only_once_published() {
        let mut store = store_for(NUM);
        let (mut u, reveals) = kept_round(&mut store);
        let dir = std::env::temp_dir().join(format!("onyx-r4-pub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("book.json");
        assert_eq!(
            store.published(&spec(NUM), &u.round.round_id),
            Err(ObserveError::RoundNotSettled),
            "an unsettled round is never dropped"
        );
        store.save_book(&path).expect("an unsettled round is saved");
        for r in reveals {
            u.round
                .reveal(cfg().commit_close + 1, r.0, r.1, r.2, r.3)
                .expect("reveal");
        }
        store.settle(&u.round, 1_201).expect("settles");
        assert!(store.unfinished(&spec(NUM)).is_some(), "still kept");
        assert!(store.awaiting_publication());
        let err = store.save_book(&path).expect_err("not saved meanwhile");
        assert!(err.contains("waits for its results"), "{err}");
        let mut fresh = store_for(NUM);
        fresh.load_book(&path).expect("the saved book");
        assert!(
            fresh.unfinished(&spec(NUM)).is_some(),
            "on disk the round is unsettled: a restart publishes it again"
        );
        store
            .published(&spec(NUM), &u.round.round_id)
            .expect("published");
        assert!(store.unfinished(&spec(NUM)).is_none());
        store.save_book(&path).expect("saved once published");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SEC-R1-F1: only a value outcome is recorded on chain, and a fixed
    /// subject's window is closed by any outcome (a dated or period subject
    /// stays open after a non-value round: `onyx_source_edgar` tests).
    #[test]
    fn only_value_outcomes_are_recorded_on_chain_and_fixed_windows_close() {
        let mut store = store_for(NUM);
        let three: Vec<Observer> = (1..=3)
            .map(|i| Observer {
                address: addr(i),
                bond: 1_000,
            })
            .collect();
        let mut out = open_in(&mut store, spec(NUM), cfg(), three.clone()).expect("open");
        play(
            &mut out,
            &[
                (1, Answer::Unavailable),
                (2, Answer::Unavailable),
                (3, Answer::Unavailable),
            ],
        );
        let s = store.settle(&out, 1_201).expect("settle");
        assert_eq!(*s.outcome(), ObservationOutcome::SourceUnavailable);
        assert_eq!(store.onchain_record(&s.result_hash()), None);
        assert_eq!(
            open_in(&mut store, spec(NUM), cfg(), three).err(),
            Some(ObserveError::FeedAlreadySettled),
            "a fixed subject's window is one fact, closed by any outcome"
        );
        let mut fresh = store_for(NUM);
        let v = settled_into(
            &mut fresh,
            &[
                (1, Answer::Number(5)),
                (2, Answer::Number(5)),
                (3, Answer::Number(5)),
            ],
        );
        let rec = fresh.onchain_record(&v.result_hash()).expect("a value");
        assert_eq!(rec.result, v.result_hash());
        assert_eq!(rec.spec_id, v.spec_id());
    }

    /// L2R2-F6: `backs` takes the feed and checks its id, so a caller cannot
    /// read a scale-2 value (123.45) as the integer 12345.
    #[test]
    fn backs_reads_kind_and_scale_from_the_feed_it_checks() {
        let mut r = round(NUM, 3);
        play(
            &mut r,
            &[
                (1, Answer::Number(12_345)),
                (2, Answer::Number(12_345)),
                (3, Answer::Number(12_345)),
            ],
        );
        let s = r.settle(1_201).expect("settle");
        let m = VerificationMethod::ReObserved {
            spec: s.spec_id(),
            result: s.result_hash(),
        };
        let int_feed = spec(ObservationKind::Numeric {
            scale: 0,
            tolerance: 50,
        });
        assert_eq!(
            s.backs(&int_feed, &m, &Value::Int(12_345)),
            Err(ObserveError::ClaimNotBacked(
                "feed is not this round's feed"
            ))
        );
        assert_eq!(
            s.backs(
                &spec(NUM),
                &m,
                &Value::Fixed {
                    mantissa: 12_345,
                    scale: 2
                }
            ),
            Ok(())
        );
    }

    /// L2R2-F4: during an outage a stale guess costs the small slash, so
    /// guessing never beats an honest `Unavailable` (which is never slashed).
    #[test]
    fn a_stale_guess_in_an_outage_costs_the_small_slash() {
        let mut r = round(NUM, 5);
        play(
            &mut r,
            &[
                (1, Answer::Unavailable),
                (2, Answer::Unavailable),
                (3, Answer::Unavailable),
                (4, Answer::Unavailable),
                (5, Answer::Number(100)),
            ],
        );
        let s = r.settle(1_201).expect("settle");
        assert_eq!(*s.outcome(), ObservationOutcome::SourceUnavailable);
        assert_eq!(status_of(&s, 5), ObserverStatus::Missed);
        assert_eq!(status_of(&s, 1), ObserverStatus::NoCall);
        assert_eq!(s.entries(), [(addr(5), -20)]);
    }

    /// L2R2-F5: with honest answers drifting between one and two tolerances,
    /// a far minority that forces `NoResult` is still slashed, because a
    /// strict majority clusters within twice the tolerance. An honest split
    /// with no such majority judges nobody.
    #[test]
    fn a_far_minority_cannot_force_a_free_no_result() {
        let kind = ObservationKind::Numeric {
            scale: 0,
            tolerance: 50,
        };
        let mut r = round(kind, 5);
        play(
            &mut r,
            &[
                (1, Answer::Number(0)),
                (2, Answer::Number(40)),
                (3, Answer::Number(90)),
                (4, Answer::Number(10_000)),
                (5, Answer::Number(10_000)),
            ],
        );
        let s = r.settle(1_201).expect("settle");
        assert_eq!(*s.outcome(), ObservationOutcome::NoResult);
        for i in 1..=3 {
            assert_eq!(status_of(&s, i), ObserverStatus::NoCall);
        }
        assert_eq!(status_of(&s, 4), ObserverStatus::Outlier);
        assert_eq!(status_of(&s, 5), ObserverStatus::Outlier);
        assert_eq!(s.entries(), [(addr(4), -100), (addr(5), -100)]);
        // 2 vs 2 with one silent member: no strict majority clusters, so the
        // far side is not judged (it may be the honest side).
        let mut split = round(kind, 5);
        play(
            &mut split,
            &[
                (1, Answer::Number(0)),
                (2, Answer::Number(0)),
                (3, Answer::Number(1_000)),
                (4, Answer::Number(1_000)),
            ],
        );
        let s = split.settle(1_201).expect("settle");
        assert_eq!(*s.outcome(), ObservationOutcome::NoResult);
        assert!(s
            .statuses()
            .iter()
            .all(|(_, st)| *st != ObserverStatus::Outlier));
    }

    /// L2R2-F5 property: an honest strict majority whose numbers lie within
    /// twice the tolerance of each other is never slashed as an outlier,
    /// whatever the minority reveals, with or without a settled value.
    #[test]
    fn an_honest_majority_within_twice_the_tolerance_is_never_an_outlier() {
        let tol: u128 = 50;
        let kind = ObservationKind::Numeric {
            scale: 0,
            tolerance: tol,
        };
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % m
        };
        for _ in 0..500 {
            let members = 3 + next(7) as u8;
            let q = quorum(usize::from(members)) as u8;
            let base = next(10_000) as i128;
            let mut answers: Vec<(u8, Answer)> = (1..=q)
                .map(|i| (i, Answer::Number(base + next(2 * tol as u64 + 1) as i128)))
                .collect();
            for i in q + 1..=members {
                answers.push((
                    i,
                    match next(3) {
                        0 => Answer::Unavailable,
                        1 => Answer::Number(next(1_000_000) as i128 - 500_000),
                        _ => Answer::Number(base + next(400) as i128 - 200),
                    },
                ));
            }
            let mut r = round(kind, members);
            play(&mut r, &answers);
            let s = r.settle(1_201).expect("settle");
            for i in 1..=q {
                assert_ne!(status_of(&s, i), ObserverStatus::Outlier, "{answers:?}");
            }
        }
    }

    fn pool_of(n: u8, bond: u128) -> Vec<Observer> {
        (1..=n)
            .map(|i| Observer {
                address: addr(i),
                bond,
            })
            .collect()
    }

    /// L2-F2: the observers are the draw of the announced pool and beacon,
    /// never a set the coordinator chose; a pool member who was not drawn
    /// cannot commit, and a consumer refuses a hand-picked set.
    #[test]
    fn observers_are_the_draw_and_a_hand_picked_set_is_refused() {
        let mut store = store_for(NUM);
        let pool = pool_of(9, 1_000);
        let snapshot = store
            .announce_draw(&spec(NUM), &cfg(), &pool, 50, &SimulatedChain::at(40))
            .expect("announce");
        let mut r = store
            .open_round(
                spec(NUM),
                cfg(),
                snapshot.clone(),
                &SimulatedChain::with_beacon(beacon()),
            )
            .expect("open");
        let drawn: Vec<MinerAddress> = r.observers().iter().map(|o| o.address).collect();
        assert_eq!(drawn.len(), 3);
        let again = draw_observers(&spec(NUM).id(), &snapshot, &r.draw(), 3, 100);
        assert_eq!(again, r.observers(), "the draw is a pure function");
        let outsider = (1..=9u8)
            .map(addr)
            .find(|a| !drawn.contains(a))
            .expect("one");
        let h = observation_commit(&r.round_id(), &outsider, &obs(Answer::Number(1)), &SALT);
        assert_eq!(
            r.commit_as(1_070, outsider, h),
            Err(ObserveError::NotObserver)
        );
        // A consumer recomputes the draw and refuses any other set.
        let mut picked = r.observers().to_vec();
        picked[0] = Observer {
            address: outsider,
            bond: 1_000,
        };
        picked.sort_by_key(|o| o.address);
        assert_eq!(
            check_draw(&spec(NUM), &cfg(), &snapshot, &r.draw(), &picked),
            Err(ObserveError::NotTheDraw)
        );
        assert_eq!(
            check_draw(&spec(NUM), &cfg(), &snapshot, &r.draw(), r.observers()),
            Ok(())
        );
        // Another beacon draws another set (with overwhelming probability).
        let other = DrawSeed {
            beacon: keccak256(b"other beacon"),
            ..r.draw()
        };
        assert_ne!(
            draw_observers(&spec(NUM).id(), &snapshot, &other, 3, 100),
            drawn
                .iter()
                .map(|a| Observer {
                    address: *a,
                    bond: 1_000
                })
                .collect::<Vec<_>>()
        );
    }

    /// L2-F2 grinding: one announcement per fact and attempt, a beacon height
    /// that does not exist yet, and an open that must match the announcement;
    /// a draw opens once.
    #[test]
    fn a_draw_is_announced_once_before_its_beacon_and_opens_once() {
        let mut store = store_for(NUM);
        let pool = pool_of(9, 1_000);
        assert_eq!(
            store
                .announce_draw(&spec(NUM), &cfg(), &pool, 40, &SimulatedChain::at(40))
                .err(),
            Some(ObserveError::BeaconNotFuture)
        );
        assert_eq!(
            store
                .open_round(
                    spec(NUM),
                    cfg(),
                    pool.clone(),
                    &SimulatedChain::with_beacon(beacon())
                )
                .err(),
            Some(ObserveError::DrawNotAnnounced)
        );
        let snapshot = store
            .announce_draw(&spec(NUM), &cfg(), &pool, 50, &SimulatedChain::at(40))
            .expect("announce");
        assert_eq!(
            store
                .announce_draw(&spec(NUM), &cfg(), &pool, 60, &SimulatedChain::at(55))
                .err(),
            Some(ObserveError::DrawAlreadyAnnounced),
            "no second announcement after the beacon is known"
        );
        let mut smaller = snapshot.clone();
        smaller.pop();
        assert_eq!(
            store
                .open_round(
                    spec(NUM),
                    cfg(),
                    smaller,
                    &SimulatedChain::with_beacon(beacon())
                )
                .err(),
            Some(ObserveError::DrawMismatch)
        );
        let mut later = cfg();
        later.reveal_close += 1;
        assert_eq!(
            store
                .open_round(
                    spec(NUM),
                    later,
                    snapshot.clone(),
                    &SimulatedChain::with_beacon(beacon())
                )
                .err(),
            Some(ObserveError::DrawMismatch)
        );
        store
            .open_round(
                spec(NUM),
                cfg(),
                snapshot.clone(),
                &SimulatedChain::with_beacon(beacon()),
            )
            .expect("open");
        assert_eq!(
            store
                .open_round(
                    spec(NUM),
                    cfg(),
                    snapshot,
                    &SimulatedChain::with_beacon(keccak256(b"x"))
                )
                .err(),
            Some(ObserveError::DrawMismatch),
            "a draw opens once: no second beacon"
        );
    }

    /// Exposure (L31-F2 lesson): a bond sits on as many open rounds as it
    /// covers slashes; the snapshot counts free bond, and settling releases it.
    #[test]
    fn a_bond_is_never_committed_twice() {
        let mut a = store_for(NUM);
        // One bond covers exactly one slash of 100.
        let pool = pool_of(3, 100);
        let snap = a
            .announce_draw(&spec(NUM), &cfg(), &pool, 50, &SimulatedChain::at(40))
            .expect("a");
        let mut r = a
            .open_round(
                spec(NUM),
                cfg(),
                snap,
                &SimulatedChain::with_beacon(beacon()),
            )
            .expect("open a");
        assert_eq!(a.exposure(&addr(1)), 100);
        // The same store, another feed's fact: the free bond is now 0.
        let mut other = spec(NUM);
        other.window_start += 10_000;
        other.window_end += 10_000;
        let mut other_cfg = cfg();
        other_cfg.commit_close += 10_000;
        other_cfg.reveal_close += 10_000;
        let snap2 = a
            .announce_draw(&other, &other_cfg, &pool, 50, &SimulatedChain::at(40))
            .expect("announce b");
        assert!(snap2.iter().all(|o| o.bond == 0), "free bond is counted");
        assert!(matches!(
            a.open_round(
                other.clone(),
                other_cfg,
                snap2,
                &SimulatedChain::with_beacon(beacon())
            ),
            Err(ObserveError::TooFewObservers { have: 0, .. })
        ));
        play(
            &mut r,
            &[
                (1, Answer::Number(5)),
                (2, Answer::Number(5)),
                (3, Answer::Number(5)),
            ],
        );
        a.settle(&r, 1_201).expect("settle a");
        assert_eq!(a.exposure(&addr(1)), 0, "settling releases the slash");
    }
    /// A chain whose latest reads fail and whose final block `block` shows
    /// `record` at every attempt (mutation L4): the final-block arms of
    /// `gone_proof` and `confirm_announced`.
    struct FinalView {
        block: u64,
        record: Option<DrawRecord>,
    }

    impl ChainView for FinalView {
        fn height(&self) -> Result<u64, String> {
            Ok(self.block)
        }
        fn block_hash(&self, _: u64) -> Result<Option<Hash32>, String> {
            Err("unused".into())
        }
    }

    impl DrawChain for FinalView {
        fn draw_count(&self, _: &OnchainFact) -> Result<u32, String> {
            Err("unused".into())
        }
        fn draw_budget(&self, _: &OnchainFact) -> Result<(u32, u32), String> {
            Err("unused".into())
        }
        fn announce(&mut self, _: &OnchainDraw) -> Result<(), String> {
            Err("unused".into())
        }
        fn fix_beacon(&mut self, _: &OnchainDraw) -> Result<DrawRecord, String> {
            Err("unused".into())
        }
        fn draw_of(&self, _: &OnchainFact, _: u32) -> Result<Option<DrawRecord>, String> {
            Err("the latest block is not read here".into())
        }
        fn draw_of_final(
            &self,
            _: &OnchainFact,
            _: u32,
        ) -> Result<(u64, Option<DrawRecord>), String> {
            Ok((self.block, self.record))
        }
        fn post_commit(
            &mut self,
            _: &OnchainFact,
            _: u32,
            _: &MinerAddress,
            _: &Hash32,
            _: &Hash32,
            _: &ObserverSig,
        ) -> Result<Posted, String> {
            Err("unused".into())
        }
        fn post_receipt(
            &mut self,
            _: &OnchainFact,
            _: u32,
            _: &MinerAddress,
            _: &Hash32,
            _: &Hash32,
            _: &ObserverSig,
        ) -> Result<Posted, String> {
            Err("unused".into())
        }
        fn log_block(&self) -> Result<Option<LogBlock>, String> {
            Err("unused".into())
        }
        fn posted_commit(
            &self,
            _: u64,
            _: &OnchainFact,
            _: u32,
            _: &MinerAddress,
            _: &Hash32,
        ) -> Result<Option<PostedCommit>, String> {
            Err("unused".into())
        }
        fn transcript_complete(&self, _: &TranscriptLogQuery) -> Result<bool, String> {
            Err("unused".into())
        }
    }

    /// Mutation L4: what ONE final block proves about the book's draw
    /// (beacon height 1,000; its `blockhash` window ends at 1,256).
    #[test]
    fn final_block_proofs_at_their_edges() {
        let draw = OnchainDraw {
            fact: OnchainFact {
                metric_key: keccak256(b"metric"),
                subject_key: keccak256(b"subject"),
                window_end: 0,
                dated: false,
                template: Hash32::ZERO,
            },
            attempt: 2,
            commit: keccak256(b"commit"),
            beacon_height: 1_000,
        };
        let ours = |beacon: Hash32| DrawRecord {
            chain_id: 31_337,
            registry: [0; 20],
            attempt: 2,
            draw_count: 3,
            announced_at: 990,
            commit: draw.commit,
            beacon_height: 1_000,
            beacon,
        };
        let other = DrawRecord {
            commit: keccak256(b"another draw"),
            ..ours(Hash32::ZERO)
        };
        let at = |block, record| FinalView { block, record };
        let w = ChainWait::NOW;
        let window_end = 1_000 + BLOCKHASH_WINDOW;
        // Announced, beacon zero: fixable up to the window's end, not after.
        assert!(gone_proof(&at(window_end, Some(ours(Hash32::ZERO))), &draw, &w).is_err());
        assert!(matches!(
            gone_proof(&at(window_end + 1, Some(ours(Hash32::ZERO))), &draw, &w),
            Ok(Release::Unfixable(_))
        ));
        assert!(gone_proof(&at(window_end + 1, Some(ours(beacon()))), &draw, &w).is_err());
        // Another draw at its attempt: it never lands.
        assert!(matches!(
            gone_proof(&at(500, Some(other)), &draw, &w),
            Ok(Release::NeverLands(_))
        ));
        // None: never lands from its beacon height on, not before.
        assert!(matches!(
            gone_proof(&at(1_000, None), &draw, &w),
            Ok(Release::NeverLands(_))
        ));
        assert!(gone_proof(&at(999, None), &draw, &w).is_err());
        // The same final block decides `confirm_announced` when the latest
        // block cannot be read.
        assert!(matches!(
            confirm_announced(&at(1_100, Some(ours(Hash32::ZERO))), &draw, &w),
            Confirm::Landed
        ));
        assert!(matches!(
            confirm_announced(&at(1_100, Some(other)), &draw, &w),
            Confirm::Gone(Release::NeverLands(_))
        ));
        assert!(matches!(
            confirm_announced(&at(1_000, None), &draw, &w),
            Confirm::Gone(Release::NeverLands(_))
        ));
        assert!(matches!(
            confirm_announced(&at(999, None), &draw, &w),
            Confirm::Unknown(_)
        ));
    }

    /// Mutation L4: a pool too small for the draw draws nobody (never a
    /// partial set); the refusal counts only members with the minimum bond;
    /// a held draw keeps the chain's count from passing it.
    #[test]
    fn draw_sizes_bonds_and_held_counts() {
        let seed = |pool: &[Observer]| DrawSeed {
            pool: pool_hash(pool),
            beacon_height: 50,
            beacon: beacon(),
            attempt: 0,
        };
        let id = spec(NUM).id();
        let two = pool_of(2, 1_000);
        assert!(draw_observers(&id, &two, &seed(&two), 3, 100).is_empty());
        assert!(draw_observers(&id, &two, &seed(&two), 0, 100).is_empty());
        assert_eq!(draw_observers(&id, &two, &seed(&two), 2, 100).len(), 2);
        // Three members, one under the minimum bond: two eligible of three needed.
        let mut pool = pool_of(2, 1_000);
        pool.push(Observer {
            address: addr(3),
            bond: 99,
        });
        assert_eq!(
            check_draw(&spec(NUM), &cfg(), &pool, &seed(&pool), &[]),
            Err(ObserveError::TooFewObservers { have: 2, need: 3 })
        );
        // The book holds attempt 0: the chain's count may be 1 (that draw),
        // not 2 (a later one would make the book forget it).
        let mut store = store_for(NUM);
        store
            .announce_draw(
                &spec(NUM),
                &cfg(),
                &pool_of(9, 1_000),
                50,
                &SimulatedChain::at(40),
            )
            .expect("announce");
        assert_eq!(
            store.sync_draws(&spec(NUM), 2),
            Err(ObserveError::DrawHeld(0))
        );
        assert_eq!(store.sync_draws(&spec(NUM), 1), Ok(()));
    }

    /// Mutation L4: a scale-0 feed's value is an integer; a `Fixed` value
    /// with scale 0 (not a valid value) never backs a claim.
    #[test]
    fn a_scale_zero_feed_is_backed_only_by_an_integer() {
        let int = ObservationKind::Numeric {
            scale: 0,
            tolerance: 50,
        };
        let mut r = round(int, 3);
        play(
            &mut r,
            &[
                (1, Answer::Number(12_345)),
                (2, Answer::Number(12_345)),
                (3, Answer::Number(12_345)),
            ],
        );
        let s = r.settle(1_201).expect("settle");
        let m = VerificationMethod::ReObserved {
            spec: s.spec_id(),
            result: s.result_hash(),
        };
        assert_eq!(s.backs(&spec(int), &m, &Value::Int(12_345)), Ok(()));
        assert!(s
            .backs(
                &spec(int),
                &m,
                &Value::Fixed {
                    mantissa: 12_345,
                    scale: 0
                }
            )
            .is_err());
    }
    /// Mutation L4: `release_draw` checks every proof against the book's own
    /// draw, whoever built it: a final block that shows the draw, a final
    /// block below its beacon height, or a round id that is not the draw's
    /// round releases nothing, and the draw stays held.
    #[test]
    fn release_draw_refuses_proofs_that_do_not_hold() {
        let mut store = store_for(NUM);
        store
            .announce_draw(
                &spec(NUM),
                &cfg(),
                &pool_of(9, 1_000),
                50,
                &SimulatedChain::at(40),
            )
            .expect("announce");
        let d = store.held_draw(&spec(NUM)).expect("held");
        let ours = DrawRecord {
            chain_id: 31_337,
            registry: [0; 20],
            attempt: d.attempt,
            draw_count: d.attempt + 1,
            announced_at: 40,
            commit: d.commit,
            beacon_height: d.beacon_height,
            beacon: Hash32::ZERO,
        };
        let refused = |store: &mut ObservationResults, why: Release| {
            matches!(
                store.release_draw(&spec(NUM), why),
                Err(ObserveError::NoReleaseProof(_))
            )
        };
        let bh = d.beacon_height;
        assert!(refused(
            &mut store,
            Release::NeverLands(FinalRead {
                block: bh + 10,
                record: Some(ours),
            })
        ));
        assert!(refused(
            &mut store,
            Release::NeverLands(FinalRead {
                block: bh - 1,
                record: None,
            })
        ));
        assert!(refused(
            &mut store,
            Release::Unfixable(FinalRead {
                block: bh + BLOCKHASH_WINDOW,
                record: Some(ours),
            })
        ));
        assert!(refused(
            &mut store,
            Release::Published(keccak256(b"no such round"))
        ));
        // Another draw with a zero beacon past the window is no proof for ours.
        let other = DrawRecord {
            commit: keccak256(b"another draw"),
            ..ours
        };
        assert!(refused(
            &mut store,
            Release::Unfixable(FinalRead {
                block: bh + BLOCKHASH_WINDOW + 1,
                record: Some(other),
            })
        ));
        assert_eq!(store.held_draw(&spec(NUM)), Some(d), "the draw stays");
        // The proof that does hold releases it: nothing at its attempt as of
        // a final block at its beacon height.
        assert_eq!(
            store.release_draw(
                &spec(NUM),
                Release::NeverLands(FinalRead {
                    block: bh,
                    record: None,
                })
            ),
            Ok(())
        );
        assert_eq!(store.held_draw(&spec(NUM)), None);
        // A draw opened here is never released as one that never landed.
        let mut store = store_for(NUM);
        let snapshot = store
            .announce_draw(
                &spec(NUM),
                &cfg(),
                &pool_of(9, 1_000),
                50,
                &SimulatedChain::at(40),
            )
            .expect("announce");
        store
            .open_round(
                spec(NUM),
                cfg(),
                snapshot,
                &SimulatedChain::with_beacon(beacon()),
            )
            .expect("open");
        let d = store.held_draw(&spec(NUM)).expect("held");
        assert!(refused(
            &mut store,
            Release::NeverLands(FinalRead {
                block: d.beacon_height,
                record: None,
            })
        ));
        assert!(refused(&mut store, Release::NeverSent));
        assert_eq!(store.held_draw(&spec(NUM)), Some(d));
    }

    // ---- ROADMAP E8 (THREATS MUT-2): the draw invariant's `Published`
    // proof, `published`, `unwritable_draw` and `settle`'s round checks,
    // directly (before, only the `revlog_r8` property test reached them).

    /// A round opened in `store` from three observers, each answering 5,
    /// not kept.
    fn opened_and_played(store: &mut ObservationResults) -> ObservationRound {
        let mut r = open_in(store, spec(NUM), cfg(), pool_of(3, 1_000)).expect("open");
        play(
            &mut r,
            &[
                (1, Answer::Number(5)),
                (2, Answer::Number(5)),
                (3, Answer::Number(5)),
            ],
        );
        r
    }

    /// `kept_round`, revealed and settled in `store`.
    fn kept_and_settled(store: &mut ObservationResults) -> UnfinishedRound {
        let (mut u, reveals) = kept_round(store);
        for r in reveals {
            u.round
                .reveal(cfg().commit_close + 1, r.0, r.1, r.2, r.3)
                .expect("reveal");
        }
        store.settle(&u.round, 1_201).expect("settles");
        u
    }

    fn released(store: &mut ObservationResults, id: Hash32) -> Result<(), ObserveError> {
        store.release_draw(&spec(NUM), Release::Published(id))
    }

    /// `Published` releases the book's draw only for that draw's round
    /// (the kept one if any, else the one opened here), and only once it is
    /// settled here or kept as a failed attempt.
    #[test]
    fn a_published_release_needs_the_draws_own_round_settled_or_failed() {
        // Opened here, not kept.
        let mut store = store_for(NUM);
        let r = opened_and_played(&mut store);
        assert!(
            matches!(
                released(&mut store, r.round_id),
                Err(ObserveError::NoReleaseProof(_))
            ),
            "not settled yet"
        );
        // Another round settled here is not this draw's round.
        let foreign = keccak256(b"another settled round");
        store.rounds.insert(foreign);
        assert!(matches!(
            released(&mut store, foreign),
            Err(ObserveError::NoReleaseProof(_))
        ));
        assert!(store.held_draw(&spec(NUM)).is_some());
        store.settle(&r, 1_201).expect("settle");
        assert_eq!(released(&mut store, r.round_id), Ok(()));
        assert_eq!(store.held_draw(&spec(NUM)), None);

        // Kept and settled: released, and the kept round leaves the book.
        let mut store = store_for(NUM);
        let u = kept_and_settled(&mut store);
        assert_eq!(released(&mut store, u.round.round_id), Ok(()));
        assert!(store.unfinished(&spec(NUM)).is_none());
        assert_eq!(store.held_draw(&spec(NUM)), None);

        // Kept, unsettled, not failed: no proof.
        let mut store = store_for(NUM);
        let (u, _) = kept_round(&mut store);
        assert!(matches!(
            released(&mut store, u.round.round_id),
            Err(ObserveError::NoReleaseProof(_))
        ));
        // A settled round id that is not the kept one: no proof either.
        store.rounds.insert(foreign);
        assert!(matches!(
            released(&mut store, foreign),
            Err(ObserveError::NoReleaseProof(_))
        ));
        assert!(store.unfinished(&spec(NUM)).is_some());

        // Kept as a failed attempt (never settled): another id releases
        // nothing; its own id releases it and clears the failed mark.
        let mut store = store_for(NUM);
        let (u, _) = kept_round(&mut store);
        store.keep_failed(u.clone()).expect("failed");
        store.attempt_published(&spec(NUM), &foreign);
        assert!(store.failed_attempt(&spec(NUM)).is_some());
        assert!(store.held_draw(&spec(NUM)).is_some());
        store.attempt_published(&spec(NUM), &u.round.round_id);
        assert!(store.failed_attempt(&spec(NUM)).is_none());
        assert!(store.unfinished(&spec(NUM)).is_none());
        assert!(store.failed.is_empty());
        assert_eq!(store.held_draw(&spec(NUM)), None);

        // A failed mark for a round kept on another attempt is no proof.
        let mut store = store_for(NUM);
        let (mut u, _) = kept_round(&mut store);
        store.keep_failed(u.clone()).expect("failed");
        let fact = store.fact_key_of(&spec(NUM)).expect("fact");
        u.round.draw.attempt += 1;
        store.unfinished.insert(fact, u.clone());
        let opened = store.opened.values().next().copied().expect("opened");
        assert!(matches!(
            released(&mut store, opened),
            Err(ObserveError::NoReleaseProof(_))
        ));
    }

    /// `published` drops only the held draw's own settled round; a settled
    /// round that is not it changes nothing.
    #[test]
    fn published_drops_only_the_held_draws_round() {
        let mut store = store_for(NUM);
        let r = opened_and_played(&mut store);
        store.settle(&r, 1_201).expect("settle");
        assert!(store.unwritable_draw().is_some(), "settled, not published");
        assert_eq!(store.published(&spec(NUM), &r.round_id), Ok(()));
        assert_eq!(store.held_draw(&spec(NUM)), None);
        assert_eq!(store.unwritable_draw(), None);
        assert_eq!(
            store.published(&spec(NUM), &r.round_id),
            Ok(()),
            "idempotent"
        );

        // A kept, unsettled round; another round id settled here.
        let mut store = store_for(NUM);
        let (u, _) = kept_round(&mut store);
        let other = keccak256(b"settled elsewhere");
        store.rounds.insert(other);
        assert_eq!(store.published(&spec(NUM), &other), Ok(()));
        assert!(store.unfinished(&spec(NUM)).is_some());
        assert!(store.held_draw(&spec(NUM)).is_some());
        assert_eq!(
            store.published(&spec(NUM), &u.round.round_id),
            Err(ObserveError::RoundNotSettled)
        );
    }

    /// The book can be written while every held draw is its fact's latest
    /// and none has a settled round that is neither kept nor published.
    #[test]
    fn unwritable_draw_names_each_unwritable_state() {
        let mut store = store_for(NUM);
        store
            .announce_draw(
                &spec(NUM),
                &cfg(),
                &pool_of(3, 1_000),
                10,
                &SimulatedChain::at(0),
            )
            .expect("announce");
        assert_eq!(store.unwritable_draw(), None, "announced only");
        let fact = store.fact_key_of(&spec(NUM)).expect("fact");
        store.draws.insert(fact, 2);
        let why = store.unwritable_draw().expect("not the latest");
        assert!(why.contains("not its fact's latest"), "{why}");

        // Kept and settled: written as kept.
        let mut store = store_for(NUM);
        let _ = kept_and_settled(&mut store);
        assert_eq!(store.unwritable_draw(), None);
        // Settled and not kept: not writable until published.
        let mut store = store_for(NUM);
        let r = opened_and_played(&mut store);
        assert_eq!(store.unwritable_draw(), None, "opened, unsettled");
        store.settle(&r, 1_201).expect("settle");
        let why = store.unwritable_draw().expect("unwritable");
        assert!(why.contains("not kept and is not published"), "{why}");
    }

    /// Only the round opened here from its announced draw settles: a round
    /// whose id, spec, config, pool or beacon height differs from the draw's
    /// is refused, each on its own.
    #[test]
    fn settle_checks_the_round_against_its_draw() {
        let mut store = store_for(NUM);
        let r = opened_and_played(&mut store);
        let edits: [&dyn Fn(&mut ObservationRound); 5] = [
            &|x| x.round_id = keccak256(b"another id"),
            &|x| x.spec_id = keccak256(b"another spec"),
            &|x| x.cfg.reward += 1,
            &|x| x.pool[0].bond += 1,
            &|x| x.draw.beacon_height += 1,
        ];
        for e in edits {
            let mut x = r.clone();
            e(&mut x);
            assert_eq!(store.settle(&x, 1_201), Err(ObserveError::DrawMismatch));
        }
        assert!(store.settle(&r, 1_201).is_ok());
    }

    /// A chain proof is checked against every field of the book's draw: a
    /// record that differs in its attempt, commitment or beacon height alone
    /// is another draw, so the book's never lands.
    #[test]
    fn never_lands_compares_every_field_of_the_draw() {
        let base = |store: &mut ObservationResults| {
            store
                .announce_draw(
                    &spec(NUM),
                    &cfg(),
                    &pool_of(9, 1_000),
                    50,
                    &SimulatedChain::at(40),
                )
                .expect("announce");
            let d = store.held_draw(&spec(NUM)).expect("held");
            DrawRecord {
                chain_id: 31_337,
                registry: [0; 20],
                attempt: d.attempt,
                draw_count: d.attempt + 1,
                announced_at: 40,
                commit: d.commit,
                beacon_height: d.beacon_height,
                beacon: Hash32::ZERO,
            }
        };
        let edits: [&dyn Fn(&mut DrawRecord); 3] = [
            &|x| x.attempt += 1,
            &|x| x.commit = keccak256(b"another"),
            &|x| x.beacon_height += 1,
        ];
        for e in edits {
            let mut store = store_for(NUM);
            let mut rec = base(&mut store);
            let block = rec.beacon_height;
            e(&mut rec);
            assert_eq!(
                store.release_draw(
                    &spec(NUM),
                    Release::NeverLands(FinalRead {
                        block,
                        record: Some(rec),
                    })
                ),
                Ok(())
            );
            assert_eq!(store.held_draw(&spec(NUM)), None);
        }
    }

    /// `Unfixable` releases a fixed-subject draw only past the `blockhash`
    /// window with the book's own draw at a zero beacon, and counts it as
    /// released (announced on chain), not as never sent.
    #[test]
    fn unfixable_releases_only_past_the_window_with_a_zero_beacon() {
        let setup = |store: &mut ObservationResults| {
            store
                .announce_draw(
                    &spec(NUM),
                    &cfg(),
                    &pool_of(9, 1_000),
                    50,
                    &SimulatedChain::at(40),
                )
                .expect("announce");
            let d = store.held_draw(&spec(NUM)).expect("held");
            DrawRecord {
                chain_id: 31_337,
                registry: [0; 20],
                attempt: d.attempt,
                draw_count: d.attempt + 1,
                announced_at: 40,
                commit: d.commit,
                beacon_height: d.beacon_height,
                beacon: Hash32::ZERO,
            }
        };
        let mut store = store_for(NUM);
        let rec = setup(&mut store);
        let past = rec.beacon_height + BLOCKHASH_WINDOW + 1;
        let fixed = DrawRecord {
            beacon: beacon(),
            ..rec
        };
        assert!(matches!(
            store.release_draw(
                &spec(NUM),
                Release::Unfixable(FinalRead {
                    block: past,
                    record: Some(fixed),
                })
            ),
            Err(ObserveError::NoReleaseProof(_))
        ));
        assert_eq!(
            store.release_draw(
                &spec(NUM),
                Release::Unfixable(FinalRead {
                    block: past,
                    record: Some(rec),
                })
            ),
            Ok(())
        );
        assert_eq!(store.held_draw(&spec(NUM)), None);
        let fact = store.fact_key_of(&spec(NUM)).expect("fact");
        assert!(store.released.contains(&(fact, rec.attempt)));
        assert!(
            store.announcements.contains_key(&(fact, rec.attempt)),
            "counted: it was announced"
        );
    }

    /// ROADMAP B2: independent observers send their signed reveals through
    /// the spool ([`crate::observer_link`]). The intake takes each drawn
    /// observer's reveal as it arrives; a forged one (another observer's
    /// signature), one from an observer not drawn, and a rewritten second
    /// reveal (a replay with another answer) are non-reveals; the files stay
    /// in the spool, so a coordinator restarted inside the window reads them
    /// again without the observers resending (REVLOG-R5-1d); a reveal written
    /// after `reveal_close` is never taken.
    #[test]
    fn b2_reveals_through_the_spool_are_checked_and_survive_a_restart() {
        use crate::observer_link::{reveal_path, send_reveal, DirInbox};
        let dir = std::env::temp_dir().join(format!("onyx-b2-spool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = store_for(NUM);
        let (mut u, reveals) = kept_round(&mut store);
        let (close, end) = (cfg().commit_close, cfg().reveal_close);
        let round = u.round.round_id;
        // Observer 1 reveals; observer 2's file holds a forgery (observer 3's
        // signature); a stranger (not drawn) writes a valid reveal of its own.
        send_reveal(&dir, &round, 0, &reveals[0]).expect("send");
        let mut forged = reveals[1].clone();
        forged.3 = reveals[2].3;
        send_reveal(&dir, &round, 0, &forged).expect("send");
        let (sk9, a9) = test_keys::ordered(9);
        let o9 = obs(Answer::Number(6));
        let s9 = sign_reveal(&sk9, &round, 0, &o9, &SALT).expect("sig");
        send_reveal(&dir, &round, 0, &(a9, o9, SALT, s9)).expect("send");
        let mut inbox = DirInbox::open(&dir).expect("spool");
        let clock = ReplayClock::at(close + 1);
        // Observer 3 never reveals: the intake ends at reveal_close.
        let taken = keep_taking_reveals(&mut store, &mut u, &mut inbox, &clock, &mut |_| Ok(()))
            .expect("intake");
        assert_eq!(taken, 1, "only observer 1's reveal is one");
        assert_eq!(u.reveals.len(), 1);
        assert_eq!(u.reveals[0].reveal, reveals[0]);
        assert_eq!(u.reveals[0].arrived, close + 1);
        assert!(
            clock.now() > end,
            "waited out the window for observers 2 and 3"
        );

        // Restart inside the window: observer 2 now sends its real reveal and
        // observer 1 rewrites its file with another answer (a replay).
        let mut store = store_for(NUM);
        let (mut u, _) = kept_round(&mut store);
        send_reveal(&dir, &round, 0, &reveals[1]).expect("send");
        let clock = ReplayClock::at(close + 10);
        let mut inbox = DirInbox::open(&dir).expect("spool");
        assert_eq!(
            keep_taking_reveals(&mut store, &mut u, &mut inbox, &clock, &mut |_| Ok(())),
            Ok(2),
            "the spool still holds observer 1's reveal: read again after the restart"
        );
        let mut other = reveals[0].clone();
        other.1 = obs(Answer::Number(7));
        let sk1 = test_keys::ordered_secret(&addr(1)).expect("key");
        other.3 = sign_reveal(&sk1, &round, 0, &other.1, &SALT).expect("sig");
        send_reveal(&dir, &round, 0, &other).expect("send");
        assert_eq!(
            inbox.take(&round, &[addr(1)], 0),
            vec![other.clone()],
            "the rewritten file is read"
        );
        assert!(
            !u.receive_reveal(other, close + 20),
            "the first reveal is kept"
        );
        assert_eq!(u.reveals.len(), 2);

        // After reveal_close nothing is taken, even with every file there.
        send_reveal(&dir, &round, 0, &reveals[2]).expect("send");
        let mut store = store_for(NUM);
        let (mut u, _) = kept_round(&mut store);
        let clock = ReplayClock::at(end + 1);
        assert_eq!(
            keep_taking_reveals(&mut store, &mut u, &mut inbox, &clock, &mut |_| Ok(())),
            Ok(0)
        );
        assert!(u.reveals.is_empty());
        assert!(reveal_path(&dir, &addr(3), &round).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ROADMAP B3 (THREATS REVLOG-4): the receipt key is rotated for new
    /// draws only. A round whose draw snapshotted the old key is never
    /// receipted with the new one: the run stops before the log read and
    /// any receipt, the round stays unfinished, and the draw's own key
    /// finishes it.
    #[test]
    fn b3_a_rotated_receipt_key_never_receipts_an_earlier_draw() {
        let mut store = store_for(NUM);
        let (u, reveals) = kept_round(&mut store);
        let (close, _) = (cfg().commit_close, cfg().reveal_close);
        let mut u = u;
        for r in &reveals {
            assert!(u.receive_reveal(r.clone(), close + 1));
        }
        let fact = store.onchain_fact(&spec(NUM)).expect("fact");
        let mut reg = SimulatedRegistry::new();
        let old = test_keys::secret("b3 rotation", 0);
        let new = test_keys::secret("b3 rotation", 1);
        reg.log.signers.insert(
            (fact.registry_key(), 0),
            secret_address(&old).expect("addr"),
        );
        // The deployment rotated to `new` after this draw was announced.
        reg.receipt_signer = secret_address(&new).expect("addr");
        reg.time = close + 1;
        let mut round = u.round.clone();
        let e = finish_reveals(
            &mut round,
            &mut reg,
            &fact,
            &new,
            &u.reveals,
            &ChainWait::NOW,
            &mut |_| Ok(()),
        )
        .expect_err("another key than the draw's");
        assert!(e.contains("rotated"), "{e}");
        assert!(round.log.is_none(), "stopped before the log read");
        let receipts = |reg: &SimulatedRegistry| {
            (1..=3)
                .filter(|i| {
                    reg.log
                        .receipt(&fact.registry_key(), 0, &addr(*i), &u.round.round_id)
                        .is_some()
                })
                .count()
        };
        assert_eq!(receipts(&reg), 0, "no receipt was posted");
        finish_reveals(
            &mut round,
            &mut reg,
            &fact,
            &old,
            &u.reveals,
            &ChainWait::NOW,
            &mut |_| Ok(()),
        )
        .expect("the draw's own key finishes it");
        assert_eq!(round.reveals.len(), 3);
        assert_eq!(receipts(&reg), 3);
    }
}
