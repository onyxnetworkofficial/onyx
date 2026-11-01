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

// The draw (`draw_and_open` and the checks a client reruns): observe/draw.rs.

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

// `impl ObservationResults`: observe/results.rs.

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

// `impl ObservationRound`: observe/round.rs.

// The glob also brings draw.rs's `pub(super)` items into this module (and so to its tests).
mod draw;
pub use draw::*;
mod results;
mod round;

#[cfg(test)]
#[path = "observe_tests.rs"]
mod tests;
