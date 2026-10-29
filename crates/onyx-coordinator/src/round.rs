//! One round of commit-reveal (ADR-0018). This is the offchain model of the
//! settlement contract (M6 moves it onchain with the same rules and hashes).
//!
//! - Open: every shard's spec validates against the pinned registry, and
//!   everything it pins is in the CAS (recipe, label set, every attestation
//!   the set cites, params). A shard nobody can fetch never opens (q10 a).
//! - Commit (until `commit_close`): one bonded commit per (miner, shard); the
//!   commit hash binds round, shard, commitment, miner and salt (ADR-0011).
//! - Reveal (until `reveal_close`): the commitment must hash to the commit,
//!   match the shard's pinned fields, and its rows must be in the CAS.
//! - Close: per shard, reveals in commit order; committers who did not
//!   reveal forfeit their bond. Correctness is decided by validators (M4).
//!
//! Every commit is signed by the miner's key (EIP-191 over
//! [`commit_message`]), so nobody can take another miner's slot (L18-F2).
//! Heights only move forward and a closed round stays closed (L18-F3).

use crate::cas::{Cas, CasExt, Kind};
use crate::dispute::DisputeConfig;
use crate::recompute::{FailureAllowList, PreflightError, RecomputeEnv};
use crate::validate::{
    deterrence_holds, Ledger, ValidationConfig, MIN_DRAW_SLACK, MIN_PENDING_GAP,
    MIN_VERDICT_WINDOW, PPM,
};
use onyx_core::{
    commit_hash, keccak256, Commitment, CommitmentError, Hash32, MinerAddress, Registry, RuleSet,
    ShardSpec,
};
use onyx_labels::attest::eip191_recover;
use onyx_labels::{ATTESTATION_EVIDENCE, MAX_ATTESTATIONS, MAX_ATTESTATION_BYTES};
use onyx_recipe_host::Host;
use std::collections::{BTreeMap, BTreeSet};

/// Text a miner signs (EIP-191) to post a commit. Every field is fixed-format.
/// `round_tag` ([`Round::tag`]) binds the network domain, round number, timing,
/// bond and shard set, so a signed commit cannot be replayed into another
/// round or network (L19-F3).
pub fn commit_message(
    round_tag: &Hash32,
    shard: &Hash32,
    commit_hash: &Hash32,
    bond: u128,
) -> String {
    format!(
        "Onyx commit v1\nround: {round_tag}\nshard: {shard}\ncommit: {commit_hash}\nbond: {bond}"
    )
}

/// Round tag (L20-F4, L24-F3, L27-F1, ADR-0021): `keccak256("onyx-round-v4" ‖ domain ‖ 0x00 ‖
/// ledger id ‖ id u64 BE ‖ commit_close u64 BE ‖ reveal_close u64 BE ‖ min_bond u128 BE ‖
/// validator_set_root ‖ canary_commit ‖ beacon_height u64 BE ‖
/// verdict_commit_close u64 BE ‖ verdict_reveal_close u64 BE ‖
/// canary_open_close u64 BE ‖ pending_close u64 BE ‖ rate_ppm u64 BE ‖ miner_reward u128 BE ‖
/// validator_fee u128 BE ‖ validator_slash u128 BE ‖ missed_slash u128 BE ‖
/// min_canary_ppm u64 BE ‖ challenge_window u64 BE ‖ min_challenge_bond u128 BE ‖
/// panel_size u32 BE ‖ panel_commit_window u64 BE ‖ panel_reveal_window u64 BE ‖
/// appeal_window u64 BE ‖ max_appeals u32 BE ‖ panel_fee u128 BE ‖ panel_slash u128 BE ‖
/// shard ids in ascending order, 32 bytes each)`. The domain is a dotted id, so it has
/// no 0x00 byte; every later field has a fixed width. Published with the round
/// so miners can recompute it before signing (the contract recomputes it in M6,
/// where the ledger id is the settlement contract). The ledger id binds the
/// round to the one ledger that opened it: the same round opened in another
/// ledger has another tag, so miners' signatures and its validation do not
/// carry over (L27-F1).
pub fn round_tag(
    domain: &str,
    ledger_id: &Hash32,
    id: u64,
    cfg: &RoundConfig,
    shard_ids: &[Hash32],
) -> Hash32 {
    let mut sorted = shard_ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut t = b"onyx-round-v4".to_vec();
    t.extend_from_slice(domain.as_bytes());
    t.push(0);
    t.extend_from_slice(&ledger_id.0);
    t.extend_from_slice(&id.to_be_bytes());
    t.extend_from_slice(&cfg.commit_close.to_be_bytes());
    t.extend_from_slice(&cfg.reveal_close.to_be_bytes());
    t.extend_from_slice(&cfg.min_bond.to_be_bytes());
    t.extend_from_slice(&cfg.validator_set_root.0);
    t.extend_from_slice(&cfg.canary_commit.0);
    t.extend_from_slice(&cfg.beacon_height.to_be_bytes());
    let v = &cfg.validation;
    t.extend_from_slice(&v.verdict_commit_close.to_be_bytes());
    t.extend_from_slice(&v.verdict_reveal_close.to_be_bytes());
    t.extend_from_slice(&v.canary_open_close.to_be_bytes());
    t.extend_from_slice(&v.pending_close.to_be_bytes());
    t.extend_from_slice(&v.rate_ppm.to_be_bytes());
    t.extend_from_slice(&v.miner_reward.to_be_bytes());
    t.extend_from_slice(&v.validator_fee.to_be_bytes());
    t.extend_from_slice(&v.validator_slash.to_be_bytes());
    t.extend_from_slice(&v.missed_slash.to_be_bytes());
    t.extend_from_slice(&v.min_canary_ppm.to_be_bytes());
    let d = &cfg.dispute;
    t.extend_from_slice(&d.challenge_window.to_be_bytes());
    t.extend_from_slice(&d.min_challenge_bond.to_be_bytes());
    t.extend_from_slice(&d.panel_size.to_be_bytes());
    t.extend_from_slice(&d.panel_commit_window.to_be_bytes());
    t.extend_from_slice(&d.panel_reveal_window.to_be_bytes());
    t.extend_from_slice(&d.appeal_window.to_be_bytes());
    t.extend_from_slice(&d.max_appeals.to_be_bytes());
    t.extend_from_slice(&d.panel_fee.to_be_bytes());
    t.extend_from_slice(&d.panel_slash.to_be_bytes());
    for k in sorted {
        t.extend_from_slice(&k.0);
    }
    keccak256(&t)
}

/// A commit as posted: who, for which shard, the hash, the bond, and the
/// miner's signature over [`commit_message`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedCommit {
    /// Miner address (EVM-style, from its key).
    pub miner: MinerAddress,
    /// Shard id.
    pub shard: Hash32,
    /// `onyx_core::commit_hash` of the hidden commitment.
    pub commit_hash: Hash32,
    /// Bond posted.
    pub bond: u128,
    /// EIP-191 signature `r ‖ s ‖ v` by `miner`.
    pub sig: [u8; 65],
}

pub use crate::RoundPhase;

/// Round timing, bond and validation rules, in chain heights. Everything
/// here is fixed when the round opens and bound into its tag (L24-F3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoundConfig {
    /// Commits are accepted while `now < commit_close`.
    pub commit_close: u64,
    /// Reveals are accepted while `commit_close <= now < reveal_close`.
    pub reveal_close: u64,
    /// Smallest bond per commit.
    pub min_bond: u128,
    /// Root of the bonded validator set that will validate this round,
    /// fixed when the round opens so it cannot be chosen after the beacon (L23-F2).
    pub validator_set_root: Hash32,
    /// Commitment to the coordinator's canary list (`validate::canary_commit`).
    pub canary_commit: Hash32,
    /// Height whose block hash is the sampling beacon (after `reveal_close`).
    pub beacon_height: u64,
    /// Validation windows, sampling rate and economics (L24-F3).
    pub validation: ValidationConfig,
    /// Challenge window, panel size, appeals and panel economics (M5,
    /// ADR-0021): fixed when the round opens, like the validation rules.
    pub dispute: DisputeConfig,
}

/// Round error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RoundError {
    /// Times not in order.
    #[error("round times must satisfy now < commit_close < reveal_close <= beacon_height")]
    BadTimes,
    /// Validation windows out of order, rate outside (0, 100%], or the
    /// sampling rate and slashes do not deter a lazy miner
    /// (`deterrence_holds`) or a lazy validator (L25-F8).
    #[error("bad validation config")]
    BadValidationConfig,
    /// Dispute config out of range: an even or empty panel, a zero window,
    /// bond or panel slash, a panel fee above the panel slash, or more than
    /// [`crate::dispute::MAX_APPEALS`] appeal rounds (ADR-0021).
    #[error("bad dispute config")]
    BadDisputeConfig,
    /// A round with this id was already opened in this ledger.
    #[error("round id already used")]
    RoundExists,
    /// The ledger passed is not the one that opened this round.
    #[error("round not opened in this ledger")]
    NotOpenedHere,
    /// A shard spec does not validate against the registry.
    #[error("shard spec invalid: {0}")]
    BadSpec(CommitmentError),
    /// The same shard twice.
    #[error("duplicate shard")]
    DuplicateShard,
    /// Something the shard pins is not in the CAS.
    #[error("not available in the CAS: {0}")]
    Unavailable(String),
    /// The canonical engine refuses the recipe (L25-F1): nobody could
    /// compute the shard, so it never opens.
    #[error("recipe refused by the canonical engine: {0}")]
    RecipeRejected(String),
    /// A shard failed its preflight run ([`Round::open_preflighted`]): over
    /// the fuel share a round may use, or not runnable (SER-R1-F2).
    #[error("shard {shard} failed preflight: {error}")]
    Preflight {
        /// The shard.
        shard: Hash32,
        /// Why.
        error: PreflightError,
    },
    /// Action not allowed in the current phase.
    #[error("wrong phase: {0:?}")]
    WrongPhase(RoundPhase),
    /// Shard not in this round.
    #[error("unknown shard")]
    UnknownShard,
    /// A second commit by the same miner for the same shard.
    #[error("miner already committed to this shard")]
    DuplicateCommit,
    /// Bond below the minimum.
    #[error("bond too low")]
    BondTooLow,
    /// Reveal without a commit.
    #[error("no commit to reveal")]
    NoCommit,
    /// Reveal already made.
    #[error("already revealed")]
    AlreadyRevealed,
    /// Reveal does not hash to the commit.
    #[error("reveal does not match the commit")]
    CommitMismatch,
    /// Commitment fields differ from the shard's pinned fields.
    #[error("commitment does not match the shard")]
    WrongShard,
    /// Commitment malformed.
    #[error("commitment invalid: {0}")]
    BadCommitment(CommitmentError),
    /// Row batch for the revealed root is not in the CAS, or has another count.
    #[error("rows for the revealed root are not available")]
    RowsUnavailable,
    /// The commit is not signed by the miner it names.
    #[error("commit signature is not the miner's")]
    BadSignature,
    /// A height earlier than one already seen.
    #[error("height went backwards")]
    TimeWentBack,
    /// The round has been closed.
    #[error("round closed")]
    Closed,
    /// The shard's work (by `ShardSpec::work_id`, registry root excluded) is
    /// already settled or open elsewhere; its rows may be public, so offering
    /// it would pay copiers (L19-F4, L20-F2).
    #[error("shard already settled")]
    AlreadySettled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CommitRecord {
    seq: u64,
    commit_hash: Hash32,
    bond: u128,
}

/// A round in progress.
pub struct Round {
    id: u64,
    tag: Hash32,
    cfg: RoundConfig,
    shards: BTreeMap<Hash32, ShardSpec>,
    commits: BTreeMap<(Hash32, MinerAddress), CommitRecord>,
    reveals: BTreeMap<(Hash32, MinerAddress), Commitment>,
    next_seq: u64,
    /// Highest height seen; earlier heights are refused.
    seen: u64,
    /// Outcome, once closed.
    closed: Option<BTreeMap<Hash32, ShardOutcome>>,
}

/// What a shard ended with after reveals closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShardOutcome {
    /// `(commit order, miner, root)` for every valid reveal, in commit order.
    pub reveals: Vec<(u64, MinerAddress, Hash32)>,
    /// Miners who committed but did not reveal: their bonds are forfeited.
    pub forfeited: Vec<(MinerAddress, u128)>,
    /// Revealed roots with the miners behind each.
    pub roots: BTreeMap<Hash32, Vec<MinerAddress>>,
    /// Bond behind each valid reveal (slashed if the root proves wrong, M4).
    pub bonds: BTreeMap<MinerAddress, u128>,
}

impl ShardOutcome {
    /// Earliest valid reveal by commit order (reward candidate once validated).
    pub fn first(&self) -> Option<&(u64, MinerAddress, Hash32)> {
        self.reveals.first()
    }
}

impl Round {
    /// Open a round at height `now` in `ledger`: the only public way to open
    /// one (SER-R2-F1). Checks the timing and validation rules, every spec and
    /// the availability of everything it pins, and that the canonical engine
    /// accepts every recipe (L25-F1); then runs every shard once on the
    /// canonical engine (SER-R1-F2) and refuses one whose run uses more than
    /// [`crate::recompute::PREFLIGHT_MAX_FUEL_PPM`] of its fuel limit, runs out
    /// of fuel, cannot be run from `env`'s inputs, or ends with a failure that
    /// `allowed` does not list for its schema (SER-R2-F2). On success the
    /// ledger records the round (id, tag and its known miners for the canary
    /// check, L24-F2, L25-F4) and marks its work open so no other round offers
    /// it (L24-F5). A settlement can later be applied only to the ledger that
    /// opened its round. Size shards by input and output count
    /// (docs/spec/recipes.md).
    pub fn open_preflighted<R: RuleSet>(
        id: u64,
        specs: Vec<ShardSpec>,
        env: &RecomputeEnv<'_>,
        cfg: RoundConfig,
        now: u64,
        ledger: &mut Ledger<R>,
        allowed: &FailureAllowList,
    ) -> Result<Round, RoundError> {
        let cache = onyx_labels::attest::AttestCache::new();
        Round::open_inner(id, specs, env.reg, env.cas, cfg, now, ledger, &mut |spec| {
            env.preflight(spec, &cache, allowed)
                .map(|_| ())
                .map_err(|error| RoundError::Preflight {
                    shard: spec.shard_id(),
                    error,
                })
        })
    }

    /// Settlement-path tests only: open a round with no preflight, as a
    /// coordinator could on chain (`openRound` trusts the coordinator,
    /// SER-R2-F1 TODO). A round that skips the preflight could offer a shard
    /// whose honest result is an out-of-fuel or malformed-input failure leaf.
    /// Compiled only with the `unchecked-open` feature, which no shipped crate
    /// enables (scripts/check-boundaries.sh).
    #[cfg(feature = "unchecked-open")]
    #[doc(hidden)]
    pub fn open_unchecked<R: RuleSet>(
        id: u64,
        specs: Vec<ShardSpec>,
        reg: &dyn Registry,
        cas: &dyn Cas,
        cfg: RoundConfig,
        now: u64,
        ledger: &mut Ledger<R>,
    ) -> Result<Round, RoundError> {
        Round::open_inner(id, specs, reg, cas, cfg, now, ledger, &mut |_| Ok(()))
    }

    #[allow(clippy::too_many_arguments)]
    fn open_inner<R: RuleSet>(
        id: u64,
        specs: Vec<ShardSpec>,
        reg: &dyn Registry,
        cas: &dyn Cas,
        cfg: RoundConfig,
        now: u64,
        ledger: &mut Ledger<R>,
        preflight: &mut dyn FnMut(&ShardSpec) -> Result<(), RoundError>,
    ) -> Result<Round, RoundError> {
        if !(now < cfg.commit_close
            && cfg.commit_close < cfg.reveal_close
            && cfg.reveal_close <= cfg.beacon_height)
        {
            return Err(RoundError::BadTimes);
        }
        let v = &cfg.validation;
        if !(cfg.beacon_height < v.verdict_commit_close
            // The seed can be drawn at MIN_DRAW_SLACK heights, each leaving a
            // full verdict window, as on the contract (M6-L44-F3).
            && cfg
                .beacon_height
                .saturating_add(MIN_VERDICT_WINDOW + MIN_DRAW_SLACK)
                <= v.verdict_commit_close
            && v.verdict_commit_close < v.verdict_reveal_close
            && v.verdict_reveal_close <= v.canary_open_close
            // A voided shard's reveals are public; leave time after the
            // canary list opens before anything can be voided (L26-F2).
            && v.canary_open_close.saturating_add(MIN_PENDING_GAP) <= v.pending_close
            && v.rate_ppm > 0
            && v.rate_ppm <= PPM
            && deterrence_holds(cfg.min_bond, v.miner_reward, v.rate_ppm)
            && v.validators_deterred())
        {
            return Err(RoundError::BadValidationConfig);
        }
        if !cfg.dispute.is_valid() {
            return Err(RoundError::BadDisputeConfig);
        }
        if ledger.has_round(id) {
            return Err(RoundError::RoundExists);
        }
        // No-valid-root work whose challenge window has closed may be
        // offered again (L30-F4).
        ledger.release_held(now);
        let taken = ledger.taken();
        let mut shards = BTreeMap::new();
        let mut works = BTreeSet::new();
        for spec in specs {
            spec.validate(reg).map_err(RoundError::BadSpec)?;
            // The same work twice in one round (e.g. pinned to two registry
            // roots) would pay a copier too.
            if taken.contains(&spec.work_id()) || !works.insert(spec.work_id()) {
                return Err(RoundError::AlreadySettled);
            }
            check_available(&spec, reg, cas)?;
            preflight(&spec)?;
            if shards.insert(spec.shard_id(), spec).is_some() {
                return Err(RoundError::DuplicateShard);
            }
        }
        let shard_ids: Vec<Hash32> = shards.keys().copied().collect();
        let tag = round_tag(reg.domain(), &ledger.id(), id, &cfg, &shard_ids);
        ledger.record_open(id, tag, works);
        Ok(Round {
            id,
            tag,
            cfg,
            shards,
            commits: BTreeMap::new(),
            reveals: BTreeMap::new(),
            next_seq: 0,
            seen: now,
            closed: None,
        })
    }

    /// Round number.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Tag miners sign in [`commit_message`]: domain, id, timing, bond and shards.
    pub fn tag(&self) -> Hash32 {
        self.tag
    }

    /// The round's configuration (validator set root, canary commitment, beacon height).
    pub fn config(&self) -> RoundConfig {
        self.cfg
    }

    /// The closed outcome, once [`Round::close`] has run.
    pub fn outcome(&self) -> Option<&BTreeMap<Hash32, ShardOutcome>> {
        self.closed.as_ref()
    }

    /// Shard id -> work id for every shard in the round.
    pub fn work_of(&self) -> BTreeMap<Hash32, Hash32> {
        self.shards.iter().map(|(k, s)| (*k, s.work_id())).collect()
    }

    /// Every miner address that committed in this round.
    pub fn committed_miners(&self) -> BTreeSet<MinerAddress> {
        self.commits.keys().map(|(_, m)| *m).collect()
    }

    /// Shard ids in this round.
    pub fn shard_ids(&self) -> Vec<Hash32> {
        self.shards.keys().copied().collect()
    }

    /// The shard specs, by shard id (what settlement reruns).
    pub fn specs(&self) -> &BTreeMap<Hash32, ShardSpec> {
        &self.shards
    }

    /// Phase at height `now`.
    pub fn phase(&self, now: u64) -> RoundPhase {
        if now < self.cfg.commit_close {
            RoundPhase::Commit
        } else if now < self.cfg.reveal_close {
            RoundPhase::Reveal
        } else {
            RoundPhase::Validate
        }
    }

    /// Refuse a closed round or a height earlier than one already accepted.
    /// The height is recorded only when the call succeeds, so a rejected call
    /// cannot move time forward (L19-F2).
    fn check_time(&self, now: u64) -> Result<(), RoundError> {
        if self.closed.is_some() {
            return Err(RoundError::Closed);
        }
        if now < self.seen {
            return Err(RoundError::TimeWentBack);
        }
        Ok(())
    }

    /// Post a signed, bonded commit. Returns its position in commit order.
    /// `ledger` must be the one that opened the round; the miner is recorded
    /// there as active in this round at once, so it is never fresh enough to
    /// be another round's canary, whatever order rounds are validated in
    /// (L26-F1).
    pub fn commit<R: RuleSet>(
        &mut self,
        now: u64,
        c: &SignedCommit,
        ledger: &mut Ledger<R>,
    ) -> Result<u64, RoundError> {
        self.check_time(now)?;
        if ledger.round_tag(self.id) != Some(self.tag) {
            return Err(RoundError::NotOpenedHere);
        }
        let SignedCommit {
            miner,
            shard,
            commit_hash,
            bond,
            sig,
        } = c.clone();
        let msg = commit_message(&self.tag, &shard, &commit_hash, bond);
        if eip191_recover(msg.as_bytes(), &sig) != Some(miner) {
            return Err(RoundError::BadSignature);
        }
        let phase = self.phase(now);
        if phase != RoundPhase::Commit {
            return Err(RoundError::WrongPhase(phase));
        }
        if !self.shards.contains_key(&shard) {
            return Err(RoundError::UnknownShard);
        }
        if bond < self.cfg.min_bond {
            return Err(RoundError::BondTooLow);
        }
        if self.commits.contains_key(&(shard, miner)) {
            return Err(RoundError::DuplicateCommit);
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.commits.insert(
            (shard, miner),
            CommitRecord {
                seq,
                commit_hash,
                bond,
            },
        );
        self.seen = now;
        ledger.record_commit(self.id, miner);
        Ok(seq)
    }

    /// Reveal a commitment and its salt.
    pub fn reveal(
        &mut self,
        now: u64,
        miner: MinerAddress,
        shard: Hash32,
        commitment: Commitment,
        salt: Hash32,
        cas: &dyn Cas,
    ) -> Result<(), RoundError> {
        self.check_time(now)?;
        let phase = self.phase(now);
        if phase != RoundPhase::Reveal {
            return Err(RoundError::WrongPhase(phase));
        }
        let spec = self.shards.get(&shard).ok_or(RoundError::UnknownShard)?;
        let rec = self
            .commits
            .get(&(shard, miner))
            .ok_or(RoundError::NoCommit)?;
        if self.reveals.contains_key(&(shard, miner)) {
            return Err(RoundError::AlreadyRevealed);
        }
        commitment.validate().map_err(RoundError::BadCommitment)?;
        if commit_hash(self.id, &shard, &commitment.binding_hash(), &miner, &salt)
            != rec.commit_hash
        {
            return Err(RoundError::CommitMismatch);
        }
        if !commitment.matches_shard(spec) {
            return Err(RoundError::WrongShard);
        }
        // Data availability (R-DA-1): the full row batch is in the CAS before reveal closes.
        match cas.rows(&commitment.root) {
            Some(rows) if rows.len() as u64 == commitment.leaf_count => {}
            _ => return Err(RoundError::RowsUnavailable),
        }
        self.reveals.insert((shard, miner), commitment);
        self.seen = now;
        Ok(())
    }

    /// Close the round once reveals are over and return the outcome per
    /// shard. Afterwards no commit or reveal is accepted, and calling `close`
    /// again returns the same outcome. Row availability is judged at reveal
    /// time (R-DA-1); validators check rows again in M4.
    pub fn close(&mut self, now: u64) -> Result<BTreeMap<Hash32, ShardOutcome>, RoundError> {
        if let Some(out) = &self.closed {
            return Ok(out.clone());
        }
        if now < self.seen {
            return Err(RoundError::TimeWentBack);
        }
        let phase = self.phase(now);
        if phase < RoundPhase::Validate {
            return Err(RoundError::WrongPhase(phase));
        }
        self.seen = now;
        let mut out: BTreeMap<Hash32, ShardOutcome> = self
            .shards
            .keys()
            .map(|k| {
                (
                    *k,
                    ShardOutcome {
                        reveals: Vec::new(),
                        forfeited: Vec::new(),
                        bonds: BTreeMap::new(),
                        roots: BTreeMap::new(),
                    },
                )
            })
            .collect();
        for ((shard, miner), rec) in &self.commits {
            let Some(o) = out.get_mut(shard) else {
                continue;
            };
            match self.reveals.get(&(*shard, *miner)) {
                Some(c) => {
                    o.reveals.push((rec.seq, *miner, c.root));
                    o.roots.entry(c.root).or_default().push(*miner);
                    o.bonds.insert(*miner, rec.bond);
                }
                None => o.forfeited.push((*miner, rec.bond)),
            }
        }
        for o in out.values_mut() {
            o.reveals.sort_unstable();
        }
        self.closed = Some(out.clone());
        Ok(out)
    }
}

/// Everything a shard pins must be fetchable by hash before it opens, and
/// usable (L18-F4): the stored label set is exactly the pinned one and valid
/// under the registry, its cited attestations fit the caps, and a document
/// snapshot's bytes are stored.
fn check_available(spec: &ShardSpec, reg: &dyn Registry, cas: &dyn Cas) -> Result<(), RoundError> {
    if let Some(recipe) = &spec.recipe {
        let wasm = cas
            .get(Kind::Recipe, recipe)
            .ok_or_else(|| RoundError::Unavailable(format!("recipe {recipe}")))?;
        // The same load-time checks and full canonical translation `run` does
        // (L25-F1). The canonical host has no configuration, so a fresh one
        // gives the same answer as any validator's.
        let profile = spec
            .runtime
            .as_ref()
            .ok_or_else(|| RoundError::RecipeRejected("no runtime profile".into()))?;
        Host::new()
            .and_then(|h| h.prepare(recipe, &wasm, profile))
            .map_err(|e| RoundError::RecipeRejected(e.to_string()))?;
    }
    if !cas.has(Kind::Blob, &keccak256(&spec.params)) {
        return Err(RoundError::Unavailable("params".into()));
    }
    if let onyx_core::Snapshot::Document { content_hash, .. } = &spec.source.snapshot {
        if !cas.has(Kind::Blob, content_hash) {
            return Err(RoundError::Unavailable("document bytes".into()));
        }
    }
    if let Some(pin) = &spec.label_set {
        let set = cas
            .label_set(&pin.content_hash)
            .ok_or_else(|| RoundError::Unavailable(format!("label set {}", pin.content_hash)))?;
        if set.name != pin.name || set.version != pin.version {
            return Err(RoundError::Unavailable(
                "label set name or version differs from the pin".into(),
            ));
        }
        set.validate(reg)
            .map_err(|e| RoundError::Unavailable(format!("label set invalid: {e}")))?;
        let cited: BTreeSet<Hash32> = set
            .labels
            .iter()
            .flat_map(|l| l.evidence.iter())
            .filter(|e| e.kind == ATTESTATION_EVIDENCE)
            .map(|e| e.content_hash)
            .collect();
        if cited.len() > MAX_ATTESTATIONS {
            return Err(RoundError::Unavailable(
                "too many cited attestations".into(),
            ));
        }
        let mut bytes = 0usize;
        for h in &cited {
            let a = cas
                .attestation(h)
                .ok_or_else(|| RoundError::Unavailable(format!("attestation {h}")))?;
            bytes = bytes
                .saturating_add(a.statement.len())
                .saturating_add(a.address_sig.len());
        }
        if bytes > MAX_ATTESTATION_BYTES {
            return Err(RoundError::Unavailable(
                "cited attestations over the byte cap".into(),
            ));
        }
    }
    Ok(())
}

/// The round state machine at each edge (ROADMAP E8, THREATS MUT-1): the
/// end-to-end tests in onyx-source-btc open rounds on real blocks, but no
/// test in this crate did, so every mutant of this file survived its own
/// crate's tests.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixture::{
        actors, recipe_float, recipe_loop, signed_commit, Actor, Rules, World, CFG,
    };

    fn ledger() -> Ledger<Rules> {
        Ledger::new()
    }

    /// A round opened at 10 with fixture shards `ns`.
    fn open(w: &World, ns: &[u8], ledger: &mut Ledger<Rules>) -> Round {
        let specs = ns.iter().map(|n| w.spec(*n)).collect();
        w.open(1, specs, CFG, 10, ledger).expect("open")
    }

    #[test]
    fn commit_message_is_fixed_text() {
        let (t, s, c) = (Hash32([1; 32]), Hash32([2; 32]), Hash32([3; 32]));
        assert_eq!(
            commit_message(&t, &s, &c, 1_000),
            format!("Onyx commit v1\nround: {t}\nshard: {s}\ncommit: {c}\nbond: 1000")
        );
    }

    /// The tag ignores shard order and duplicates and changes with every
    /// field it binds.
    #[test]
    fn round_tag_binds_its_fields_and_not_the_shard_order() {
        let ledger_id = Hash32([9; 32]);
        let (a, b) = (Hash32([1; 32]), Hash32([2; 32]));
        let tag = |d: &str, l: &Hash32, id: u64, cfg: &RoundConfig, s: &[Hash32]| {
            round_tag(d, l, id, cfg, s)
        };
        let base = tag("onyx.devnet", &ledger_id, 1, &CFG, &[a, b]);
        assert_ne!(base, Hash32::ZERO);
        assert_eq!(base, tag("onyx.devnet", &ledger_id, 1, &CFG, &[b, a, b]));
        assert_ne!(base, tag("onyx.other", &ledger_id, 1, &CFG, &[a, b]));
        assert_ne!(base, tag("onyx.devnet", &Hash32([8; 32]), 1, &CFG, &[a, b]));
        assert_ne!(base, tag("onyx.devnet", &ledger_id, 2, &CFG, &[a, b]));
        assert_ne!(base, tag("onyx.devnet", &ledger_id, 1, &CFG, &[a]));
        let mut changed = Vec::new();
        let edits: [&dyn Fn(&mut RoundConfig); 6] = [
            &|c| c.commit_close += 1,
            &|c| c.min_bond += 1,
            &|c| c.beacon_height += 1,
            &|c| c.validation.rate_ppm -= 1,
            &|c| c.validation.missed_slash += 1,
            &|c| c.dispute.panel_slash += 1,
        ];
        for e in edits {
            let mut c = CFG;
            e(&mut c);
            changed.push(tag("onyx.devnet", &ledger_id, 1, &c, &[a, b]));
        }
        assert!(changed.iter().all(|t| *t != base));
    }

    #[test]
    fn open_checks_the_times_at_each_edge() {
        let w = World::new();
        let open_at =
            |cfg: RoundConfig, now: u64| w.open(1, vec![w.spec(1)], cfg, now, &mut ledger()).err();
        assert_eq!(open_at(CFG, 99), None, "one height before commit_close");
        assert_eq!(open_at(CFG, 100), Some(RoundError::BadTimes));
        let c = RoundConfig {
            reveal_close: 100,
            ..CFG
        };
        assert_eq!(open_at(c, 10), Some(RoundError::BadTimes));
        // The beacon may be the reveal deadline itself, not before it.
        let mut c = CFG;
        c.beacon_height = 200;
        c.validation.verdict_commit_close = 230;
        assert_eq!(open_at(c, 10), None);
        c.beacon_height = 199;
        assert_eq!(open_at(c, 10), Some(RoundError::BadTimes));
    }

    #[test]
    fn open_checks_the_validation_rules_at_each_edge() {
        let w = World::new();
        let open_with = |f: &dyn Fn(&mut ValidationConfig)| {
            let mut c = CFG;
            f(&mut c.validation);
            w.open(1, vec![w.spec(1)], c, 10, &mut ledger()).err()
        };
        let bad = Some(RoundError::BadValidationConfig);
        assert_eq!(open_with(&|_| {}), None);
        // beacon 201 + MIN_VERDICT_WINDOW + MIN_DRAW_SLACK = 231.
        assert_eq!(open_with(&|v| v.verdict_commit_close = 231), None);
        assert_eq!(open_with(&|v| v.verdict_commit_close = 230), bad);
        assert_eq!(open_with(&|v| v.verdict_commit_close = 201), bad);
        assert_eq!(open_with(&|v| v.verdict_reveal_close = 300), bad);
        assert_eq!(open_with(&|v| v.canary_open_close = 400), None);
        assert_eq!(open_with(&|v| v.canary_open_close = 399), bad);
        // MIN_PENDING_GAP after the canary deadline, exactly.
        assert_eq!(open_with(&|v| v.pending_close = 550), None);
        assert_eq!(open_with(&|v| v.pending_close = 549), bad);
        assert_eq!(open_with(&|v| v.rate_ppm = 0), bad);
        // A zero rate is refused even when a zero reward makes it "deter".
        assert_eq!(
            open_with(&|v| {
                v.rate_ppm = 0;
                v.miner_reward = 0;
            }),
            bad
        );
        assert_eq!(open_with(&|v| v.rate_ppm = PPM + 1), bad);
        // Lazy-miner deterrence: 1,000 x rate >= 500 x 1e6 needs rate >= 50%.
        assert_eq!(open_with(&|v| v.rate_ppm = 500_000), None);
        assert_eq!(open_with(&|v| v.rate_ppm = 499_999), bad);
        assert_eq!(open_with(&|v| v.missed_slash = 0), bad);
        let c = RoundConfig {
            dispute: DisputeConfig {
                panel_size: 4,
                ..CFG.dispute
            },
            ..CFG
        };
        assert_eq!(
            w.open(1, vec![w.spec(1)], c, 10, &mut ledger()).err(),
            Some(RoundError::BadDisputeConfig)
        );
    }

    /// Open records the round and its work in the ledger; the same id, or
    /// work open or settled anywhere in it, is refused.
    #[test]
    fn open_records_the_round_and_refuses_reuse() {
        let w = World::new();
        let mut l = ledger();
        assert!(!l.has_round(1));
        let r = open(&w, &[1, 2], &mut l);
        assert!(l.has_round(1));
        assert_eq!(l.round_tag(1), Some(r.tag()));
        assert_eq!(r.id(), 1);
        assert_eq!(r.config(), CFG);
        assert_eq!(r.outcome(), None);
        let (s1, s2) = (w.spec(1), w.spec(2));
        let mut ids = vec![s1.shard_id(), s2.shard_id()];
        ids.sort_unstable();
        assert_eq!(r.shard_ids(), ids);
        assert_eq!(r.specs().len(), 2);
        assert_eq!(r.specs()[&s1.shard_id()], s1);
        assert_eq!(r.work_of()[&s2.shard_id()], s2.work_id());
        assert!(l.open_work().contains(&s1.work_id()) && l.open_work().contains(&s2.work_id()));
        assert_eq!(
            r.tag(),
            round_tag(w.reg.domain(), &l.id(), 1, &CFG, &r.shard_ids())
        );
        assert_eq!(
            w.open(1, vec![w.spec(3)], CFG, 10, &mut l).err(),
            Some(RoundError::RoundExists)
        );
        assert_eq!(
            w.open(2, vec![w.spec(2)], CFG, 10, &mut l).err(),
            Some(RoundError::AlreadySettled),
            "work open in round 1"
        );
        assert_eq!(
            w.open(3, vec![w.spec(4), w.spec(4)], CFG, 10, &mut l).err(),
            Some(RoundError::AlreadySettled),
            "the same work twice in one round"
        );
        assert!(w.open(4, vec![w.spec(5)], CFG, 10, &mut l).is_ok());
    }

    /// Everything a shard pins must be stored and usable before it opens.
    #[test]
    fn open_needs_everything_the_shard_pins() {
        let w = World::new();
        let err = |spec: ShardSpec| w.open(1, vec![spec], CFG, 10, &mut ledger()).err();
        let mut s = w.spec(1);
        s.registry_root = Hash32([7; 32]);
        assert!(matches!(err(s), Some(RoundError::BadSpec(_))));
        let mut s = w.spec(1);
        s.params = vec![1, 2, 3];
        assert_eq!(err(s), Some(RoundError::Unavailable("params".into())));
        let mut s = w.spec(1);
        s.recipe = Some(Hash32([5; 32]));
        assert!(matches!(err(s), Some(RoundError::Unavailable(m)) if m.starts_with("recipe")));
        assert!(matches!(
            err(w.spec_with(&recipe_float(), 1)),
            Some(RoundError::RecipeRejected(_))
        ));
        assert!(matches!(
            err(w.spec_with(&recipe_loop(), 1)),
            Some(RoundError::Preflight { .. })
        ));
        // A document snapshot's bytes must be stored.
        let mut s = w.spec(1);
        let doc = b"document bytes".to_vec();
        s.source.snapshot = onyx_core::Snapshot::Document {
            published_unix: 1,
            content_hash: keccak256(&doc),
        };
        assert_eq!(
            err(s.clone()),
            Some(RoundError::Unavailable("document bytes".into()))
        );
        w.cas.put(Kind::Blob, &doc).expect("doc");
        assert!(!matches!(err(s), Some(RoundError::Unavailable(_))));
    }

    #[test]
    fn phases_change_at_the_deadlines() {
        let w = World::new();
        let r = open(&w, &[1], &mut ledger());
        assert_eq!(r.phase(99), RoundPhase::Commit);
        assert_eq!(r.phase(100), RoundPhase::Reveal);
        assert_eq!(r.phase(199), RoundPhase::Reveal);
        assert_eq!(r.phase(200), RoundPhase::Validate);
    }

    #[test]
    fn commit_rules_at_each_edge() {
        let w = World::new();
        let mut l = ledger();
        let mut r = open(&w, &[1], &mut l);
        let spec = w.spec(1);
        let shard = spec.shard_id();
        let m = actors("m", 3);
        let wk = w.honest(&spec, 1, &m[0]);
        let c = signed_commit(&r, &shard, &m[0], &wk, 1_000);
        // Another ledger did not open this round.
        let mut other = ledger();
        assert_eq!(r.commit(20, &c, &mut other), Err(RoundError::NotOpenedHere));
        // The signature must be the named miner's.
        let mut forged = c.clone();
        forged.miner = m[1].addr;
        assert_eq!(r.commit(20, &forged, &mut l), Err(RoundError::BadSignature));
        let mut bumped = c.clone();
        bumped.bond = 2_000;
        assert_eq!(r.commit(20, &bumped, &mut l), Err(RoundError::BadSignature));
        // Unknown shard, low bond.
        let stray = signed_commit(&r, &Hash32([4; 32]), &m[0], &wk, 1_000);
        assert_eq!(r.commit(20, &stray, &mut l), Err(RoundError::UnknownShard));
        let low = signed_commit(&r, &shard, &m[0], &wk, 999);
        assert_eq!(r.commit(20, &low, &mut l), Err(RoundError::BondTooLow));
        // A rejected call does not move time: 20 is still accepted.
        assert_eq!(r.commit(20, &c, &mut l), Ok(0));
        assert_eq!(r.commit(21, &c, &mut l), Err(RoundError::DuplicateCommit));
        let c1 = signed_commit(&r, &shard, &m[1], &w.honest(&spec, 1, &m[1]), 1_000);
        assert_eq!(r.commit(19, &c1, &mut l), Err(RoundError::TimeWentBack));
        assert_eq!(r.commit(99, &c1, &mut l), Ok(1));
        let c2 = signed_commit(&r, &shard, &m[2], &w.honest(&spec, 1, &m[2]), 1_000);
        assert_eq!(
            r.commit(100, &c2, &mut l),
            Err(RoundError::WrongPhase(RoundPhase::Reveal))
        );
        assert_eq!(
            r.committed_miners(),
            [m[0].addr, m[1].addr].into_iter().collect()
        );
        // Each committer is active in round 1 at once (L26-F1).
        assert!(l.active_elsewhere(2).contains(&m[0].addr));
        assert!(!l.active_elsewhere(1).contains(&m[0].addr));
        r.close(200).expect("close");
        assert_eq!(r.commit(200, &c2, &mut l), Err(RoundError::Closed));
    }

    #[test]
    fn reveal_rules_at_each_edge() {
        let w = World::new();
        let mut l = ledger();
        let mut r = open(&w, &[1, 2], &mut l);
        let (spec, spec2) = (w.spec(1), w.spec(2));
        let shard = spec.shard_id();
        let m = actors("m", 3);
        let wk = w.honest(&spec, 1, &m[0]);
        r.commit(20, &signed_commit(&r, &shard, &m[0], &wk, 1_000), &mut l)
            .expect("commit");
        let reveal = |r: &mut Round, now: u64, who: &Actor, c: Commitment, salt: Hash32| {
            r.reveal(now, who.addr, shard, c, salt, &w.cas)
        };
        let c = wk.commitment.clone();
        assert_eq!(
            reveal(&mut r, 99, &m[0], c.clone(), wk.salt),
            Err(RoundError::WrongPhase(RoundPhase::Commit))
        );
        assert_eq!(
            r.reveal(100, m[0].addr, Hash32([4; 32]), c.clone(), wk.salt, &w.cas),
            Err(RoundError::UnknownShard)
        );
        assert_eq!(
            reveal(&mut r, 100, &m[1], c.clone(), wk.salt),
            Err(RoundError::NoCommit)
        );
        let mut empty = c.clone();
        empty.leaf_count = 0;
        assert!(matches!(
            reveal(&mut r, 100, &m[0], empty, wk.salt),
            Err(RoundError::BadCommitment(_))
        ));
        assert_eq!(
            reveal(&mut r, 100, &m[0], c.clone(), Hash32([1; 32])),
            Err(RoundError::CommitMismatch)
        );
        // Rows not in the store, or another row count, are not available.
        let mut missing = c.clone();
        missing.root = keccak256(b"rows nobody stored");
        let wk_missing = crate::test_fixture::Work {
            commit_hash: commit_hash(1, &shard, &missing.binding_hash(), &m[1].addr, &wk.salt),
            commitment: missing,
            salt: wk.salt,
        };
        r.commit(
            21,
            &signed_commit(&r, &shard, &m[1], &wk_missing, 1_000),
            &mut l,
        )
        .expect("commit");
        assert_eq!(
            reveal(&mut r, 100, &m[1], wk_missing.commitment.clone(), wk.salt),
            Err(RoundError::RowsUnavailable)
        );
        // Rows stored under the root, but fewer than the commitment claims.
        let mut short = c.clone();
        short.leaf_count = 2;
        let m3 = Actor::new("m3");
        let wk_short = crate::test_fixture::Work {
            commit_hash: commit_hash(1, &shard, &short.binding_hash(), &m3.addr, &wk.salt),
            commitment: short,
            salt: wk.salt,
        };
        r.commit(
            21,
            &signed_commit(&r, &shard, &m3, &wk_short, 1_000),
            &mut l,
        )
        .expect("commit");
        assert_eq!(
            r.reveal(
                100,
                m3.addr,
                shard,
                wk_short.commitment.clone(),
                wk.salt,
                &w.cas
            ),
            Err(RoundError::RowsUnavailable)
        );
        // A commitment under another schema, committed to shard 1: the
        // wrong shard. (Shards that differ only in params share fields.)
        let mut alien = spec.clone();
        alien.schema = keccak256(b"another schema");
        let other = w.work(&alien, 1, &m[2], &w.honest_rows(&spec));
        assert!(w.honest(&spec2, 1, &m[2]).commitment.matches_shard(&spec));
        let cross = commit_hash(
            1,
            &shard,
            &other.commitment.binding_hash(),
            &m[2].addr,
            &other.salt,
        );
        let wk_cross = crate::test_fixture::Work {
            commit_hash: cross,
            ..other.clone()
        };
        r.commit(
            22,
            &signed_commit(&r, &shard, &m[2], &wk_cross, 1_000),
            &mut l,
        )
        .expect("commit");
        assert_eq!(
            reveal(&mut r, 100, &m[2], other.commitment, other.salt),
            Err(RoundError::WrongShard)
        );
        assert_eq!(reveal(&mut r, 150, &m[0], c.clone(), wk.salt), Ok(()));
        assert_eq!(
            reveal(&mut r, 150, &m[0], c.clone(), wk.salt),
            Err(RoundError::AlreadyRevealed)
        );
        assert_eq!(
            reveal(&mut r, 149, &m[1], c.clone(), wk.salt),
            Err(RoundError::TimeWentBack)
        );
        assert_eq!(
            reveal(&mut r, 200, &m[1], c, wk.salt),
            Err(RoundError::WrongPhase(RoundPhase::Validate))
        );
    }

    /// Close: reveals in commit order per shard, silent committers forfeit
    /// their bond, and the outcome is fixed.
    #[test]
    fn close_orders_reveals_by_commit_and_forfeits_the_silent() {
        let w = World::new();
        let mut l = ledger();
        let mut r = open(&w, &[1, 2], &mut l);
        let spec = w.spec(1);
        let shard = spec.shard_id();
        let m = actors("m", 4);
        let works = [
            w.wrong(&spec, 1, &m[0], "first"),
            w.honest(&spec, 1, &m[1]),
            w.honest(&spec, 1, &m[2]),
            w.honest(&spec, 1, &m[3]),
        ];
        for (i, (a, wk)) in m.iter().zip(&works).enumerate() {
            let c = signed_commit(&r, &shard, a, wk, 1_000 + i as u128);
            assert_eq!(r.commit(20 + i as u64, &c, &mut l), Ok(i as u64));
        }
        // Reveal out of commit order; m[3] never reveals.
        for i in [2, 0, 1] {
            r.reveal(
                150,
                m[i].addr,
                shard,
                works[i].commitment.clone(),
                works[i].salt,
                &w.cas,
            )
            .expect("reveal");
        }
        assert_eq!(
            r.close(199),
            Err(RoundError::WrongPhase(RoundPhase::Reveal))
        );
        assert_eq!(r.close(149), Err(RoundError::TimeWentBack));
        // At the last reveal's own time the phase check answers first (E8 review: kills `now < seen` -> `<=`).
        assert_eq!(
            r.close(150),
            Err(RoundError::WrongPhase(RoundPhase::Reveal))
        );
        let out = r.close(200).expect("close");
        let o = &out[&shard];
        let good = w.honest_root(&spec);
        let bad = works[0].commitment.root;
        assert_eq!(
            o.reveals,
            vec![
                (0, m[0].addr, bad),
                (1, m[1].addr, good),
                (2, m[2].addr, good)
            ]
        );
        assert_eq!(o.first(), Some(&(0, m[0].addr, bad)));
        assert_eq!(o.forfeited, vec![(m[3].addr, 1_003)]);
        assert_eq!(o.roots[&good], {
            let mut v = vec![m[1].addr, m[2].addr];
            v.sort_unstable();
            v
        });
        assert_eq!(o.roots[&bad], vec![m[0].addr]);
        assert_eq!(
            o.bonds,
            [(m[0].addr, 1_000), (m[1].addr, 1_001), (m[2].addr, 1_002)]
                .into_iter()
                .collect()
        );
        // The other shard had no commit: an empty outcome.
        let empty = &out[&w.spec(2).shard_id()];
        assert!(empty.reveals.is_empty() && empty.forfeited.is_empty());
        assert_eq!(empty.first(), None);
        // Closed for good, the same outcome every time.
        assert_eq!(r.outcome(), Some(&out));
        assert_eq!(r.close(10), Ok(out.clone()));
        assert_eq!(
            r.reveal(
                300,
                m[3].addr,
                shard,
                works[3].commitment.clone(),
                works[3].salt,
                &w.cas
            ),
            Err(RoundError::Closed)
        );
    }

    /// A pinned label set must be stored under its hash with the pinned name
    /// and version.
    #[test]
    fn open_needs_the_pinned_label_set_itself() {
        let w = World::new();
        let set = onyx_labels::LabelSet {
            name: "onyx.test.labels".into(),
            version: 2,
            labels: vec![],
        };
        let pin = |name: &str, version: u32| onyx_core::LabelSetRef {
            name: name.into(),
            version,
            content_hash: set.content_hash(),
        };
        let open = |p: onyx_core::LabelSetRef| {
            let mut s = w.spec(1);
            s.label_set = Some(p);
            w.open(1, vec![s], CFG, 10, &mut ledger()).err()
        };
        assert!(matches!(
            open(pin("onyx.test.labels", 2)),
            Some(RoundError::Unavailable(m)) if m.starts_with("label set 0x")
        ));
        w.cas.put_label_set(&set).expect("store");
        let differs = Some(RoundError::Unavailable(
            "label set name or version differs from the pin".into(),
        ));
        assert_eq!(open(pin("onyx.test.other", 2)), differs);
        assert_eq!(open(pin("onyx.test.labels", 3)), differs);
        assert_eq!(open(pin("onyx.test.labels", 2)), None);
    }
}
