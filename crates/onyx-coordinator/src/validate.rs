//! Validation of a closed round (ADR-0019, rewritten loop 23 after the L22 review).
//!
//! 1. Validators are a bonded registry ([`ValidatorSet`]), snapshotted before
//!    the beacon; its root is part of the seed. Only members are assigned, and
//!    never to their own reveal (L22-F1).
//! 2. Seed (computed here, not passed in, L22-F8): `keccak256("onyx-seed-evm-v1"
//!    ‖ round_tag ‖ beacon)`, the settlement contract's form (ADR-0022 item 5,
//!    M6-G4). The tag already binds the round id, the validator set root and
//!    the shard ids; the reveals are fixed when the round closes, before the
//!    beacon, the block hash at `beacon_height`.
//! 3. Assignment: member `v` checks reveal `(shard, miner)` iff the first 8
//!    bytes of `keccak256(seed ‖ v ‖ shard ‖ miner)` fall below the rate.
//! 4. Validators recompute and commit, then reveal, a signed verdict bound to
//!    the seed (so it cannot be replayed into another validation, L22-F13).
//! 5. Truth is the reference root of a shard: if every reveal of a shard has
//!    the same root, no verdict disagrees and no canary is in it, that root;
//!    otherwise `settle` reruns the shard itself from the specs the round
//!    pinned ([`crate::recompute`]). No caller supplies a reference (L24-F1);
//!    a caller can only withhold inputs, which leaves the shard pending.
//! 6. Canaries are reveals the coordinator made (committed before the round
//!    as `(shard, miner, root)`, strictly sorted, fresh addresses): they earn
//!    nothing and are never finalized, but validators are judged on them
//!    against the reference like on any reveal (L22-F2, F3).
//! 7. Settlement per shard with a reference: the first correct non-canary
//!    reveal (commit order) earns the reward; wrong reveals lose their bond;
//!    verdicts matching the reference earn a fee once per (validator, shard,
//!    root); wrong verdicts are slashed; assigned validators who stay silent
//!    are slashed a smaller amount (L22-F6, F7, F11).
//! 8. Loop 25 (L24): the validation rules are fixed in the round config and
//!    tag; one validation per round, recorded in the ledger that opened the
//!    round; canaries must be opened before `canary_open_close` or are
//!    dropped; canary freshness uses the known miners copied at round open;
//!    settlements are sealed and apply only to the ledger that opened their
//!    round, with the same tag and seed.
//! 9. Loop 26 (L25): a "no valid root" verdict is a dispute; reruns use only
//!    the binary's built-in address rules ([`RuleSet`]); the known-miner
//!    snapshot lives in the ledger; shards still undecided at `pending_close`
//!    are voided; an unrevealed verdict is not also a missed task.
//! 10. Loop 27 (L26): a miner is recorded as active the moment its commit is
//!     accepted, so it is never fresh enough to be a canary in another round,
//!     whatever order rounds are validated in; the ledger re-checks canaries
//!     when it applies a settlement. Voided work whose reveals are public is
//!     never offered again, and voiding starts at least [`MIN_PENDING_GAP`]
//!     after the canary deadline. The ledger is bound to one rule set
//!     (`Ledger<R>`). Validator deterrence is computed at the declared canary
//!     density (`min_canary_ppm`).
//! 11. Loop 28 (L27, q12 a): each ledger has a random id that is in every
//!     round tag, so a round is bound to the ledger that opened it; `apply`
//!     checks canaries against the snapshot taken when validation started
//!     (no later veto); canary-only voided shards release their work; a rerun
//!     root with no correct reveal is finalized unpaid. Call order across
//!     rounds, the beacon and a coordinator bond are left to the M6 contract.

use crate::dispute::{Dispute, DisputeConfig, ShardRecord};
use crate::recompute::{no_valid_root_marker, RecomputeEnv, Reference};
use crate::round::{Round, ShardOutcome};
use onyx_core::{keccak256, Hash32, MinerAddress, RuleSet, ShardSpec};
use onyx_labels::attest::{eip191_recover, AttestCache};
use std::any::TypeId;
use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;

/// Parts per million.
pub const PPM: u64 = 1_000_000;

/// Smallest gap, in heights, between the canary opening deadline and the
/// voiding deadline (L26-F2): the canary list must be public well before any
/// shard can be voided.
pub const MIN_PENDING_GAP: u64 = 100;

/// A seed must leave at least this many heights for verdict commits; drawn
/// later, the round voids instead. Same value as `OnyxSettlement.MIN_VERDICT_WINDOW`
/// (M6-L37-F2, M6-L44-F3).
pub const MIN_VERDICT_WINDOW: u64 = 20;

/// The seed can be drawn at no fewer than this many heights: a round must have
/// `beacon_height + MIN_VERDICT_WINDOW + MIN_DRAW_SLACK <= verdict_commit_close`.
/// Same value as `OnyxSettlement.MIN_DRAW_SLACK` (M6-L44-F3).
pub const MIN_DRAW_SLACK: u64 = 10;

/// The chain keeps block hashes for this many blocks only (`blockhash`).
pub const BEACON_WINDOW: u64 = 256;

/// True if drawing the seed at height `at` voids the round instead, exactly as
/// `OnyxSettlement.drawSeed` decides: the beacon block is no longer readable, or
/// fewer than [`MIN_VERDICT_WINDOW`] heights would remain for verdict commits.
/// Rust and the contract agree on void through this rule (M6-L44-F3, vector
/// `seedDraw` in fixtures/settlement/vectors.json).
pub fn seed_draw_voids(beacon_height: u64, verdict_commit_close: u64, at: u64) -> bool {
    at.saturating_sub(beacon_height) > BEACON_WINDOW
        || at.saturating_add(MIN_VERDICT_WINDOW) > verdict_commit_close
}

/// Bonded validators, snapshotted before the round's beacon.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValidatorSet {
    bonds: BTreeMap<MinerAddress, u128>,
}

impl ValidatorSet {
    /// Empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a validator with a bond (must be at least `min_bond`).
    pub fn add(
        &mut self,
        v: MinerAddress,
        bond: u128,
        min_bond: u128,
    ) -> Result<(), ValidationError> {
        if bond < min_bond || self.bonds.contains_key(&v) {
            return Err(ValidationError::BadValidator);
        }
        self.bonds.insert(v, bond);
        Ok(())
    }

    /// True if `v` is a member.
    pub fn contains(&self, v: &MinerAddress) -> bool {
        self.bonds.contains_key(v)
    }

    /// Bond of `v`.
    pub fn bond(&self, v: &MinerAddress) -> u128 {
        self.bonds.get(v).copied().unwrap_or(0)
    }

    /// Members in order.
    pub fn members(&self) -> impl Iterator<Item = &MinerAddress> {
        self.bonds.keys()
    }

    /// Root of the set: `keccak256("onyx-validators-v1" ‖ (address ‖ bond u128 BE)*)`.
    pub fn root(&self) -> Hash32 {
        let mut b = b"onyx-validators-v1".to_vec();
        for (v, bond) in &self.bonds {
            b.extend_from_slice(v);
            b.extend_from_slice(&bond.to_be_bytes());
        }
        keccak256(&b)
    }
}

/// The sampling seed for a closed round: `keccak256("onyx-seed-evm-v1" ‖
/// round_tag ‖ beacon)`, byte for byte the contract's `OnyxRound.seed`
/// (ADR-0022 item 5, M6-G4). The tag binds the round id, the validator set
/// root (checked against the members in [`Validation::new`]) and the shard
/// ids; the reveals are fixed before the beacon block, so a validator that
/// runs this code checks exactly the tasks the contract judges.
pub fn sample_seed(round_tag: &Hash32, beacon: &Hash32) -> Hash32 {
    let mut b = b"onyx-seed-evm-v1".to_vec();
    b.extend_from_slice(&round_tag.0);
    b.extend_from_slice(&beacon.0);
    keccak256(&b)
}

/// True if `validator` must check the reveal of `miner` on `shard`, at a
/// sampling rate of `rate_ppm` (0..=1,000,000). Never for its own reveal.
pub fn assigned(
    seed: &Hash32,
    validator: &MinerAddress,
    shard: &Hash32,
    miner: &MinerAddress,
    rate_ppm: u64,
) -> bool {
    if validator == miner {
        return false;
    }
    let mut b = seed.0.to_vec();
    b.extend_from_slice(validator);
    b.extend_from_slice(&shard.0);
    b.extend_from_slice(miner);
    let h = keccak256(&b);
    let mut first = [0u8; 8];
    first.copy_from_slice(&h.0[..8]);
    let x = u128::from(u64::from_be_bytes(first));
    // x / 2^64 < rate / 1e6, exactly, in integers (cannot overflow u128).
    x * u128::from(PPM) < u128::from(rate_ppm.min(PPM)) << 64
}

/// Deterrence condition from validation.md: with sampling rate `p`, cheating
/// pays only if `slash * p < reward`. True if the parameters deter it; false
/// if the products overflow (L22-F12).
pub fn deterrence_holds(slash: u128, reward: u128, rate_ppm: u64) -> bool {
    match (
        slash.checked_mul(u128::from(rate_ppm)),
        reward.checked_mul(u128::from(PPM)),
    ) {
        (Some(a), Some(b)) => a >= b,
        _ => false,
    }
}

/// A wrong root's bond covers one extra rerun fee per member: `min_bond >=
/// validator_fee x members` (review L2-R2, ADR-0022 Amendment 1). Otherwise
/// a colluding miner revealing a wrong root lets right voters draw extra fees
/// beyond its lost bond, the rest owed and paid by the reward pool. The
/// contract refuses such a round in `openRound`; Rust in [`Validation::new`],
/// the first point the round's validator set (bound by its root) is known.
/// False if the product overflows.
pub fn fees_covered_by_bond(min_bond: u128, validator_fee: u128, members: usize) -> bool {
    u128::try_from(members)
        .ok()
        .and_then(|n| validator_fee.checked_mul(n))
        .is_some_and(|fees| min_bond >= fees)
}

/// A validator's verdict on one reveal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, borsh::BorshSerialize)]
pub enum Verdict {
    /// Recomputed the same root.
    Agree,
    /// Recomputed a different root (given; must differ from the revealed one).
    Disagree {
        /// The validator's root.
        root: Hash32,
    },
    /// Could not compute (missing data, machine limits). Earns nothing.
    Abstain,
    /// Recomputed and found that no root can be correct: the canonical
    /// engine cannot load the recipe, or its rows cannot be stored. A
    /// dispute: the shard is rerun, and the verdict is right exactly when the
    /// rerun says the same (L25-F1).
    NoValidRoot,
}

/// Hash a validator commits to before revealing its verdict.
pub fn verdict_commit_hash(
    seed: &Hash32,
    validator: &MinerAddress,
    shard: &Hash32,
    miner: &MinerAddress,
    verdict: &Verdict,
    salt: &Hash32,
) -> Hash32 {
    let mut b = b"onyx-verdict-v2".to_vec();
    b.extend_from_slice(&seed.0);
    b.extend_from_slice(validator);
    b.extend_from_slice(&shard.0);
    b.extend_from_slice(miner);
    b.extend_from_slice(&borsh::to_vec(verdict).unwrap_or_default());
    b.extend_from_slice(&salt.0);
    keccak256(&b)
}

/// Text a validator signs (EIP-191) to post a verdict commit. Bound to the
/// validation's seed, which covers round, round tag, validator set and beacon.
pub fn verdict_message(
    seed: &Hash32,
    shard: &Hash32,
    miner: &MinerAddress,
    commit: &Hash32,
) -> String {
    format!(
        "Onyx verdict v2\nseed: {seed}\nshard: {shard}\nminer: 0x{}\ncommit: {commit}",
        hex::encode(miner)
    )
}

/// A signed verdict commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedVerdict {
    /// Validator address.
    pub validator: MinerAddress,
    /// Shard.
    pub shard: Hash32,
    /// Miner whose reveal is judged.
    pub miner: MinerAddress,
    /// [`verdict_commit_hash`].
    pub commit: Hash32,
    /// EIP-191 signature by `validator` over [`verdict_message`].
    pub sig: [u8; 65],
}

/// One canary: a reveal the coordinator made itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, borsh::BorshSerialize)]
pub struct Canary {
    /// Shard.
    pub shard: Hash32,
    /// Canary miner address (fresh: no history in the ledger).
    pub miner: MinerAddress,
    /// The root the canary reveals.
    pub root: Hash32,
}

/// Commitment to a canary list, posted before the round opens. `None` unless
/// the list is strictly ascending with one entry per (shard, miner) (L22-F3).
pub fn canary_commit(canaries: &[Canary], salt: &Hash32) -> Option<Hash32> {
    if canaries
        .windows(2)
        .any(|w| (w[0].shard, w[0].miner) >= (w[1].shard, w[1].miner))
    {
        return None;
    }
    let mut b = b"onyx-canaries-v2".to_vec();
    b.extend_from_slice(&borsh::to_vec(&canaries.to_vec()).unwrap_or_default());
    b.extend_from_slice(&salt.0);
    Some(keccak256(&b))
}

/// Timing and economics of validation, in chain heights and token units.
/// Part of [`crate::round::RoundConfig`] and its tag: fixed when the round
/// opens, never chosen at validation time (L24-F3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidationConfig {
    /// Verdict commits accepted while `beacon_height <= now < verdict_commit_close`.
    pub verdict_commit_close: u64,
    /// Verdict reveals accepted while `verdict_commit_close <= now < verdict_reveal_close`.
    pub verdict_reveal_close: u64,
    /// The coordinator must open its canary list before this height; from
    /// it on, settlement proceeds without canaries (L24-F4).
    pub canary_open_close: u64,
    /// A shard still undecided at this height (its inputs withheld) is
    /// voided: no rewards, no slashes (L25-F5); its work stays taken because
    /// its reveals are public (L26-F2). At least [`MIN_PENDING_GAP`] after
    /// `canary_open_close`.
    pub pending_close: u64,
    /// Sampling rate in ppm.
    pub rate_ppm: u64,
    /// Reward to the first correct reveal per shard.
    pub miner_reward: u128,
    /// Fee per correct verdict (once per validator, shard and root).
    pub validator_fee: u128,
    /// Slash for a wrong verdict or a committed verdict never revealed.
    pub validator_slash: u128,
    /// Slash for an assigned validator who never committed a verdict.
    pub missed_slash: u128,
    /// Smallest share of reveals the coordinator commits to plant as
    /// canaries, in ppm. A blind Agree is caught only on a wrong reveal, so
    /// validator deterrence is computed at this density, not at the sampling
    /// rate (L26-F4). Enforcing it needs a coordinator bond (L25-F7, M6).
    pub min_canary_ppm: u64,
}

impl ValidationConfig {
    /// Validator-side deterrence (L25-F8, L26-F4): both slashes are
    /// positive, a wrong verdict costs at least as much as silence, and a
    /// blind Agree, caught only on a wrong reveal (a canary, at least
    /// `min_canary_ppm` of reveals), loses more than it earns
    /// (`deterrence_holds(validator_slash, validator_fee, min_canary_ppm)`).
    pub fn validators_deterred(&self) -> bool {
        self.validator_slash > 0
            && self.missed_slash > 0
            && self.validator_slash >= self.missed_slash
            && self.min_canary_ppm > 0
            && self.min_canary_ppm <= PPM
            && deterrence_holds(
                self.validator_slash,
                self.validator_fee,
                self.min_canary_ppm,
            )
    }
}

/// Validation error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// The round was not opened in this ledger.
    #[error("round not opened in this ledger")]
    NotOpenedHere,
    /// This round already has a validation (one per round, L24-F3).
    #[error("round already being validated")]
    AlreadyValidating,
    /// Not a member, duplicate member, or bond too low.
    #[error("unknown or invalid validator")]
    BadValidator,
    /// Wrong phase for the action.
    #[error("wrong phase")]
    WrongPhase,
    /// The seed was drawn too late (fewer than [`MIN_VERDICT_WINDOW`] heights
    /// left for verdicts, or the beacon lapsed): the round is void, as on the
    /// contract. No slashes, no rewards (M6-L44-F3).
    #[error("seed drawn too late: the round is void")]
    SeedTooLate,
    /// Height went backwards.
    #[error("height went backwards")]
    TimeWentBack,
    /// Validator not assigned to this reveal.
    #[error("validator not assigned")]
    NotAssigned,
    /// No such reveal in the round.
    #[error("no such reveal")]
    UnknownReveal,
    /// Signature is not the validator's.
    #[error("bad signature")]
    BadSignature,
    /// Second commit for the same task.
    #[error("duplicate verdict")]
    Duplicate,
    /// Reveal without commit, or not matching it, or a Disagree naming the revealed root.
    #[error("verdict does not match its commit")]
    CommitMismatch,
    /// Opened canary list does not match its commitment, a canary does not
    /// match a reveal, or a canary address has history.
    #[error("canary list invalid")]
    CanaryMismatch,
    /// Settled twice.
    #[error("already settled")]
    Settled,
    /// `min_bond < validator_fee x members`: a wrong root's bond would not
    /// cover one extra rerun fee per member (review L2-R2), so the contract
    /// refuses to open such a round.
    #[error("min bond below validator fee x members")]
    FeesExceedBond,
}

/// How a shard's reference was decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettlePath {
    /// Every reveal had the same root, undisputed, no canary.
    Unanimous,
    /// Settlement reran the shard from its pinned inputs.
    Recomputed,
    /// The rerun showed no root can be correct (recipe cannot load, rows
    /// cannot be stored): every reveal is wrong; a `NoValidRoot` verdict is
    /// right, any other answer is wrong (L25-F1).
    NoValidRoot,
    /// Still undecided at `pending_close` (inputs withheld): no rewards, no
    /// slashes. Its reveals are public, so the work is never offered again
    /// (L25-F5, L26-F2).
    Voided,
}

/// Status of one reveal after settlement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevealStatus {
    /// Root equals the shard's reference root.
    Correct,
    /// Root differs from the reference: bond slashed.
    Wrong,
    /// No reference yet (shard disputed): held until M5 recompute.
    Pending,
    /// A canary (never finalized, never rewarded).
    Canary,
    /// The shard was voided: neither rewarded nor slashed (L25-F5).
    Voided,
}

/// A ledger entry (validation.md).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LedgerEntry {
    /// Payment to an actor.
    Reward {
        /// Round.
        round: u64,
        /// Who.
        actor: MinerAddress,
        /// Amount.
        amount: u128,
        /// Why.
        reason: &'static str,
    },
    /// Bond taken from an actor.
    Slash {
        /// Round.
        round: u64,
        /// Who.
        actor: MinerAddress,
        /// Amount.
        amount: u128,
        /// Why.
        reason: &'static str,
    },
}

/// Settlement of one round (or of the shards one `settle` call decided).
/// Only [`Validation::settle`] builds one and nobody can change it; it
/// applies only to the ledger that opened its round, with the same tag and
/// seed (L24-F7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settlement {
    round: u64,
    tag: Hash32,
    seed: Hash32,
    statuses: BTreeMap<(Hash32, MinerAddress), RevealStatus>,
    entries: Vec<LedgerEntry>,
    finalized: BTreeMap<Hash32, Hash32>,
    references: BTreeMap<Hash32, (Hash32, SettlePath)>,
    pending: BTreeSet<Hash32>,
    shards: BTreeSet<Hash32>,
    round_level: bool,
    work_of: BTreeMap<Hash32, Hash32>,
    miners: BTreeSet<MinerAddress>,
    /// The rule set that settled it (L26-F3).
    rules: TypeId,
    /// Addresses this settlement treated as canaries; the ledger refuses it
    /// if any of them was active in another round (L26-F1).
    canaries: BTreeSet<MinerAddress>,
    /// Shards voided with reveals: their work stays taken (L26-F2).
    burned: BTreeSet<Hash32>,
    /// What a dispute needs about each shard settled with a reference (M5,
    /// ADR-0021): the reference, the reveals and verdicts, and the ledger
    /// entries that depended on the reference.
    records: BTreeMap<Hash32, ShardRecord>,
    /// Bond of every validator of the round: `apply` caps validator slashes
    /// against the live ledger total, not a snapshot (L30-F2).
    bonds: BTreeMap<MinerAddress, u128>,
    /// Per shard, the range of `entries` its record's effects were taken
    /// from, so `apply` can rebuild them from the capped entries (L30-F2).
    effect_ranges: BTreeMap<Hash32, (usize, usize)>,
}

impl Settlement {
    /// Round.
    pub fn round(&self) -> u64 {
        self.round
    }

    /// Status per (shard, miner) reveal.
    pub fn statuses(&self) -> &BTreeMap<(Hash32, MinerAddress), RevealStatus> {
        &self.statuses
    }

    /// Ledger entries, in a deterministic order.
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    /// Finalized root per shard with a correct reveal.
    pub fn finalized(&self) -> &BTreeMap<Hash32, Hash32> {
        &self.finalized
    }

    /// Reference and how it was decided, per shard settled by this call. For
    /// [`SettlePath::NoValidRoot`] the reference is [`no_valid_root_marker`].
    pub fn references(&self) -> &BTreeMap<Hash32, (Hash32, SettlePath)> {
        &self.references
    }

    /// Shards still waiting for a reference (inputs not available here).
    pub fn pending(&self) -> &BTreeSet<Hash32> {
        &self.pending
    }

    /// Shards settled by this call (pending ones are not included).
    pub fn shards(&self) -> &BTreeSet<Hash32> {
        &self.shards
    }

    /// True if this settlement carries the round-level entries.
    pub fn round_level(&self) -> bool {
        self.round_level
    }
}

/// Validation of one closed round. `R` is the closed set of address rules
/// this binary trusts for reruns (L25-F2), fixed at compile time.
pub struct Validation<R: RuleSet> {
    round: u64,
    tag: Hash32,
    seed: Hash32,
    cfg: ValidationConfig,
    validators: ValidatorSet,
    outcome: BTreeMap<Hash32, ShardOutcome>,
    canaries_committed: Hash32,
    work_of: BTreeMap<Hash32, Hash32>,
    miners: BTreeSet<MinerAddress>,
    commits: BTreeMap<(MinerAddress, Hash32, MinerAddress), Hash32>,
    verdicts: BTreeMap<(MinerAddress, Hash32, MinerAddress), Verdict>,
    seen: u64,
    /// Canary list, checked once on the first settlement (L23-F3).
    canaries: Option<Vec<Canary>>,
    /// Shards already settled (each at most once).
    done: BTreeSet<Hash32>,
    /// Round-level entries (forfeits, unrevealed verdicts) already emitted.
    round_level_done: bool,
    /// Total slashed per validator in this validation, capped at its bond (L23-F8).
    slashed: BTreeMap<MinerAddress, u128>,
    /// Panel exposure of each member when validation started: `panel_slash`
    /// staked on open dispute panels (L31-F2). Part of the pool count.
    exposure: BTreeMap<MinerAddress, u128>,
    /// The round's shard specs: what settlement reruns (L24-F1).
    specs: BTreeMap<Hash32, ShardSpec>,
    /// Addresses none of which may be a canary: the known miners recorded in
    /// the ledger when the round opened and when validation started, and the
    /// validator set (L24-F2, L25-F4, F6).
    known_miners: BTreeSet<MinerAddress>,
    /// Attestation verdict cache for reruns (only filled through built-in rules).
    cache: AttestCache,
    /// Dispute rules fixed in the round config (ADR-0021).
    dispute: DisputeConfig,
    rules: PhantomData<fn() -> R>,
}

impl<R: RuleSet> Validation<R> {
    /// Start validating a closed round. Everything comes from the round: its
    /// outcome, tag, canary commitment, validation rules, specs, known miners
    /// and the validator set root it fixed when it opened; `validators` must
    /// hash to that root (L23-F2). The seed is computed here from the beacon,
    /// the block hash at the round's `beacon_height` (read from the chain in
    /// M6), and recorded in `ledger`, which must be the ledger that opened the
    /// round: one validation per round (L24-F3). Canary freshness uses the
    /// known miners that ledger recorded, never a copy the caller brings
    /// (L25-F4); this round's committers and validators become known to later
    /// rounds now, not only when a settlement is applied (L25-F6).
    pub fn new(
        round: &Round,
        validators: ValidatorSet,
        beacon: Hash32,
        now: u64,
        ledger: &mut Ledger<R>,
    ) -> Result<Self, ValidationError> {
        let rc = round.config();
        let cfg = rc.validation;
        let outcome = round.outcome().ok_or(ValidationError::WrongPhase)?.clone();
        if validators.root() != rc.validator_set_root {
            return Err(ValidationError::BadValidator);
        }
        if !fees_covered_by_bond(rc.min_bond, cfg.validator_fee, validators.members().count()) {
            return Err(ValidationError::FeesExceedBond);
        }
        if now < rc.beacon_height {
            return Err(ValidationError::WrongPhase);
        }
        if seed_draw_voids(rc.beacon_height, cfg.verdict_commit_close, now) {
            return Err(ValidationError::SeedTooLate);
        }
        let seed = sample_seed(&round.tag(), &beacon);
        // Snapshot: known at open, anyone who committed in any other round so
        // far (L26-F1), every validator seen, and this round's validators.
        let known_miners =
            ledger.record_seed(round.id(), round.tag(), seed, validators.members().copied())?;
        ledger.record_validators(validators.members().copied());
        // Slash caps count what this ledger already took (L29-F5).
        let exposure: BTreeMap<MinerAddress, u128> = validators
            .members()
            .filter_map(|v| ledger.panel_exposure.get(v).map(|t| (*v, *t)))
            .collect();
        let slashed: BTreeMap<MinerAddress, u128> = validators
            .members()
            .filter_map(|v| ledger.validator_slashed.get(v).map(|t| (*v, *t)))
            .collect();
        ledger.record_known(
            round
                .committed_miners()
                .into_iter()
                .chain(validators.members().copied()),
        );
        Ok(Validation {
            round: round.id(),
            tag: round.tag(),
            seed,
            cfg,
            validators,
            outcome,
            canaries_committed: rc.canary_commit,
            work_of: round.work_of(),
            miners: round.committed_miners(),
            commits: BTreeMap::new(),
            verdicts: BTreeMap::new(),
            seen: now,
            canaries: None,
            done: BTreeSet::new(),
            round_level_done: false,
            slashed,
            exposure,
            specs: round.specs().clone(),
            known_miners,
            cache: AttestCache::new(),
            dispute: rc.dispute,
            rules: PhantomData,
        })
    }

    /// The seed in use (validators sign it).
    pub fn seed(&self) -> Hash32 {
        self.seed
    }

    /// The reveals `validator` must check: (shard, miner, revealed root).
    pub fn tasks_for(&self, validator: &MinerAddress) -> Vec<(Hash32, MinerAddress, Hash32)> {
        if !self.validators.contains(validator) {
            return Vec::new();
        }
        self.outcome
            .iter()
            .flat_map(|(shard, o)| o.reveals.iter().map(move |(_, m, r)| (*shard, *m, *r)))
            .filter(|(shard, m, _)| assigned(&self.seed, validator, shard, m, self.cfg.rate_ppm))
            .collect()
    }

    fn revealed_root(&self, shard: &Hash32, miner: &MinerAddress) -> Option<Hash32> {
        self.outcome
            .get(shard)?
            .reveals
            .iter()
            .find(|(_, m, _)| m == miner)
            .map(|(_, _, r)| *r)
    }

    fn check_time(&self, now: u64) -> Result<(), ValidationError> {
        if self.round_level_done {
            return Err(ValidationError::Settled);
        }
        if now < self.seen {
            return Err(ValidationError::TimeWentBack);
        }
        Ok(())
    }

    /// Post a signed verdict commit for an assigned task.
    pub fn commit_verdict(&mut self, now: u64, v: &SignedVerdict) -> Result<(), ValidationError> {
        self.check_time(now)?;
        if now >= self.cfg.verdict_commit_close {
            return Err(ValidationError::WrongPhase);
        }
        let msg = verdict_message(&self.seed, &v.shard, &v.miner, &v.commit);
        if eip191_recover(msg.as_bytes(), &v.sig) != Some(v.validator) {
            return Err(ValidationError::BadSignature);
        }
        if !self.validators.contains(&v.validator) {
            return Err(ValidationError::BadValidator);
        }
        if self.revealed_root(&v.shard, &v.miner).is_none() {
            return Err(ValidationError::UnknownReveal);
        }
        if !assigned(
            &self.seed,
            &v.validator,
            &v.shard,
            &v.miner,
            self.cfg.rate_ppm,
        ) {
            return Err(ValidationError::NotAssigned);
        }
        let key = (v.validator, v.shard, v.miner);
        if self.commits.contains_key(&key) {
            return Err(ValidationError::Duplicate);
        }
        self.commits.insert(key, v.commit);
        self.seen = now;
        Ok(())
    }

    /// Reveal a committed verdict.
    pub fn reveal_verdict(
        &mut self,
        now: u64,
        validator: MinerAddress,
        shard: Hash32,
        miner: MinerAddress,
        verdict: Verdict,
        salt: Hash32,
    ) -> Result<(), ValidationError> {
        self.check_time(now)?;
        if now < self.cfg.verdict_commit_close || now >= self.cfg.verdict_reveal_close {
            return Err(ValidationError::WrongPhase);
        }
        let key = (validator, shard, miner);
        let want = self
            .commits
            .get(&key)
            .ok_or(ValidationError::CommitMismatch)?;
        if verdict_commit_hash(&self.seed, &validator, &shard, &miner, &verdict, &salt) != *want {
            return Err(ValidationError::CommitMismatch);
        }
        if let Verdict::Disagree { root } = verdict {
            if Some(root) == self.revealed_root(&shard, &miner) {
                return Err(ValidationError::CommitMismatch);
            }
        }
        if self.verdicts.contains_key(&key) {
            return Err(ValidationError::Duplicate);
        }
        self.verdicts.insert(key, verdict);
        self.seen = now;
        Ok(())
    }

    /// Slash a validator, capped so its total never exceeds its bond.
    fn slash_validator(
        &mut self,
        entries: &mut Vec<LedgerEntry>,
        v: MinerAddress,
        amount: u128,
        reason: &'static str,
    ) {
        let used = self.slashed.entry(v).or_insert(0);
        let left = self.validators.bond(&v).saturating_sub(*used);
        let take = amount.min(left);
        if take > 0 {
            *used += take;
            entries.push(LedgerEntry::Slash {
                round: self.round,
                actor: v,
                amount: take,
                reason,
            });
        }
    }

    /// Settle after verdict reveals close.
    ///
    /// Truth per shard (L24-F1): a shard whose reveals all share one root, that
    /// no verdict disputes and that holds no canary takes that root. Every
    /// other shard is rerun here from the spec the round pinned, with inputs
    /// from `env` (each checked against its pinned hash). Nobody names a
    /// reference; a shard whose inputs are not available stays pending and a
    /// later call settles it, until `pending_close`, when it is voided
    /// (L25-F5). Reruns use the registry in `env` only if every address rule
    /// in it is the built-in type `R` names (L25-F2); otherwise nothing is
    /// rerun and disputed shards stay pending.
    ///
    /// Canaries: the first call opens the committed list (`Some`); before
    /// `canary_open_close` it must. From that height on, `None` settles
    /// without canaries (L24-F4). Canary addresses must match reveals and have
    /// no history in the ledger that opened the round (L24-F2, L25-F4, F6).
    /// Later calls ignore the argument.
    pub fn settle(
        &mut self,
        now: u64,
        canaries: Option<(&[Canary], &Hash32)>,
        env: &RecomputeEnv<'_>,
    ) -> Result<Settlement, ValidationError> {
        if now < self.seen {
            return Err(ValidationError::TimeWentBack);
        }
        if now < self.cfg.verdict_reveal_close {
            return Err(ValidationError::WrongPhase);
        }
        if self.canaries.is_none() {
            match canaries {
                Some((list, salt)) => {
                    if canary_commit(list, salt) != Some(self.canaries_committed) {
                        return Err(ValidationError::CanaryMismatch);
                    }
                    for c in list {
                        if self.revealed_root(&c.shard, &c.miner) != Some(c.root)
                            || self.known_miners.contains(&c.miner)
                        {
                            return Err(ValidationError::CanaryMismatch);
                        }
                    }
                    self.canaries = Some(list.to_vec());
                }
                None if now >= self.cfg.canary_open_close => self.canaries = Some(Vec::new()),
                None => return Err(ValidationError::WrongPhase),
            }
        }
        let canary: BTreeSet<(Hash32, MinerAddress)> = self
            .canaries
            .iter()
            .flatten()
            .map(|c| (c.shard, c.miner))
            .collect();
        let round = self.round;
        let cfg = self.cfg;
        let first_call = !self.round_level_done;
        let mut statuses = BTreeMap::new();
        let mut entries = Vec::new();
        let mut finalized = BTreeMap::new();
        let mut references = BTreeMap::new();
        let mut pending = BTreeSet::new();
        let mut newly = BTreeSet::new();
        let mut burned = BTreeSet::new();
        let mut records = BTreeMap::new();
        let mut effect_ranges: BTreeMap<Hash32, (usize, usize)> = BTreeMap::new();
        let mut paid: BTreeSet<(MinerAddress, Hash32, Hash32)> = BTreeSet::new();
        let shards: Vec<Hash32> = self.outcome.keys().copied().collect();
        let members: Vec<MinerAddress> = self.validators.members().copied().collect();
        let rules_ok = env.reg.rules_built_in::<R>();
        let void = now >= cfg.pending_close;
        // What this settlement will certainly still slash each validator
        // (L31-F3, L33-N2): one `missed_slash` per shard not yet settled where
        // it is assigned but committed nothing, and on the first call one
        // `validator_slash` per verdict it committed and never revealed. A
        // wrong-verdict slash depends on the reference and is not counted:
        // counting that worst case dropped every active validator from the
        // pool below and sent every shard to a rerun. A later shortfall is
        // caught at challenge time (no panel: the shard is rerun). Commits are
        // read once, into per-shard verdict-givers (L33-N5).
        let mut givers: BTreeMap<Hash32, BTreeSet<MinerAddress>> = BTreeMap::new();
        let mut at_risk: BTreeMap<MinerAddress, u128> = BTreeMap::new();
        for key in self.commits.keys() {
            let (v, s, _) = key;
            givers.entry(*s).or_default().insert(*v);
            if first_call && !self.verdicts.contains_key(key) {
                let e = at_risk.entry(*v).or_insert(0);
                *e = e.saturating_add(cfg.validator_slash);
            }
        }
        for (shard, o) in &self.outcome {
            if self.done.contains(shard) || o.reveals.is_empty() {
                continue;
            }
            let committed = givers.get(shard);
            for v in &members {
                if committed.is_some_and(|g| g.contains(v)) {
                    continue;
                }
                if o.reveals
                    .iter()
                    .any(|(_, m, _)| assigned(&self.seed, v, shard, m, cfg.rate_ppm))
                {
                    let e = at_risk.entry(*v).or_insert(0);
                    *e = e.saturating_add(cfg.missed_slash);
                }
            }
        }
        let slashed_before = self.slashed.clone();

        for shard in shards {
            if self.done.contains(&shard) {
                continue;
            }
            let o = self.outcome[&shard].clone();
            if o.reveals.is_empty() {
                // Nothing to judge: settled, not finalized (L23-F4).
                self.done.insert(shard);
                newly.insert(shard);
                continue;
            }
            let roots: BTreeSet<Hash32> = o.reveals.iter().map(|(_, _, r)| *r).collect();
            let disputed = self.verdicts.iter().any(|((_, s, _), v)| {
                *s == shard && matches!(v, Verdict::Disagree { .. } | Verdict::NoValidRoot)
            });
            let has_canary = o
                .reveals
                .iter()
                .any(|(_, m, _)| canary.contains(&(shard, *m)));
            // A shard that could not be disputed afterwards (too few bonded
            // validators outside its miners and verdict-givers to draw a
            // panel, plus one for a challenger from that pool) is never taken
            // on unanimity: it is rerun here (L29-F4).
            let shard_miners: BTreeSet<MinerAddress> =
                o.reveals.iter().map(|(_, m, _)| *m).collect();
            let verdict_givers = givers.get(&shard).cloned().unwrap_or_default();
            // A validator whose remaining bond cannot cover a panel slash is
            // never drawn, so it does not count either (L30-F3): its bond
            // minus what the ledger took, what open panels stake (L31-F2)
            // and everything this settlement can still slash (L31-F3). A
            // later shortfall is caught at challenge time: a unanimous shard
            // with no panel is rerun (`Ledger::rerun_shard`).
            let pool = members
                .iter()
                .filter(|v| !shard_miners.contains(*v) && !verdict_givers.contains(*v))
                .filter(|v| {
                    let taken = slashed_before
                        .get(*v)
                        .copied()
                        .unwrap_or(0)
                        .saturating_add(self.exposure.get(*v).copied().unwrap_or(0))
                        .saturating_add(at_risk.get(*v).copied().unwrap_or(0));
                    crate::dispute::can_sit(
                        self.validators.bond(v),
                        taken,
                        self.dispute.panel_slash,
                    )
                })
                .count();
            let disputable = pool > self.dispute.panel_size as usize;
            let unanimous = roots.len() == 1 && !disputed && !has_canary && disputable;
            let decided = match (unanimous, roots.iter().next(), self.specs.get(&shard)) {
                (true, Some(r), _) => Reference::Root(*r),
                (false, _, Some(_)) if !rules_ok => {
                    Reference::Undecided("registry rules are not the built-in ones".into())
                }
                (false, _, Some(spec)) => env.reference(spec, &self.cache),
                _ => Reference::Undecided("no spec for the shard".into()),
            };
            let (reference, path) = match decided {
                Reference::Root(r) if unanimous => (Some(r), SettlePath::Unanimous),
                Reference::Root(r) => (Some(r), SettlePath::Recomputed),
                Reference::NoValidRoot => (None, SettlePath::NoValidRoot),
                Reference::Undecided(_) if void => {
                    // Inputs withheld past the deadline: nobody wins or loses
                    // on this shard (L25-F5). If its reveals are public, its
                    // work is never offered again (L26-F2).
                    self.done.insert(shard);
                    newly.insert(shard);
                    // Burn only if a real (non-canary) reveal made a root
                    // public; a canary-only shard is released (L27-F3).
                    if o.reveals
                        .iter()
                        .any(|(_, m, _)| !canary.contains(&(shard, *m)))
                    {
                        burned.insert(shard);
                    }
                    references.insert(shard, (Hash32::ZERO, SettlePath::Voided));
                    for (_, m, _) in &o.reveals {
                        statuses.insert((shard, *m), RevealStatus::Voided);
                    }
                    continue;
                }
                Reference::Undecided(_) => {
                    pending.insert(shard);
                    for (_, m, _) in &o.reveals {
                        statuses.insert((shard, *m), RevealStatus::Pending);
                    }
                    continue;
                }
            };
            self.done.insert(shard);
            newly.insert(shard);
            references.insert(
                shard,
                (
                    reference.unwrap_or_else(|| no_valid_root_marker(&shard)),
                    path,
                ),
            );
            let effects_from = entries.len();
            // Miners.
            let mut rewarded = false;
            for (_, m, r) in &o.reveals {
                if canary.contains(&(shard, *m)) {
                    statuses.insert((shard, *m), RevealStatus::Canary);
                    continue;
                }
                if Some(*r) == reference {
                    statuses.insert((shard, *m), RevealStatus::Correct);
                    if !rewarded {
                        rewarded = true;
                        entries.push(LedgerEntry::Reward {
                            round,
                            actor: *m,
                            amount: cfg.miner_reward,
                            reason: "first correct reveal",
                        });
                    }
                } else {
                    statuses.insert((shard, *m), RevealStatus::Wrong);
                    entries.push(LedgerEntry::Slash {
                        round,
                        actor: *m,
                        amount: o.bonds.get(m).copied().unwrap_or(0),
                        reason: "revealed a wrong root",
                    });
                }
            }
            // A root is finalized whenever it is known: rewarded, or rerun
            // here with no correct reveal (no reward; the root is public in
            // `references`, so the work must not be offered again, L27-F4).
            if let Some(reference) = reference {
                finalized.insert(shard, reference);
            }
            // Validators, judged against the reference. One missed-task slash
            // per (validator, shard) at most (L23-F9); Abstain counts as missed
            // (L23-F7); canary fees once per (validator, shard) (L23-F10).
            for v in &members {
                let mut assigned_any = false;
                let mut answered = false;
                for (_, m, r) in &o.reveals {
                    if !assigned(&self.seed, v, &shard, m, cfg.rate_ppm) {
                        continue;
                    }
                    assigned_any = true;
                    let Some(verdict) = self.verdicts.get(&(*v, shard, *m)).copied() else {
                        // Committed but never revealed: slashed once at round
                        // level, not also as a missed task (L25-F9).
                        if self.commits.contains_key(&(*v, shard, *m)) {
                            answered = true;
                        }
                        continue;
                    };
                    let Some(right) = verdict_right(&verdict, r, reference.as_ref()) else {
                        continue;
                    };
                    answered = true;
                    if right {
                        let is_canary = canary.contains(&(shard, *m));
                        let fee_key = (*v, shard, if is_canary { Hash32::ZERO } else { *r });
                        if paid.insert(fee_key) {
                            entries.push(LedgerEntry::Reward {
                                round,
                                actor: *v,
                                amount: cfg.validator_fee,
                                reason: "correct verdict",
                            });
                        }
                    } else {
                        self.slash_validator(
                            &mut entries,
                            *v,
                            cfg.validator_slash,
                            "wrong verdict",
                        );
                    }
                }
                if assigned_any && !answered {
                    self.slash_validator(
                        &mut entries,
                        *v,
                        cfg.missed_slash,
                        "assigned task not answered",
                    );
                }
            }
            // What a later dispute needs (ADR-0021): only the entries that
            // depend on the reference are reversed if a panel overturns it.
            let effects: Vec<LedgerEntry> = entries[effects_from..]
                .iter()
                .filter(|e| crate::dispute::depends_on_reference(e))
                .cloned()
                .collect();
            effect_ranges.insert(shard, (effects_from, entries.len()));
            records.insert(
                shard,
                ShardRecord {
                    round,
                    shard,
                    tag: self.tag,
                    seed: self.seed,
                    work: self.work_of.get(&shard).copied().unwrap_or(Hash32::ZERO),
                    reference: reference.unwrap_or_else(|| no_valid_root_marker(&shard)),
                    path,
                    reveals: o.reveals.clone(),
                    bonds: o.bonds.clone(),
                    canaries: o
                        .reveals
                        .iter()
                        .filter(|(_, m, _)| canary.contains(&(shard, *m)))
                        .map(|(_, m, _)| *m)
                        .collect(),
                    verdicts: self
                        .verdicts
                        .iter()
                        .filter(|((_, s, _), _)| *s == shard)
                        .map(|((v, _, m), verdict)| (*v, *m, *verdict))
                        .collect(),
                    verdict_givers,
                    effects,
                    spec: self.specs.get(&shard).cloned(),
                    validators: self.validators.clone(),
                    validation: cfg,
                    dispute: self.dispute,
                    settled_at: now,
                },
            );
        }
        if first_call {
            for o in self.outcome.values() {
                for (miner, bond) in &o.forfeited {
                    entries.push(LedgerEntry::Slash {
                        round,
                        actor: *miner,
                        amount: *bond,
                        reason: "commit not revealed",
                    });
                }
            }
            let unrevealed: Vec<MinerAddress> = self
                .commits
                .keys()
                .filter(|k| !self.verdicts.contains_key(*k))
                .map(|k| k.0)
                .collect();
            for v in unrevealed {
                self.slash_validator(&mut entries, v, cfg.validator_slash, "verdict not revealed");
            }
        }
        self.round_level_done = true;
        self.seen = now;
        Ok(Settlement {
            round,
            tag: self.tag,
            seed: self.seed,
            statuses,
            entries,
            finalized,
            references,
            pending,
            shards: newly,
            round_level: first_call,
            work_of: self.work_of.clone(),
            miners: self.miners.clone(),
            rules: TypeId::of::<R>(),
            canaries: self.canaries.iter().flatten().map(|c| c.miner).collect(),
            burned,
            records,
            bonds: self
                .validators
                .members()
                .map(|v| (*v, self.validators.bond(v)))
                .collect(),
            effect_ranges,
        })
    }
}

/// What a ledger knows about a round it opened: its tag, its validation
/// seed once validation starts, the known miners when it opened (L25-F4),
/// and every address with history when its validation started: the canary
/// freshness snapshot `apply` checks, fixed then so that later activity
/// cannot veto the round's settlement (L27-F2).
#[derive(Clone, Debug, PartialEq, Eq)]
struct RoundRecord {
    tag: Hash32,
    seed: Option<Hash32>,
    known_at_open: BTreeSet<MinerAddress>,
    known_at_validation: BTreeSet<MinerAddress>,
}

/// A fresh ledger id: random per instance and never chosen by a caller, so
/// no second ledger can reproduce another's round tags (L27-F1). In M6 the
/// settlement contract's address plays this role.
fn fresh_ledger_id() -> Hash32 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut b = b"onyx-ledger-v1".to_vec();
    for i in 0..4u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(n);
        h.write_u64(i);
        b.extend_from_slice(&h.finish().to_be_bytes());
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    b.extend_from_slice(&nanos.to_be_bytes());
    b.extend_from_slice(&n.to_be_bytes());
    keccak256(&b)
}

/// Amount of a ledger entry.
pub(crate) fn entry_amount(e: &LedgerEntry) -> u128 {
    match e {
        LedgerEntry::Reward { amount, .. } | LedgerEntry::Slash { amount, .. } => *amount,
    }
}

/// Whether a revealed verdict on a reveal of `miner_root` is right against
/// `reference` (`None` = no valid root): `None` for Abstain (not judged).
/// With no valid root, only a NoValidRoot verdict is right (L25-F1). The
/// settlement contract judges with the same rule (`OnyxDisputes.verdictRight`,
/// ADR-0024; shared vector `disputes.verdictRight`).
pub fn verdict_right(
    verdict: &Verdict,
    miner_root: &Hash32,
    reference: Option<&Hash32>,
) -> Option<bool> {
    Some(match (verdict, reference) {
        (Verdict::Abstain, _) => return None,
        (Verdict::NoValidRoot, None) => true,
        (_, None) | (Verdict::NoValidRoot, Some(_)) => false,
        (Verdict::Agree, Some(re)) => miner_root == re,
        (Verdict::Disagree { root }, Some(re)) => miner_root != re && root == re,
    })
}

/// True for ledger slashes taken from a validator's bond (capped at it).
pub(crate) fn is_validator_slash(reason: &str) -> bool {
    matches!(
        reason,
        "wrong verdict"
            | "assigned task not answered"
            | "verdict not revealed"
            | "panel: no reveal or off the majority"
    )
}

/// Record across rounds: entries, finalized work (`work_id -> (round, root)`),
/// work still open or pending, work burned by voiding, applied rounds
/// (idempotent, L22-F9), miner addresses with history and the rounds each
/// committed in (canaries must be fresh), and the rounds opened in it with
/// their tag and validation seed. Only [`Round::open_preflighted`], [`Round::commit`],
/// [`Validation::new`] and [`Ledger::apply`] change it (L24-F2, F5, F7).
///
/// `R` is the rule set that settles its rounds: [`Validation`] takes only a
/// `Ledger<R>` of its own `R`, and `apply` refuses a settlement made under
/// another set (L26-F3). The remaining trust is the published settlement
/// binary, which independent settlers run in M6. Not `Clone`: a copied
/// ledger would be a second history. Every ledger has a random id that is in
/// the tag of each round it opens (L27-F1); `Default` and [`Ledger::new`]
/// both draw a fresh one, and nothing takes an id from a caller.
#[derive(Debug)]
pub struct Ledger<R: RuleSet> {
    pub(crate) id: Hash32,
    pub(crate) entries: Vec<LedgerEntry>,
    pub(crate) finalized: BTreeMap<Hash32, (u64, Hash32)>,
    open: BTreeSet<Hash32>,
    pub(crate) burned: BTreeSet<Hash32>,
    applied: BTreeSet<(u64, Hash32)>,
    known_miners: BTreeSet<MinerAddress>,
    active_in: BTreeMap<MinerAddress, BTreeSet<u64>>,
    validators_seen: BTreeSet<MinerAddress>,
    rounds: BTreeMap<u64, RoundRecord>,
    /// Settled shards with a reference, by (round, shard): what disputes
    /// judge against (M5, ADR-0021).
    pub(crate) settled: BTreeMap<(u64, Hash32), ShardRecord>,
    /// Every dispute opened in this ledger, by id.
    pub(crate) disputes: BTreeMap<Hash32, Dispute>,
    /// The one open dispute per (round, shard), if any.
    pub(crate) open_disputes: BTreeMap<(u64, Hash32), Hash32>,
    /// Every challenge ever accepted (key of challenger, round, shard,
    /// claimed root, bond, level): a signed challenge opens once (L29-F1).
    pub(crate) consumed_challenges: BTreeSet<Hash32>,
    /// Shards whose dispute ended Upheld, Overturned or Corrected: closed to
    /// further disputes (L29-F6).
    pub(crate) closed_disputes: BTreeSet<(u64, Hash32)>,
    /// Net validator slashes per address across every validation and
    /// dispute in this ledger: caps are against this, not per call (L29-F5).
    pub(crate) validator_slashed: BTreeMap<MinerAddress, u128>,
    /// Work settled with no valid root, held (not offered again) until its
    /// challenge window closes and no dispute on it is open:
    /// work -> (release height, round, shard) (L30-F4).
    pub(crate) held: BTreeMap<Hash32, (u64, u64, Hash32)>,
    /// `panel_slash` each validator stakes on open dispute panels (L31-F2).
    pub(crate) panel_exposure: BTreeMap<MinerAddress, u128>,
    /// Unanimous shards a challenge found no panel for, and shards whose
    /// dispute ended without a majority: the settler must rerun them
    /// (L31-F3, q13 (a)).
    pub(crate) rerun_required: BTreeSet<(u64, Hash32)>,
    /// Height after which each pending rerun may expire (M5-L46-F4).
    pub(crate) rerun_deadline: BTreeMap<(u64, Hash32), u64>,
    /// Shards whose rerun expired: never final here (M5-L46-F4).
    pub(crate) unresolved: BTreeSet<(u64, Hash32)>,
    rules: PhantomData<fn() -> R>,
}

impl<R: RuleSet> Default for Ledger<R> {
    fn default() -> Self {
        Ledger {
            id: fresh_ledger_id(),
            entries: Vec::new(),
            finalized: BTreeMap::new(),
            open: BTreeSet::new(),
            burned: BTreeSet::new(),
            applied: BTreeSet::new(),
            known_miners: BTreeSet::new(),
            active_in: BTreeMap::new(),
            validators_seen: BTreeSet::new(),
            rounds: BTreeMap::new(),
            settled: BTreeMap::new(),
            disputes: BTreeMap::new(),
            open_disputes: BTreeMap::new(),
            consumed_challenges: BTreeSet::new(),
            closed_disputes: BTreeSet::new(),
            validator_slashed: BTreeMap::new(),
            held: BTreeMap::new(),
            panel_exposure: BTreeMap::new(),
            rerun_required: BTreeSet::new(),
            rerun_deadline: BTreeMap::new(),
            unresolved: BTreeSet::new(),
            rules: PhantomData,
        }
    }
}

impl<R: RuleSet> Ledger<R> {
    /// A new, empty ledger with a fresh random id.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty, read-only mirror of the settlement contract at `contract` on
    /// `chain_id`: its id is the contract's `ledgerId()`
    /// ([`onyx_core::evm_ledger_id`], ADR-0022), so the round tags it issues,
    /// and with them every seed, verdict commit and dispute id, are the
    /// contract's (M6-G4). Only for following that contract: every mirror of
    /// one contract shares its id, so their verdicts are interchangeable
    /// (L27-F1, accepted residual M6-G4R-F3). Off-chain ledgers use
    /// [`Ledger::new`].
    pub fn mirror_of_contract(chain_id: u64, contract: &MinerAddress) -> Self {
        Ledger {
            id: onyx_core::evm_ledger_id(chain_id, contract),
            ..Self::default()
        }
    }

    /// This ledger's id (part of every round tag it issues).
    pub fn id(&self) -> Hash32 {
        self.id
    }

    /// Every entry, in order.
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    /// Finalized work: work id -> (round, root). A dispute or rerun may
    /// still change a root listed here; [`Ledger::final_root`] and
    /// [`Ledger::finality`] say whether it is final (M5-L46-F4).
    pub fn finalized(&self) -> &BTreeMap<Hash32, (u64, Hash32)> {
        &self.finalized
    }

    /// Work offered in a round not yet settled, or pending a reference.
    pub fn open_work(&self) -> &BTreeSet<Hash32> {
        &self.open
    }

    /// Miner addresses with any history.
    pub fn known_miners(&self) -> &BTreeSet<MinerAddress> {
        &self.known_miners
    }

    /// Work ids that may not be offered: finalized, open, or burned by
    /// voiding after its reveals became public (L26-F2).
    pub fn taken(&self) -> BTreeSet<Hash32> {
        self.finalized
            .keys()
            .chain(self.open.iter())
            .chain(self.burned.iter())
            .chain(self.held.keys())
            .copied()
            .collect()
    }

    /// Release held no-valid-root work whose challenge window has closed,
    /// that has no open dispute and no pending rerun (L30-F4, M5-L46-F1): a
    /// later rerun may still finalize it in its own round, so offering it
    /// again first would pay its "first correct reveal" twice. Called by
    /// [`Round::open_preflighted`].
    pub(crate) fn release_held(&mut self, now: u64) {
        let open = &self.open_disputes;
        let rerun = &self.rerun_required;
        self.held.retain(|_, (until, round, shard)| {
            now < *until
                || open.contains_key(&(*round, *shard))
                || rerun.contains(&(*round, *shard))
        });
    }

    /// Tag of a round opened in this ledger.
    pub fn round_tag(&self, id: u64) -> Option<Hash32> {
        self.rounds.get(&id).map(|r| r.tag)
    }

    /// Record that `miner` committed in round `id` (from `Round::commit`).
    pub(crate) fn record_commit(&mut self, id: u64, miner: MinerAddress) {
        self.active_in.entry(miner).or_default().insert(id);
    }

    /// Addresses that committed in any round other than `id`.
    pub(crate) fn active_elsewhere(&self, id: u64) -> BTreeSet<MinerAddress> {
        self.active_in
            .iter()
            .filter(|(_, rounds)| rounds.iter().any(|r| *r != id))
            .map(|(m, _)| *m)
            .collect()
    }

    /// Record a round's validator set members (from `Validation::new`).
    pub(crate) fn record_validators(&mut self, who: impl IntoIterator<Item = MinerAddress>) {
        self.validators_seen.extend(who);
    }

    /// True if a round with this id was opened in this ledger.
    pub fn has_round(&self, id: u64) -> bool {
        self.rounds.contains_key(&id)
    }

    /// Record an opened round, its known-miner snapshot, and mark its work
    /// open (from `Round::open_preflighted`).
    pub(crate) fn record_open(&mut self, id: u64, tag: Hash32, works: BTreeSet<Hash32>) {
        self.rounds.insert(
            id,
            RoundRecord {
                tag,
                seed: None,
                known_at_open: self.known_miners.clone(),
                known_at_validation: BTreeSet::new(),
            },
        );
        self.open.extend(works);
    }

    /// Record a round's validation seed, once (from `Validation::new`), and
    /// return its canary freshness snapshot: the known miners this ledger
    /// recorded when the round opened, plus every address with history now
    /// (other rounds' committers, validators, known miners) and `extra`. The
    /// snapshot is stored and is what `apply` checks (L27-F2).
    pub(crate) fn record_seed(
        &mut self,
        id: u64,
        tag: Hash32,
        seed: Hash32,
        extra: impl IntoIterator<Item = MinerAddress>,
    ) -> Result<BTreeSet<MinerAddress>, ValidationError> {
        let mut now: BTreeSet<MinerAddress> = self.known_miners.clone();
        now.extend(self.active_elsewhere(id));
        now.extend(self.validators_seen.iter().copied());
        now.extend(extra);
        let rec = self
            .rounds
            .get_mut(&id)
            .filter(|r| r.tag == tag)
            .ok_or(ValidationError::NotOpenedHere)?;
        if rec.seed.is_some() {
            return Err(ValidationError::AlreadyValidating);
        }
        rec.seed = Some(seed);
        now.extend(rec.known_at_open.iter().copied());
        rec.known_at_validation = now.clone();
        Ok(now)
    }

    /// Mark addresses as having history (committers and validators of a
    /// round whose validation started, L25-F6).
    pub(crate) fn record_known(&mut self, who: impl IntoIterator<Item = MinerAddress>) {
        self.known_miners.extend(who);
    }

    /// Record a settlement once. It must come from the validation recorded
    /// for its round in this ledger (same tag and seed, L24-F7). Work ids and
    /// miners come from the settlement (which took them from the round,
    /// L23-F5). Every newly settled shard leaves `open`: finalized work is
    /// recorded; work that settled without a correct reveal may be offered
    /// again (L23-F4), except voided work whose reveals are public, which is
    /// burned (L26-F2). A settlement made under another rule set, or one
    /// naming as a canary an address that had history when the round's
    /// validation started, is refused (L26-F1, F3, L27-F2). Returns false if the settlement does not
    /// belong to this ledger, is refused, or any part was already applied.
    pub fn apply(&mut self, s: &Settlement) -> bool {
        let ours = s.rules == TypeId::of::<R>()
            && self
                .rounds
                .get(&s.round)
                .is_some_and(|r| r.tag == s.tag && r.seed == Some(s.seed));
        if !ours {
            return false;
        }
        // A canary must be fresh as of the snapshot taken when validation
        // started (L26-F1); activity recorded after that cannot veto the
        // round (L27-F2).
        let stale = self
            .rounds
            .get(&s.round)
            .is_some_and(|r| s.canaries.iter().any(|m| r.known_at_validation.contains(m)));
        if stale {
            return false;
        }
        let fresh: Vec<(u64, Hash32)> = s
            .shards
            .iter()
            .map(|sh| (s.round, *sh))
            .chain(s.round_level.then_some((s.round, Hash32::ZERO)))
            .collect();
        if fresh.iter().any(|k| self.applied.contains(k)) {
            return false;
        }
        self.applied.extend(fresh);
        // Validator slashes are capped here against what this ledger has
        // already taken (L30-F2): two validations settled from the same
        // snapshot, or a dispute in between, can never take more than a bond.
        let capped: Vec<LedgerEntry> = s
            .entries
            .iter()
            .map(|e| match e {
                LedgerEntry::Slash {
                    round,
                    actor,
                    amount,
                    reason,
                } if is_validator_slash(reason) => {
                    let used = self.validator_slashed.entry(*actor).or_insert(0);
                    let bond = s.bonds.get(actor).copied().unwrap_or(0);
                    let take = (*amount).min(bond.saturating_sub(*used));
                    *used = used.saturating_add(take);
                    LedgerEntry::Slash {
                        round: *round,
                        actor: *actor,
                        amount: take,
                        reason,
                    }
                }
                other => other.clone(),
            })
            .collect();
        self.entries
            .extend(capped.iter().filter(|e| entry_amount(e) > 0).cloned());
        for shard in &s.shards {
            if let Some(work) = s.work_of.get(shard) {
                self.open.remove(work);
                if let Some(root) = s.finalized.get(shard) {
                    self.finalized.entry(*work).or_insert((s.round, *root));
                }
                if s.burned.contains(shard) {
                    self.burned.insert(*work);
                }
            }
            if let Some(rec) = s.records.get(shard) {
                let mut rec = rec.clone();
                if let Some((from, to)) = s.effect_ranges.get(shard) {
                    rec.effects = capped
                        .get(*from..*to)
                        .unwrap_or_default()
                        .iter()
                        .filter(|e| entry_amount(e) > 0 && crate::dispute::depends_on_reference(e))
                        .cloned()
                        .collect();
                }
                // No valid root: the work is not offered again until no
                // dispute can change that (L30-F4).
                if rec.path == SettlePath::NoValidRoot {
                    if let Some(work) = s.work_of.get(shard) {
                        self.held.insert(
                            *work,
                            (
                                rec.settled_at.saturating_add(rec.dispute.challenge_window),
                                s.round,
                                *shard,
                            ),
                        );
                    }
                }
                self.settled.insert((s.round, *shard), rec);
            }
        }
        if s.round_level {
            self.known_miners.extend(s.miners.iter().copied());
        }
        true
    }

    /// Net balance change of an actor over all entries.
    pub fn balance(&self, actor: &MinerAddress) -> i128 {
        self.entries.iter().fold(0i128, |acc, e| match e {
            LedgerEntry::Reward {
                actor: a, amount, ..
            } if a == actor => acc.saturating_add(i128::try_from(*amount).unwrap_or(i128::MAX)),
            LedgerEntry::Slash {
                actor: a, amount, ..
            } if a == actor => acc.saturating_sub(i128::try_from(*amount).unwrap_or(i128::MAX)),
            _ => acc,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review L2-R2: the bond covers one fee per member, exactly at the bound;
    /// the PoC's fee 400 x 9 members against bond 1,000 is refused; overflow
    /// is refused.
    #[test]
    fn fees_covered_by_bond_bounds() {
        assert!(fees_covered_by_bond(1_000, 111, 9));
        assert!(!fees_covered_by_bond(1_000, 112, 9));
        assert!(!fees_covered_by_bond(1_000, 400, 9));
        assert!(
            fees_covered_by_bond(4_000_000, 50_000, 16),
            "testnet and demo config"
        );
        assert!(fees_covered_by_bond(0, 0, 128));
        assert!(!fees_covered_by_bond(u128::MAX, u128::MAX, 2));
    }

    /// A rule set for ledger unit tests.
    struct TestRules;
    impl RuleSet for TestRules {
        fn proof_type(_: &str) -> Option<TypeId> {
            None
        }
        fn canon_type(_: &str) -> Option<TypeId> {
            None
        }
    }

    fn settlement_slashing(
        ledger: &mut Ledger<TestRules>,
        round: u64,
        v: MinerAddress,
        bond: u128,
        amount: u128,
    ) -> Settlement {
        let tag = keccak256(&round.to_be_bytes());
        let seed = keccak256(&[round.to_be_bytes().as_slice(), b"seed"].concat());
        ledger.rounds.insert(
            round,
            RoundRecord {
                tag,
                seed: Some(seed),
                known_at_open: BTreeSet::new(),
                known_at_validation: BTreeSet::new(),
            },
        );
        Settlement {
            round,
            tag,
            seed,
            statuses: BTreeMap::new(),
            entries: vec![LedgerEntry::Slash {
                round,
                actor: v,
                amount,
                reason: "wrong verdict",
            }],
            finalized: BTreeMap::new(),
            references: BTreeMap::new(),
            pending: BTreeSet::new(),
            shards: BTreeSet::new(),
            round_level: true,
            work_of: BTreeMap::new(),
            miners: BTreeSet::new(),
            rules: TypeId::of::<TestRules>(),
            canaries: BTreeSet::new(),
            burned: BTreeSet::new(),
            records: BTreeMap::new(),
            bonds: [(v, bond)].into_iter().collect(),
            effect_ranges: BTreeMap::new(),
        }
    }

    /// L30-F2: two validations settled from the same snapshot (each thinks
    /// the validator still has its whole bond) cannot together take more
    /// than the bond: `apply` caps against the live ledger total.
    #[test]
    fn concurrent_settlements_never_slash_past_the_bond() {
        let mut ledger = Ledger::<TestRules>::new();
        let v = [7u8; 20];
        let a = settlement_slashing(&mut ledger, 1, v, 1_000, 600);
        let b = settlement_slashing(&mut ledger, 2, v, 1_000, 600);
        assert!(ledger.apply(&a));
        assert!(ledger.apply(&b));
        assert_eq!(ledger.validator_slashed[&v], 1_000);
        assert_eq!(ledger.balance(&v), -1_000);
        // A third, fully capped slash adds no entry at all.
        let c = settlement_slashing(&mut ledger, 3, v, 1_000, 600);
        let before = ledger.entries().len();
        assert!(ledger.apply(&c));
        assert_eq!(ledger.entries().len(), before);
    }

    /// L30-F4: no-valid-root work is held (taken) until its challenge window
    /// closes, and longer while a dispute on it is open.
    #[test]
    fn no_valid_root_work_is_held_until_no_dispute_can_change_it() {
        let mut ledger = Ledger::<TestRules>::new();
        let work = keccak256(b"work");
        let shard = keccak256(b"shard");
        ledger.held.insert(work, (500, 1, shard));
        assert!(ledger.taken().contains(&work));
        ledger.release_held(499);
        assert!(ledger.taken().contains(&work));
        ledger
            .open_disputes
            .insert((1, shard), keccak256(b"dispute"));
        ledger.release_held(600);
        assert!(
            ledger.taken().contains(&work),
            "held while a dispute is open"
        );
        ledger.open_disputes.clear();
        // A pending rerun keeps it too (M5-L46-F1).
        ledger.rerun_required.insert((1, shard));
        ledger.release_held(600);
        assert!(
            ledger.taken().contains(&work),
            "held while a rerun is pending"
        );
        ledger.rerun_required.clear();
        ledger.release_held(600);
        assert!(!ledger.taken().contains(&work));
    }

    #[test]
    fn sampling_rate_is_respected() {
        let seed = keccak256(b"seed");
        let shard = keccak256(b"shard");
        let n = 20_000u32;
        let hits = (0..n)
            .filter(|i| {
                let mut v = [0u8; 20];
                v[..4].copy_from_slice(&i.to_be_bytes());
                assigned(&seed, &v, &shard, &[1; 20], 250_000)
            })
            .count();
        // 25% of 20,000 = 5,000; a 4-sigma band is about +/- 245.
        assert!((4_755..=5_245).contains(&hits), "{hits}");
        assert!(assigned(&seed, &[0; 20], &shard, &[1; 20], PPM));
        assert!(!assigned(&seed, &[0; 20], &shard, &[1; 20], 0));
        // Never one's own reveal.
        assert!(!assigned(&seed, &[1; 20], &shard, &[1; 20], PPM));
    }

    #[test]
    fn deterrence_condition() {
        assert!(deterrence_holds(1_000, 100, 100_000));
        assert!(!deterrence_holds(999, 100, 100_000));
        // L22-F12: overflow is not "deters".
        assert!(!deterrence_holds(u128::MAX, u128::MAX / 2, 2));
    }

    #[test]
    fn canary_list_must_be_strictly_sorted() {
        let a = Canary {
            shard: keccak256(b"s"),
            miner: [1; 20],
            root: keccak256(b"r1"),
        };
        let mut b = a;
        b.miner = [2; 20];
        let salt = keccak256(b"salt");
        assert!(canary_commit(&[a, b], &salt).is_some());
        assert_eq!(canary_commit(&[b, a], &salt), None);
        // L22-F3: one (shard, miner) twice is refused.
        let mut dup = a;
        dup.root = keccak256(b"r2");
        assert_eq!(canary_commit(&[a, dup], &salt), None);
    }
    /// Mutation L4: validator deterrence needs every one of its conditions;
    /// each fails here on its own, and each bound holds at its edge.
    #[test]
    fn validators_deterred_needs_every_condition() {
        let ok = ValidationConfig {
            verdict_commit_close: 0,
            verdict_reveal_close: 0,
            canary_open_close: 0,
            pending_close: 0,
            rate_ppm: 0,
            miner_reward: 0,
            validator_fee: 10,
            validator_slash: 1_000,
            missed_slash: 1_000,
            // 1,000 x 1% = 10: a blind Agree loses exactly what it earns.
            min_canary_ppm: 10_000,
        };
        assert!(ok.validators_deterred());
        let with = |f: &dyn Fn(&mut ValidationConfig)| {
            let mut c = ok;
            f(&mut c);
            c.validators_deterred()
        };
        // Exactly one condition fails in each.
        assert!(!with(&|c| c.missed_slash = 0));
        assert!(!with(&|c| c.missed_slash = 1_001));
        assert!(!with(&|c| {
            c.validator_fee = 0;
            c.min_canary_ppm = 0;
        }));
        assert!(!with(&|c| c.min_canary_ppm = PPM + 1));
        assert!(!with(&|c| c.validator_fee = 11));
        // Edges that hold.
        assert!(with(&|c| c.missed_slash = 1));
        assert!(with(&|c| {
            c.validator_fee = 0;
            c.min_canary_ppm = 1;
        }));
        assert!(with(&|c| c.min_canary_ppm = PPM));
    }
    /// Mutation L4: the bond cap is for validator slashes only; a miner's
    /// slash (no validator bond) is applied in full.
    #[test]
    fn a_miner_slash_is_not_capped_by_validator_bonds() {
        let mut ledger = Ledger::<TestRules>::new();
        let m = [8u8; 20];
        let mut s = settlement_slashing(&mut ledger, 1, m, 0, 700);
        s.entries = vec![LedgerEntry::Slash {
            round: 1,
            actor: m,
            amount: 700,
            reason: "revealed a wrong root",
        }];
        s.bonds.clear();
        assert!(ledger.apply(&s));
        assert_eq!(ledger.balance(&m), -700);
    }

    /// Mutation L4: held work is released at the end of its window (the
    /// height itself), not one block later.
    #[test]
    fn held_work_is_released_at_its_height() {
        let mut ledger = Ledger::<TestRules>::new();
        let work = keccak256(b"work");
        ledger.held.insert(work, (500, 1, keccak256(b"shard")));
        ledger.release_held(500);
        assert!(!ledger.taken().contains(&work));
    }

    /// Mutation L4: a contract mirror's ledger id is the contract's
    /// (`evm_ledger_id`), not the default.
    #[test]
    fn a_contract_mirror_has_the_contracts_ledger_id() {
        let c = [3u8; 20];
        let mirror = Ledger::<TestRules>::mirror_of_contract(84_532, &c);
        assert_eq!(mirror.id(), onyx_core::evm_ledger_id(84_532, &c));
        assert_ne!(mirror.id(), Ledger::<TestRules>::new().id());
    }

    // ---- ROADMAP E8 (THREATS MUT-1): settlement outcomes on real rounds ----
    //
    // The fixture (`crate::test_fixture`) opens, commits, reveals and closes a
    // real round of one shard in milliseconds; its recipe's rows do not
    // depend on the input, so the rerun reference is the honest root.

    use crate::round::RoundConfig;
    use crate::test_fixture::{
        actors, canary_salt, signed_verdict, verdict_salt, Actor, Closed, Plays, Rules, World, CFG,
        ROUND,
    };
    use Plays::{Honest, Silent, Wrong};

    /// Source bytes available to reruns.
    fn source(_: &ShardSpec) -> Option<Vec<u8>> {
        Some(crate::test_fixture::SOURCE.to_vec())
    }

    /// Source bytes withheld: reruns are undecided.
    fn withheld(_: &ShardSpec) -> Option<Vec<u8>> {
        None
    }

    /// Settle with the empty canary list the fixture committed to.
    fn settle(
        c: &Closed,
        val: &mut Validation<Rules>,
        now: u64,
        src: &dyn Fn(&ShardSpec) -> Option<Vec<u8>>,
    ) -> Result<Settlement, ValidationError> {
        val.settle(now, Some((&[], &canary_salt())), &c.w.env(src))
    }

    /// Entries as sorted `(reward?, actor, amount, reason)` tuples.
    fn tally(entries: &[LedgerEntry]) -> Vec<(bool, MinerAddress, u128, &'static str)> {
        let mut t: Vec<_> = entries
            .iter()
            .map(|e| match e {
                LedgerEntry::Reward {
                    actor,
                    amount,
                    reason,
                    ..
                } => (true, *actor, *amount, *reason),
                LedgerEntry::Slash {
                    actor,
                    amount,
                    reason,
                    ..
                } => (false, *actor, *amount, *reason),
            })
            .collect();
        t.sort_unstable();
        t
    }

    fn reward(
        a: &Actor,
        amount: u128,
        reason: &'static str,
    ) -> (bool, MinerAddress, u128, &'static str) {
        (true, a.addr, amount, reason)
    }

    fn slash(
        a: &Actor,
        amount: u128,
        reason: &'static str,
    ) -> (bool, MinerAddress, u128, &'static str) {
        (false, a.addr, amount, reason)
    }

    fn sorted<T: Ord>(mut v: Vec<T>) -> Vec<T> {
        v.sort_unstable();
        v
    }

    #[test]
    fn seed_draw_voids_at_each_edge() {
        // Verdict window: at + 20 <= verdict_commit_close.
        assert!(!seed_draw_voids(201, 300, 280));
        assert!(seed_draw_voids(201, 300, 281));
        // Beacon window: at - beacon <= 256.
        assert!(!seed_draw_voids(201, 1_000, 457));
        assert!(seed_draw_voids(201, 1_000, 458));
        // Before the beacon nothing has lapsed.
        assert!(!seed_draw_voids(201, 1_000, 100));
    }

    #[test]
    fn validator_set_rules() {
        let mut set = ValidatorSet::new();
        assert_eq!(
            set.add([1; 20], 99, 100),
            Err(ValidationError::BadValidator)
        );
        assert_eq!(set.add([1; 20], 100, 100), Ok(()));
        assert_eq!(
            set.add([1; 20], 500, 100),
            Err(ValidationError::BadValidator)
        );
        assert!(set.contains(&[1; 20]) && !set.contains(&[2; 20]));
        assert_eq!(set.bond(&[1; 20]), 100);
        assert_eq!(set.bond(&[2; 20]), 0);
        let empty_root = ValidatorSet::new().root();
        assert_ne!(set.root(), empty_root);
        let mut other = ValidatorSet::new();
        other.add([1; 20], 101, 0).expect("add");
        assert_ne!(set.root(), other.root(), "the root binds the bond");
        assert_eq!(
            sample_seed(&Hash32([1; 32]), &Hash32([2; 32])),
            keccak256(&[b"onyx-seed-evm-v1".as_slice(), &[1; 32], &[2; 32]].concat())
        );
    }

    /// The verdict rule shared with the contract (`OnyxDisputes.verdictRight`).
    #[test]
    fn verdict_right_table() {
        let (mine, other, re) = (Hash32([1; 32]), Hash32([2; 32]), Hash32([1; 32]));
        let dis = |root| Verdict::Disagree { root };
        assert_eq!(verdict_right(&Verdict::Abstain, &mine, Some(&re)), None);
        assert_eq!(verdict_right(&Verdict::Abstain, &mine, None), None);
        assert_eq!(
            verdict_right(&Verdict::NoValidRoot, &mine, None),
            Some(true)
        );
        assert_eq!(
            verdict_right(&Verdict::NoValidRoot, &mine, Some(&re)),
            Some(false)
        );
        assert_eq!(verdict_right(&Verdict::Agree, &mine, None), Some(false));
        assert_eq!(verdict_right(&dis(other), &mine, None), Some(false));
        assert_eq!(verdict_right(&Verdict::Agree, &mine, Some(&re)), Some(true));
        assert_eq!(
            verdict_right(&Verdict::Agree, &other, Some(&re)),
            Some(false)
        );
        // Disagree is right only on a wrong reveal and only naming the reference.
        assert_eq!(verdict_right(&dis(re), &other, Some(&re)), Some(true));
        assert_eq!(
            verdict_right(&dis(Hash32([3; 32])), &other, Some(&re)),
            Some(false)
        );
        assert_eq!(verdict_right(&dis(re), &mine, Some(&re)), Some(false));
        assert!(is_validator_slash("verdict not revealed"));
        assert!(is_validator_slash("panel: no reveal or off the majority"));
        assert!(!is_validator_slash("commit not revealed"));
        assert_eq!(
            entry_amount(&LedgerEntry::Reward {
                round: 1,
                actor: [1; 20],
                amount: 7,
                reason: "x"
            }),
            7
        );
    }

    #[test]
    fn verdict_texts_are_fixed() {
        let (seed, shard, commit) = (Hash32([1; 32]), Hash32([2; 32]), Hash32([3; 32]));
        assert_eq!(
            verdict_message(&seed, &shard, &[0xab; 20], &commit),
            format!(
                "Onyx verdict v2\nseed: {seed}\nshard: {shard}\nminer: 0x{}\ncommit: {commit}",
                "ab".repeat(20)
            )
        );
        let h = |v: &Verdict| verdict_commit_hash(&seed, &[4; 20], &shard, &[5; 20], v, &commit);
        assert_ne!(h(&Verdict::Agree), h(&Verdict::Abstain));
        assert_ne!(
            h(&Verdict::Disagree { root: seed }),
            h(&Verdict::Disagree { root: shard })
        );
    }

    /// Validation starts once, from the ledger that opened the round, in its
    /// window, with the round's validator set and fees the bond covers.
    #[test]
    fn validation_starts_once_in_its_window() {
        let mut c = Closed::new(&[Honest]);
        // The round's set, no other.
        let mut wrong = c.set.clone();
        wrong.add([0xee; 20], 10_000, 0).expect("add");
        assert_eq!(
            Validation::new(&c.round, wrong, keccak256(b"b"), 210, &mut c.ledger).err(),
            Some(ValidationError::BadValidator)
        );
        assert_eq!(
            c.validation_at(200).err(),
            Some(ValidationError::WrongPhase)
        );
        assert_eq!(
            c.validation_at(281).err(),
            Some(ValidationError::SeedTooLate)
        );
        let mut other = Ledger::<Rules>::new();
        assert_eq!(
            Validation::new(&c.round, c.set.clone(), keccak256(b"b"), 210, &mut other).err(),
            Some(ValidationError::NotOpenedHere)
        );
        let val = c
            .validation_at(280)
            .expect("the last height that leaves a verdict window");
        assert_eq!(
            val.seed(),
            sample_seed(&c.round.tag(), &keccak256(b"beacon"))
        );
        assert_eq!(
            c.validation_at(280).err(),
            Some(ValidationError::AlreadyValidating)
        );
        // Committers and validators now have history (L25-F6).
        assert!(c.ledger.known_miners().contains(&c.miners[0].addr));
        assert!(c.ledger.known_miners().contains(&c.validators[0].addr));
        // Rate 100%: every validator checks the one reveal, a miner never its own.
        let task = (c.shard, c.miners[0].addr, c.good);
        assert_eq!(val.tasks_for(&c.validators[3].addr), vec![task]);
        assert!(val.tasks_for(&c.miners[0].addr).is_empty(), "not a member");

        // A round that is not closed.
        let w = World::new();
        let mut l = Ledger::<Rules>::new();
        let open = w.open(1, vec![w.spec(1)], CFG, 10, &mut l).expect("open");
        assert_eq!(
            Validation::new(&open, ValidatorSet::new(), keccak256(b"b"), 210, &mut l).err(),
            Some(ValidationError::WrongPhase)
        );
    }

    /// `min_bond >= validator_fee x members` (review L2-R2), at the bound.
    #[test]
    fn validation_needs_fees_the_bond_covers() {
        let mut cfg = CFG;
        cfg.validation.validator_fee = 100;
        let canary = canary_commit(&[], &canary_salt()).expect("empty");
        let mut ten = Closed::with(&[Honest], actors("v", 10), &[10_000; 10], canary, cfg);
        assert!(ten.validation_at(210).is_ok());
        let mut eleven = Closed::with(&[Honest], actors("v", 11), &[10_000; 11], canary, cfg);
        assert_eq!(
            eleven.validation_at(210).err(),
            Some(ValidationError::FeesExceedBond)
        );
    }

    /// Verdict commits and reveals at each edge of their windows and rules.
    #[test]
    fn verdict_rules_at_each_edge() {
        let mut c = Closed::new(&[Honest, Wrong]);
        let mut val = c.validation();
        let seed = val.seed();
        let (v0, v1) = (&c.validators[0], &c.validators[1]);
        let (m0, m1) = (c.miners[0].addr, c.miners[1].addr);
        let sv = signed_verdict(&seed, v0, &c.shard, &m0, &Verdict::Agree);
        assert_eq!(
            val.commit_verdict(300, &sv),
            Err(ValidationError::WrongPhase)
        );
        let mut forged = sv.clone();
        forged.validator = v1.addr;
        assert_eq!(
            val.commit_verdict(250, &forged),
            Err(ValidationError::BadSignature)
        );
        let outsider = Actor::new("outsider");
        let so = signed_verdict(&seed, &outsider, &c.shard, &m0, &Verdict::Agree);
        assert_eq!(
            val.commit_verdict(250, &so),
            Err(ValidationError::BadValidator)
        );
        let nobody = signed_verdict(&seed, v0, &c.shard, &[0x77; 20], &Verdict::Agree);
        assert_eq!(
            val.commit_verdict(250, &nobody),
            Err(ValidationError::UnknownReveal)
        );
        let stray = signed_verdict(&seed, v0, &Hash32([4; 32]), &m0, &Verdict::Agree);
        assert_eq!(
            val.commit_verdict(250, &stray),
            Err(ValidationError::UnknownReveal)
        );
        assert_eq!(val.commit_verdict(250, &sv), Ok(()));
        assert_eq!(
            val.commit_verdict(299, &sv),
            Err(ValidationError::Duplicate)
        );
        let s1 = signed_verdict(
            &seed,
            v1,
            &c.shard,
            &m1,
            &Verdict::Disagree { root: c.root_of(1) },
        );
        assert_eq!(
            val.commit_verdict(249, &s1),
            Err(ValidationError::TimeWentBack)
        );
        assert_eq!(val.commit_verdict(299, &s1), Ok(()));

        let salt0 = verdict_salt(v0, &c.shard, &m0);
        let reveal = |val: &mut Validation<Rules>, now, v: &Actor, m, verdict, salt| {
            val.reveal_verdict(now, v.addr, c.shard, m, verdict, salt)
        };
        assert_eq!(
            reveal(&mut val, 299, v0, m0, Verdict::Agree, salt0),
            Err(ValidationError::WrongPhase)
        );
        assert_eq!(
            reveal(&mut val, 400, v0, m0, Verdict::Agree, salt0),
            Err(ValidationError::WrongPhase)
        );
        assert_eq!(
            reveal(&mut val, 300, v0, m0, Verdict::Abstain, salt0),
            Err(ValidationError::CommitMismatch)
        );
        assert_eq!(
            reveal(&mut val, 300, v0, m0, Verdict::Agree, Hash32([1; 32])),
            Err(ValidationError::CommitMismatch)
        );
        assert_eq!(
            reveal(&mut val, 300, v0, m1, Verdict::Agree, salt0),
            Err(ValidationError::CommitMismatch),
            "no commit for this task"
        );
        // A Disagree naming the revealed root is not a disagreement.
        let salt1 = verdict_salt(v1, &c.shard, &m1);
        assert_eq!(
            reveal(
                &mut val,
                300,
                v1,
                m1,
                Verdict::Disagree { root: c.root_of(1) },
                salt1
            ),
            Err(ValidationError::CommitMismatch)
        );
        assert_eq!(reveal(&mut val, 300, v0, m0, Verdict::Agree, salt0), Ok(()));
        assert_eq!(
            reveal(&mut val, 399, v0, m0, Verdict::Agree, salt0),
            Err(ValidationError::Duplicate)
        );
        assert_eq!(
            reveal(&mut val, 299, v0, m0, Verdict::Agree, salt0),
            Err(ValidationError::TimeWentBack)
        );
        // Settled: nothing more.
        settle(&c, &mut val, 400, &source).expect("settle");
        assert_eq!(val.commit_verdict(250, &s1), Err(ValidationError::Settled));
        assert_eq!(
            reveal(&mut val, 399, v1, m1, Verdict::Agree, salt1),
            Err(ValidationError::Settled)
        );
    }

    /// Below 100% some tasks are not assigned, and committing to one is refused.
    #[test]
    fn a_verdict_on_an_unassigned_task_is_refused() {
        let mut cfg = CFG;
        cfg.validation.rate_ppm = 500_000;
        let canary = canary_commit(&[], &canary_salt()).expect("empty");
        let mut c = Closed::with(
            &[Honest, Honest],
            actors("validator", 5),
            &[10_000; 5],
            canary,
            cfg,
        );
        let mut val = c.validation();
        let seed = val.seed();
        let (v, m) = c
            .validators
            .iter()
            .flat_map(|v| c.miners.iter().map(move |m| (v, m)))
            .find(|(v, m)| !assigned(&seed, &v.addr, &c.shard, &m.addr, 500_000))
            .expect("an unassigned task at 50%");
        let sv = signed_verdict(&seed, v, &c.shard, &m.addr, &Verdict::Agree);
        assert_eq!(
            val.commit_verdict(250, &sv),
            Err(ValidationError::NotAssigned)
        );
        assert!(!val.tasks_for(&v.addr).iter().any(|(_, a, _)| *a == m.addr));
    }

    /// Unanimous, undisputed, with a full dispute pool: the first correct
    /// reveal by commit order earns the reward once; a silent committer
    /// forfeits its bond; silent assigned validators are slashed the missed
    /// amount once each.
    #[test]
    fn unanimous_shard_pays_the_first_correct_reveal_once() {
        let mut c = Closed::new(&[Honest, Honest, Silent]);
        let mut val = c.validation();
        assert_eq!(
            settle(&c, &mut val, 399, &withheld).err(),
            Some(ValidationError::WrongPhase)
        );
        // No rerun needed: the source is withheld and the shard still settles.
        let s = settle(&c, &mut val, 400, &withheld).expect("settle");
        assert_eq!(s.round(), ROUND);
        assert!(s.round_level());
        assert_eq!(s.references()[&c.shard], (c.good, SettlePath::Unanimous));
        assert_eq!(s.finalized()[&c.shard], c.good);
        assert!(s.pending().is_empty());
        assert_eq!(
            s.shards().iter().copied().collect::<Vec<_>>(),
            vec![c.shard]
        );
        assert_eq!(
            s.statuses()[&(c.shard, c.miners[0].addr)],
            RevealStatus::Correct
        );
        assert_eq!(
            s.statuses()[&(c.shard, c.miners[1].addr)],
            RevealStatus::Correct
        );
        assert_eq!(s.statuses().len(), 2);
        let mut want = vec![
            reward(&c.miners[0], 500, "first correct reveal"),
            slash(&c.miners[2], 1_002, "commit not revealed"),
        ];
        want.extend(
            c.validators
                .iter()
                .map(|v| slash(v, 100, "assigned task not answered")),
        );
        assert_eq!(tally(s.entries()), sorted(want.clone()));
        assert!(c.ledger.apply(&s));
        assert_eq!(tally(c.ledger.entries()), sorted(want));
        assert!(!c.ledger.apply(&s), "once");
        assert_eq!(c.ledger.balance(&c.miners[0].addr), 500);
        assert_eq!(c.ledger.balance(&c.miners[1].addr), 0);
        assert_eq!(c.ledger.balance(&c.miners[2].addr), -1_002);
        assert_eq!(c.ledger.balance(&c.validators[0].addr), -100);
        let work = c.spec.work_id();
        assert_eq!(c.ledger.finalized()[&work], (ROUND, c.good));
        assert!(!c.ledger.open_work().contains(&work));
        assert!(c.ledger.taken().contains(&work));
        // A later call settles nothing new.
        let again = settle(&c, &mut val, 400, &withheld).expect("again");
        assert!(again.entries().is_empty() && again.shards().is_empty());
        assert!(!again.round_level());
        assert_eq!(
            settle(&c, &mut val, 399, &withheld).err(),
            Some(ValidationError::TimeWentBack)
        );
    }

    /// Reveals that differ are rerun: the wrong one loses its bond, the first
    /// correct one (not the first to commit) is paid; each right verdict earns
    /// one fee per (validator, shard, root), each wrong one is slashed, an
    /// Abstain is a missed task, a verdict committed and never revealed is
    /// slashed once at round level and not also as missed.
    #[test]
    fn a_split_shard_is_rerun_and_everyone_is_judged_by_the_rerun() {
        let mut c = Closed::new(&[Wrong, Honest, Honest]);
        let mut val = c.validation();
        let (good, bad) = (c.good, c.root_of(0));
        c.verdicts(
            &mut val,
            &[
                (0, 0, Verdict::Disagree { root: good }),
                (0, 1, Verdict::Agree),
                (0, 2, Verdict::Agree),
                (1, 0, Verdict::Agree),
                (1, 2, Verdict::NoValidRoot),
                (2, 0, Verdict::Abstain),
            ],
            &[(3, 1, Verdict::Agree)],
        );
        let s = settle(&c, &mut val, 400, &source).expect("settle");
        assert_eq!(s.references()[&c.shard], (good, SettlePath::Recomputed));
        assert_eq!(s.finalized()[&c.shard], good);
        assert_ne!(bad, good);
        assert_eq!(
            s.statuses()[&(c.shard, c.miners[0].addr)],
            RevealStatus::Wrong
        );
        assert_eq!(
            s.statuses()[&(c.shard, c.miners[1].addr)],
            RevealStatus::Correct
        );
        assert_eq!(
            s.statuses()[&(c.shard, c.miners[2].addr)],
            RevealStatus::Correct
        );
        let v = &c.validators;
        let want = vec![
            slash(&c.miners[0], 1_000, "revealed a wrong root"),
            reward(&c.miners[1], 500, "first correct reveal"),
            // v0: right on the wrong root and on the good root, once each.
            reward(&v[0], 10, "correct verdict"),
            reward(&v[0], 10, "correct verdict"),
            // v1: Agree on a wrong root, "no valid root" on a correct one.
            slash(&v[1], 2_000, "wrong verdict"),
            slash(&v[1], 2_000, "wrong verdict"),
            // v2 abstained: unanswered.
            slash(&v[2], 100, "assigned task not answered"),
            // v3 committed, never revealed: round level only.
            slash(&v[3], 2_000, "verdict not revealed"),
            slash(&v[4], 100, "assigned task not answered"),
        ];
        assert_eq!(tally(s.entries()), sorted(want));
        assert!(c.ledger.apply(&s));
        assert_eq!(c.ledger.balance(&v[0].addr), 20);
        assert_eq!(c.ledger.balance(&v[1].addr), -4_000);
        assert_eq!(c.ledger.validator_slashed[&v[1].addr], 4_000);
        assert_eq!(c.ledger.validator_slashed[&v[3].addr], 2_000);
        // The record a dispute reverses holds only the reference-dependent entries.
        let rec = &c.ledger.settled[&(ROUND, c.shard)];
        assert_eq!(rec.path, SettlePath::Recomputed);
        assert_eq!(rec.reference, good);
        assert_eq!(rec.work, c.spec.work_id());
        assert_eq!(rec.settled_at, 400);
        assert_eq!(
            tally(&rec.effects),
            sorted(vec![
                slash(&c.miners[0], 1_000, "revealed a wrong root"),
                reward(&c.miners[1], 500, "first correct reveal"),
                reward(&v[0], 10, "correct verdict"),
                reward(&v[0], 10, "correct verdict"),
                slash(&v[1], 2_000, "wrong verdict"),
                slash(&v[1], 2_000, "wrong verdict"),
            ])
        );
        assert_eq!(rec.verdict_givers.len(), 4);
        assert_eq!(rec.verdicts.len(), 6);
    }

    /// One root, but a validator disputes it: rerun (and the disputer is
    /// wrong). "No valid root" disputes too.
    #[test]
    fn a_disputed_unanimous_shard_is_rerun() {
        for dispute in [
            Verdict::Disagree {
                root: Hash32([9; 32]),
            },
            Verdict::NoValidRoot,
        ] {
            let mut c = Closed::new(&[Honest, Honest]);
            let mut val = c.validation();
            c.verdicts(&mut val, &[(0, 1, dispute)], &[]);
            let s = settle(&c, &mut val, 400, &source).expect("settle");
            assert_eq!(s.references()[&c.shard], (c.good, SettlePath::Recomputed));
            assert!(tally(s.entries()).contains(&slash(&c.validators[0], 2_000, "wrong verdict")));
            assert!(tally(s.entries()).contains(&reward(
                &c.miners[0],
                500,
                "first correct reveal"
            )));
        }
        // An Agree is no dispute.
        let mut c = Closed::new(&[Honest, Honest]);
        let mut val = c.validation();
        c.verdicts(&mut val, &[(0, 1, Verdict::Agree)], &[]);
        let s = settle(&c, &mut val, 400, &withheld).expect("settle");
        assert_eq!(s.references()[&c.shard].1, SettlePath::Unanimous);
    }

    /// A unanimous shard is taken as is only if a panel could still be drawn
    /// to dispute it: more than `panel_size` bonded validators outside its
    /// miners and verdict-givers, each able to stake a panel slash on top of
    /// what was taken, staked and can still be slashed (L29-F4, L30-F3, L31-F2, F3).
    #[test]
    fn unanimity_needs_a_panel_that_could_dispute_it() {
        let canary = canary_commit(&[], &canary_salt()).expect("empty");
        let path = |validators: Vec<Actor>, bonds: &[u128], prep: &dyn Fn(&mut Closed)| {
            let mut c = Closed::with(&[Honest, Honest], validators, bonds, canary, CFG);
            prep(&mut c);
            let mut val = c.validation();
            settle(&c, &mut val, 400, &source)
                .expect("settle")
                .references()[&c.shard]
                .1
        };
        let none = |_: &mut Closed| {};
        // panel_size 3: four free validators are a pool, three are not.
        assert_eq!(
            path(actors("v", 4), &[10_000; 4], &none),
            SettlePath::Unanimous
        );
        assert_eq!(
            path(actors("v", 3), &[10_000; 3], &none),
            SettlePath::Recomputed
        );
        // A miner in the set is not in the pool.
        let mut with_miner = actors("v", 4);
        with_miner[3] = Actor::new("miner0");
        assert_eq!(
            path(with_miner, &[10_000; 4], &none),
            SettlePath::Recomputed
        );
        // A validator that can still lose 100 (silent at 100%) needs 600 to sit.
        assert_eq!(
            path(actors("v", 4), &[10_000, 10_000, 10_000, 600], &none),
            SettlePath::Unanimous
        );
        assert_eq!(
            path(actors("v", 4), &[10_000, 10_000, 10_000, 599], &none),
            SettlePath::Recomputed
        );
        // What the ledger already took, or open panels stake, counts too.
        let low = Actor::new("v3").addr;
        assert_eq!(
            path(actors("v", 4), &[10_000, 10_000, 10_000, 601], &|c| {
                c.ledger.validator_slashed.insert(low, 1);
            }),
            SettlePath::Unanimous
        );
        assert_eq!(
            path(actors("v", 4), &[10_000, 10_000, 10_000, 601], &|c| {
                c.ledger.validator_slashed.insert(low, 2);
            }),
            SettlePath::Recomputed
        );
        assert_eq!(
            path(actors("v", 4), &[10_000, 10_000, 10_000, 601], &|c| {
                c.ledger.panel_exposure.insert(low, 2);
            }),
            SettlePath::Recomputed
        );
    }

    /// A verdict-giver is not in the dispute pool, and a verdict committed
    /// and never revealed counts against the giver's free bond.
    #[test]
    fn verdict_givers_leave_the_pool() {
        // Five validators, one gives a verdict: four free, a pool.
        let mut c = Closed::new(&[Honest, Honest]);
        let mut val = c.validation();
        c.verdicts(&mut val, &[(0, 0, Verdict::Agree)], &[]);
        let s = settle(&c, &mut val, 400, &withheld).expect("settle");
        assert_eq!(s.references()[&c.shard].1, SettlePath::Unanimous);
        // Two give verdicts: three free, no pool.
        let mut c = Closed::new(&[Honest, Honest]);
        let mut val = c.validation();
        c.verdicts(
            &mut val,
            &[(0, 0, Verdict::Agree)],
            &[(1, 1, Verdict::Agree)],
        );
        let s = settle(&c, &mut val, 400, &source).expect("settle");
        assert_eq!(s.references()[&c.shard].1, SettlePath::Recomputed);
    }

    /// A shard whose rerun cannot be decided (source withheld) waits; at
    /// `pending_close` it is voided: no rewards, no slashes, and its work is
    /// burned because its reveals are public.
    #[test]
    fn undecided_shards_wait_then_are_voided() {
        let mut c = Closed::new(&[Honest, Wrong, Silent]);
        let mut val = c.validation();
        c.verdicts(&mut val, &[], &[(0, 0, Verdict::Agree)]);
        let first = settle(&c, &mut val, 400, &withheld).expect("settle");
        assert_eq!(
            first.pending().iter().copied().collect::<Vec<_>>(),
            vec![c.shard]
        );
        assert!(first.shards().is_empty() && first.references().is_empty());
        assert_eq!(
            first.statuses()[&(c.shard, c.miners[0].addr)],
            RevealStatus::Pending
        );
        assert_eq!(
            first.statuses()[&(c.shard, c.miners[1].addr)],
            RevealStatus::Pending
        );
        // Round-level entries come with the first call.
        assert_eq!(
            tally(first.entries()),
            sorted(vec![
                slash(&c.miners[2], 1_002, "commit not revealed"),
                slash(&c.validators[0], 2_000, "verdict not revealed"),
            ])
        );
        assert!(c.ledger.apply(&first));
        let work = c.spec.work_id();
        assert!(c.ledger.open_work().contains(&work), "still open");
        let still = settle(&c, &mut val, 599, &withheld).expect("settle");
        assert_eq!(still.pending().len(), 1);
        assert!(still.entries().is_empty() && !still.round_level());
        let void = settle(&c, &mut val, 600, &withheld).expect("settle");
        assert_eq!(
            void.references()[&c.shard],
            (Hash32::ZERO, SettlePath::Voided)
        );
        assert_eq!(
            void.statuses()[&(c.shard, c.miners[0].addr)],
            RevealStatus::Voided
        );
        assert_eq!(
            void.statuses()[&(c.shard, c.miners[1].addr)],
            RevealStatus::Voided
        );
        assert!(void.entries().is_empty() && void.finalized().is_empty());
        assert!(void.pending().is_empty());
        assert_eq!(void.shards().len(), 1);
        assert!(c.ledger.apply(&void));
        assert!(!c.ledger.open_work().contains(&work));
        assert!(!c.ledger.finalized().contains_key(&work));
        assert!(c.ledger.taken().contains(&work), "burned");
        assert_eq!(c.ledger.balance(&c.miners[1].addr), 0);
    }

    /// Inputs that arrive before `pending_close` decide a pending shard.
    #[test]
    fn a_pending_shard_settles_when_its_inputs_arrive() {
        let mut c = Closed::new(&[Honest, Wrong]);
        let mut val = c.validation();
        let first = settle(&c, &mut val, 400, &withheld).expect("settle");
        assert_eq!(first.pending().len(), 1);
        assert!(c.ledger.apply(&first));
        let later = settle(&c, &mut val, 599, &source).expect("settle");
        assert_eq!(
            later.references()[&c.shard],
            (c.good, SettlePath::Recomputed)
        );
        assert!(!later.round_level());
        assert!(c.ledger.apply(&later));
        assert_eq!(c.ledger.balance(&c.miners[0].addr), 500);
        assert_eq!(c.ledger.balance(&c.miners[1].addr), -1_001);
        assert_eq!(c.ledger.finalized()[&c.spec.work_id()], (ROUND, c.good));
    }

    /// The canary list: opened on the first call before `canary_open_close`
    /// (from then on settlement may go without it), it must match its
    /// commitment and the reveals, and name fresh addresses only.
    #[test]
    fn canaries_open_in_time_and_match_fresh_reveals() {
        let salt = keccak256(b"canaries");
        let build = |plays: &[Plays], list: &dyn Fn(&Closed) -> Vec<Canary>| {
            // The list depends on the shard and roots: build once to learn them.
            let probe = Closed::new(plays);
            let l = list(&probe);
            let commit = canary_commit(&l, &salt).expect("sorted");
            let c = Closed::with(plays, actors("validator", 5), &[10_000; 5], commit, CFG);
            (c, l)
        };
        let one = |c: &Closed| {
            vec![Canary {
                shard: c.shard,
                miner: c.miners[0].addr,
                root: c.root_of(0),
            }]
        };
        let (mut c, list) = build(&[Honest, Honest], &one);
        let mut val = c.validation();
        let env_src =
            |val: &mut Validation<Rules>, now, arg| val.settle(now, arg, &c.w.env(&source));
        assert_eq!(
            env_src(&mut val, 400, None).err(),
            Some(ValidationError::WrongPhase),
            "before canary_open_close the list must be opened"
        );
        assert_eq!(
            env_src(&mut val, 400, Some((&list, &Hash32([1; 32])))).err(),
            Some(ValidationError::CanaryMismatch)
        );
        let mut off = list.clone();
        off[0].root = Hash32([3; 32]);
        let off_commit = canary_commit(&off, &salt).expect("sorted");
        let mut c2 = Closed::with(
            &[Honest, Honest],
            actors("validator", 5),
            &[10_000; 5],
            off_commit,
            CFG,
        );
        let mut val2 = c2.validation();
        assert_eq!(
            val2.settle(400, Some((&off, &salt)), &c2.w.env(&source))
                .err(),
            Some(ValidationError::CanaryMismatch),
            "a canary must match a reveal"
        );
        let s = env_src(&mut val, 400, Some((&list, &salt))).expect("settle");
        // A canary makes the shard a rerun; it earns nothing; the first
        // correct non-canary reveal is paid.
        assert_eq!(s.references()[&c.shard], (c.good, SettlePath::Recomputed));
        assert_eq!(
            s.statuses()[&(c.shard, c.miners[0].addr)],
            RevealStatus::Canary
        );
        assert_eq!(
            s.statuses()[&(c.shard, c.miners[1].addr)],
            RevealStatus::Correct
        );
        assert!(tally(s.entries()).contains(&reward(&c.miners[1], 500, "first correct reveal")));
        assert!(!tally(s.entries()).iter().any(|e| e.1 == c.miners[0].addr));
        assert!(c.ledger.apply(&s));
        assert_eq!(c.ledger.balance(&c.miners[0].addr), 0);
    }

    /// Validators are judged on canaries like any reveal; a right verdict on
    /// a canary earns one fee per (validator, shard), apart from the fee on
    /// the real root; a canary with a wrong root is not slashed, an Agree on
    /// it is.
    #[test]
    fn validators_are_judged_on_canaries() {
        let salt = keccak256(b"canaries");
        let probe = Closed::new(&[Wrong, Honest]);
        let list = vec![Canary {
            shard: probe.shard,
            miner: probe.miners[0].addr,
            root: probe.root_of(0),
        }];
        let commit = canary_commit(&list, &salt).expect("sorted");
        let mut c = Closed::with(
            &[Wrong, Honest],
            actors("validator", 5),
            &[10_000; 5],
            commit,
            CFG,
        );
        let mut val = c.validation();
        c.verdicts(
            &mut val,
            &[
                (0, 0, Verdict::Disagree { root: c.good }),
                (0, 1, Verdict::Agree),
                (1, 0, Verdict::Agree),
            ],
            &[],
        );
        let s = val
            .settle(400, Some((&list, &salt)), &c.w.env(&source))
            .expect("settle");
        let t = tally(s.entries());
        assert_eq!(
            s.statuses()[&(c.shard, c.miners[0].addr)],
            RevealStatus::Canary
        );
        assert!(
            !t.iter().any(|e| e.1 == c.miners[0].addr),
            "a canary is never slashed"
        );
        assert_eq!(
            t.iter().filter(|e| e.1 == c.validators[0].addr).count(),
            2,
            "a canary fee and a real fee"
        );
        assert!(t.contains(&slash(&c.validators[1], 2_000, "wrong verdict")));
        assert!(t.contains(&reward(&c.miners[1], 500, "first correct reveal")));
        let rec = &s.records[&c.shard];
        assert_eq!(rec.canaries, [c.miners[0].addr].into_iter().collect());
    }

    /// From `canary_open_close` on, settlement may go without the list; a
    /// canary address with history is refused.
    #[test]
    fn canaries_are_optional_late_and_must_be_fresh() {
        let mut c = Closed::new(&[Honest, Honest]);
        let mut val = c.validation();
        assert_eq!(
            val.settle(449, None, &c.w.env(&withheld)).err(),
            Some(ValidationError::WrongPhase)
        );
        let s = val.settle(450, None, &c.w.env(&withheld)).expect("settle");
        assert_eq!(s.references()[&c.shard].1, SettlePath::Unanimous);
        assert!(c.ledger.apply(&s));

        let salt = keccak256(b"canaries");
        let probe = Closed::new(&[Honest, Honest]);
        let list = vec![Canary {
            shard: probe.shard,
            miner: probe.miners[0].addr,
            root: probe.root_of(0),
        }];
        let commit = canary_commit(&list, &salt).expect("sorted");
        let mut c = Closed::with(
            &[Honest, Honest],
            actors("validator", 5),
            &[10_000; 5],
            commit,
            CFG,
        );
        c.ledger.record_known([c.miners[0].addr]);
        let mut val = c.validation();
        assert_eq!(
            val.settle(400, Some((&list, &salt)), &c.w.env(&source))
                .err(),
            Some(ValidationError::CanaryMismatch)
        );
    }

    /// Voided work is burned only if a real reveal made a root public; a
    /// shard whose only reveals are canaries is released (L27-F3).
    #[test]
    fn a_canary_only_voided_shard_is_released() {
        let salt = keccak256(b"canaries");
        let probe = Closed::new(&[Honest]);
        let list = vec![Canary {
            shard: probe.shard,
            miner: probe.miners[0].addr,
            root: probe.root_of(0),
        }];
        let commit = canary_commit(&list, &salt).expect("sorted");
        let mut c = Closed::with(&[Honest], actors("validator", 5), &[10_000; 5], commit, CFG);
        let mut val = c.validation();
        let s = val
            .settle(600, Some((&list, &salt)), &c.w.env(&withheld))
            .expect("settle");
        assert_eq!(s.references()[&c.shard].1, SettlePath::Voided);
        assert!(s.burned.is_empty());
        assert!(c.ledger.apply(&s));
        assert!(!c.ledger.taken().contains(&c.spec.work_id()));
    }

    /// A validator's slashes in one validation stop at its bond, counting
    /// what the ledger took before; a fully capped slash adds no entry.
    #[test]
    fn validator_slashes_stop_at_the_bond() {
        let canary = canary_commit(&[], &canary_salt()).expect("empty");
        let mut c = Closed::with(
            &[Wrong, Wrong, Honest],
            actors("validator", 5),
            &[10_000, 10_000, 10_000, 10_000, 5_000],
            canary,
            CFG,
        );
        let poor = c.validators[4].addr;
        c.ledger.validator_slashed.insert(poor, 500);
        let mut val = c.validation();
        // Two wrong verdicts (2,000 each) and an unrevealed one (2,000):
        // 4,500 of bond left, so 2,000 + 2,000 + 500.
        c.verdicts(
            &mut val,
            &[(4, 0, Verdict::Agree), (4, 1, Verdict::Agree)],
            &[(4, 2, Verdict::Agree)],
        );
        let s = settle(&c, &mut val, 400, &source).expect("settle");
        let mine: Vec<u128> = s
            .entries()
            .iter()
            .filter_map(|e| match e {
                LedgerEntry::Slash { actor, amount, .. } if *actor == poor => Some(*amount),
                _ => None,
            })
            .collect();
        assert_eq!(sorted(mine), vec![500, 2_000, 2_000]);
        assert!(s.entries().iter().all(|e| entry_amount(e) > 0));
        assert!(c.ledger.apply(&s));
        assert_eq!(c.ledger.validator_slashed[&poor], 5_000);
        assert_eq!(c.ledger.balance(&poor), -4_500);

        // Fully slashed before: no entry at all.
        let mut c = Closed::with(
            &[Wrong, Honest],
            actors("validator", 5),
            &[10_000, 10_000, 10_000, 10_000, 5_000],
            canary,
            CFG,
        );
        c.ledger.validator_slashed.insert(poor, 5_000);
        let mut val = c.validation();
        c.verdicts(&mut val, &[(4, 0, Verdict::Agree)], &[]);
        let s = settle(&c, &mut val, 400, &source).expect("settle");
        assert!(!s
            .entries()
            .iter()
            .any(|e| matches!(e, LedgerEntry::Slash { actor, .. } if *actor == poor)));
    }

    /// `apply` takes only this ledger's settlements, once, and holds
    /// no-valid-root work for its challenge window.
    #[test]
    fn apply_takes_its_own_settlements_once() {
        let mut c = Closed::new(&[Wrong, Honest]);
        let mut val = c.validation();
        let s = settle(&c, &mut val, 400, &source).expect("settle");
        let mut stranger = Ledger::<Rules>::new();
        assert!(!stranger.apply(&s));
        assert!(stranger.entries().is_empty());
        // Another rule set's settlement.
        let mut foreign = s.clone();
        foreign.rules = TypeId::of::<TestRules>();
        assert!(!c.ledger.apply(&foreign));
        // Another seed or tag.
        let mut reseeded = s.clone();
        reseeded.seed = Hash32([1; 32]);
        assert!(!c.ledger.apply(&reseeded));
        let mut retagged = s.clone();
        retagged.tag = Hash32([1; 32]);
        assert!(!c.ledger.apply(&retagged));
        // A canary that had history when validation started.
        let mut stale = s.clone();
        stale.canaries.insert(c.validators[0].addr);
        assert!(!c.ledger.apply(&stale));
        // No valid root: held until settled_at + challenge_window.
        let mut nvr = s.clone();
        nvr.records.get_mut(&c.shard).expect("record").path = SettlePath::NoValidRoot;
        assert!(c.ledger.apply(&nvr));
        let work = c.spec.work_id();
        assert_eq!(c.ledger.held[&work], (600, ROUND, c.shard));
        assert!(!c.ledger.apply(&s), "already applied");
        // Round level: the round's committers are known.
        assert!(c.ledger.known_miners().contains(&c.miners[1].addr));
    }

    /// `apply` keeps the first finalized root of a work, and burns voided
    /// work only from the settlement's own list.
    #[test]
    fn apply_records_work_from_the_settlement() {
        let mut c = Closed::new(&[Honest]);
        let mut val = c.validation();
        let s = settle(&c, &mut val, 400, &withheld).expect("settle");
        let work = c.spec.work_id();
        c.ledger.finalized.insert(work, (0, Hash32([5; 32])));
        assert!(c.ledger.apply(&s));
        assert_eq!(
            c.ledger.finalized()[&work],
            (0, Hash32([5; 32])),
            "first root kept"
        );
        assert!(!c.ledger.burned.contains(&work));
        assert!(c.ledger.held.is_empty());
    }

    /// A validator slash `apply` caps to zero (the ledger took the bond
    /// meanwhile) leaves no entry, and no effect in the shard's record.
    #[test]
    fn a_slash_capped_to_zero_leaves_no_entry_and_no_effect() {
        let mut c = Closed::new(&[Wrong, Honest]);
        let mut val = c.validation();
        c.verdicts(
            &mut val,
            &[(1, 0, Verdict::Agree), (0, 1, Verdict::Agree)],
            &[],
        );
        let s = settle(&c, &mut val, 400, &source).expect("settle");
        let v1 = c.validators[1].addr;
        assert!(tally(s.entries()).contains(&slash(&c.validators[1], 2_000, "wrong verdict")));
        // Another validation took v1's whole bond before this one is applied.
        c.ledger.validator_slashed.insert(v1, 10_000);
        assert!(c.ledger.apply(&s));
        assert!(c.ledger.entries().iter().all(|e| entry_amount(e) > 0));
        assert_eq!(c.ledger.balance(&v1), 0);
        let rec = &c.ledger.settled[&(ROUND, c.shard)];
        assert!(!rec
            .effects
            .iter()
            .any(|e| matches!(e, LedgerEntry::Slash { actor, .. } if *actor == v1)));
        assert_eq!(
            rec.effects.len(),
            3,
            "the miner slash, the reward and v0's fee"
        );
    }

    /// A registry whose address rules are not this binary's built-in ones
    /// is never used for a rerun: a split shard stays pending (L25-F2).
    #[test]
    fn reruns_need_the_built_in_rules() {
        let canary = canary_commit(&[], &canary_salt()).expect("empty");
        let mut c = Closed::in_world(
            World::with_foreign_canon(),
            &[Wrong, Honest],
            actors("validator", 5),
            &[10_000; 5],
            canary,
            CFG,
        );
        let mut val = c.validation();
        let s = settle(&c, &mut val, 400, &source).expect("settle");
        assert_eq!(
            s.pending().iter().copied().collect::<Vec<_>>(),
            vec![c.shard]
        );
        // A unanimous shard needs no rerun, so no rules.
        let mut c = Closed::in_world(
            World::with_foreign_canon(),
            &[Honest, Honest],
            actors("validator", 5),
            &[10_000; 5],
            canary,
            CFG,
        );
        let mut val = c.validation();
        let s = settle(&c, &mut val, 400, &source).expect("settle");
        assert_eq!(s.references()[&c.shard].1, SettlePath::Unanimous);
    }

    /// Two shards in one round: a dispute on one is no dispute on the other,
    /// each record holds its own verdicts only, and a verdict committed and
    /// never revealed counts against its giver's free bond in every shard's
    /// dispute pool.
    #[test]
    fn shards_of_one_round_are_judged_apart() {
        // Shard A: two honest reveals. Shard B: an honest and a wrong one.
        // v0 disputes B; v1 commits a verdict on B it never reveals (2,000 at
        // risk) and is silent on A (100 at risk). Four validators, panel 3:
        // A is unanimous only if all four can sit, so v1 needs 2,100 + 500.
        let run = |v1_bond: u128| {
            let w = World::new();
            let mut ledger = Ledger::<Rules>::new();
            let (a, b) = (w.spec(1), w.spec(2));
            let (sa, sb) = (a.shard_id(), b.shard_id());
            let m = actors("miner", 4);
            let v = actors("validator", 4);
            let mut set = ValidatorSet::new();
            for (i, x) in v.iter().enumerate() {
                set.add(x.addr, if i == 1 { v1_bond } else { 10_000 }, 0)
                    .expect("validator");
            }
            let cfg = RoundConfig {
                validator_set_root: set.root(),
                canary_commit: canary_commit(&[], &canary_salt()).expect("empty"),
                ..CFG
            };
            let mut round = w
                .open(ROUND, vec![a.clone(), b.clone()], cfg, 10, &mut ledger)
                .expect("open");
            let works = [
                (sa, w.honest(&a, ROUND, &m[0])),
                (sa, w.honest(&a, ROUND, &m[1])),
                (sb, w.honest(&b, ROUND, &m[2])),
                (sb, w.wrong(&b, ROUND, &m[3], "wrong b")),
            ];
            for (i, (sh, wk)) in works.iter().enumerate() {
                let c = crate::test_fixture::signed_commit(&round, sh, &m[i], wk, 1_000);
                round.commit(50, &c, &mut ledger).expect("commit");
            }
            for (i, (sh, wk)) in works.iter().enumerate() {
                round
                    .reveal(150, m[i].addr, *sh, wk.commitment.clone(), wk.salt, &w.cas)
                    .expect("reveal");
            }
            round.close(200).expect("close");
            let mut val =
                Validation::<Rules>::new(&round, set, keccak256(b"beacon"), 210, &mut ledger)
                    .expect("validation");
            let seed = val.seed();
            let good_b = w.honest_root(&b);
            let d = signed_verdict(
                &seed,
                &v[0],
                &sb,
                &m[3].addr,
                &Verdict::Disagree { root: good_b },
            );
            val.commit_verdict(250, &d).expect("commit");
            let u = signed_verdict(&seed, &v[1], &sb, &m[2].addr, &Verdict::Agree);
            val.commit_verdict(250, &u).expect("commit");
            val.reveal_verdict(
                350,
                v[0].addr,
                sb,
                m[3].addr,
                Verdict::Disagree { root: good_b },
                verdict_salt(&v[0], &sb, &m[3].addr),
            )
            .expect("reveal");
            let first = val
                .settle(400, Some((&[], &canary_salt())), &w.env(&withheld))
                .expect("settle");
            (w, val, first, sa, sb)
        };
        // v1 can sit: A is unanimous; B is disputed and waits for its inputs.
        let (_, _, first, sa, sb) = run(2_600);
        assert_eq!(first.references()[&sa].1, SettlePath::Unanimous);
        assert!(first.records[&sa].verdicts.is_empty());
        assert!(first.records[&sa].verdict_givers.is_empty());
        assert_eq!(
            first.pending().iter().copied().collect::<Vec<_>>(),
            vec![sb]
        );
        // One less: no pool for A, which must be rerun too.
        let (w, mut val, first, sa, sb) = run(2_599);
        assert_eq!(first.pending().len(), 2);
        let later = val.settle(401, None, &w.env(&source)).expect("settle");
        assert_eq!(later.references()[&sa].1, SettlePath::Recomputed);
        assert_eq!(later.references()[&sb].1, SettlePath::Recomputed);
        assert!(later.records[&sa].verdicts.is_empty());
        assert_eq!(later.records[&sb].verdicts.len(), 1);
    }
}
