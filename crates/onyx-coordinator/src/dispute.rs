//! Disputes (M5, ADR-0021): a bonded challenge against a settled shard,
//! decided by a randomly drawn panel of bonded validators who each rerun the
//! shard from its pinned inputs and commit, then reveal, the root they get.
//!
//! 1. Anyone may challenge a shard settled with a reference within
//!    `challenge_window` heights of its settlement, with a bond of at least
//!    `min_challenge_bond`, naming the root it claims is right (it must
//!    differ from the reference). One open dispute per (round, shard). The
//!    challenge is signed over the round tag, which carries the ledger id, so
//!    it opens only in the ledger that settled the shard.
//! 2. Panel: `panel_size` members of the round's bonded validator set, drawn
//!    by sorting candidates on `keccak256("onyx-panel-v1" ‖ seed ‖ dispute id
//!    ‖ level ‖ beacon ‖ address)`. Never drawn: the shard's miners, the
//!    challenger and appellants, validators who committed a verdict on the
//!    shard, and members of earlier panels of the dispute.
//! 3. Members commit, then reveal, a root bound to the dispute and level,
//!    signed EIP-191. Honest members rerun with [`honest_panel_root`].
//! 4. A panel that is not unanimous (a missing reveal or two roots) may be
//!    appealed once its reveals close and before `appeal_window` ends, with a
//!    bond of at least `min_challenge_bond * 2^level` and at least the next
//!    panel's fees (`DisputeConfig::appeal_floor`, M6-L2-F3), naming the root
//!    the appellant backs. The new panel has exactly
//!    `2 * previous + 1` members, drawn from those not yet drawn; if that many
//!    are not available the appeal sends the shard to a rerun, which decides
//!    (L29-F2, q13 a, M5-L46-F2). At most `max_appeals` rounds.
//! 5. The last panel decides by strict majority of its members (missing
//!    reveals count against every root):
//!    - the reference: upheld; the challenger loses its bond, half to the
//!      first correct miner, half burned;
//!    - the challenger's root: overturned; every ledger entry the old
//!      reference caused is reversed by a compensating entry (never edited),
//!      miners and validators are judged again against the new root, and the
//!      challenger keeps its bond and earns half of the new miner slashes;
//!    - another root: corrected to it as above, but the challenger loses its
//!      bond (its claim was wrong too);
//!    - no majority: not a final answer (PLAN q13 (a), loop 46). Every
//!      funder pays its panels' fees, burned (L30-F1); nobody on any panel
//!      is slashed, for silence or for a reveal (L31-F1); the shard is
//!      marked "rerun required" and the dispute waits
//!      ([`DisputeOutcome::RerunRequired`]). The settler's rerun from the
//!      pinned inputs ([`Ledger::rerun_shard`]) then decides it as a panel
//!      majority would: Upheld, Overturned or Corrected, with the same
//!      payouts and the same losses for funders backing another root. The
//!      panel is only the fast path; the rerun is the final arbiter.
//!
//!    Who pays (L29-F3): every panel is funded by the bond of whoever asked
//!    for it (the challenger for the first, each appellant for its appeal).
//!    Members on the final majority earn `panel_fee` out of that bond,
//!    whatever the outcome; nothing is minted. A funder whose root is not the
//!    final majority also loses the rest of its bond. Panel members who did
//!    not reveal, or revealed another root than the final majority, are
//!    slashed `panel_slash` (burned), capped with every other validator slash
//!    at their bond across the whole ledger (L29-F5). Silence is slashed
//!    only on a panel where someone revealed the final majority root: a
//!    panel where nobody did could not rerun, whatever junk a sybil revealed
//!    on it (L33-N4).
//! 7. A signed challenge opens once per ledger (L29-F1). A shard whose dispute
//!    ended Upheld, Overturned or Corrected, or that the settler reran, is
//!    closed to disputes (L29-F6, L33-N3).
//! 8. Voided shards have no reference and pay nobody, so there is nothing to
//!    dispute (L29-F10).
//! 9. A member on an open panel stakes `panel_slash` of its bond until the
//!    dispute resolves: it is drawn only if its bond minus what the ledger
//!    took minus its open exposure still covers one more slash (L31-F2).
//! 10. A challenge that finds no panel does not leave the root final: the
//!     shard is marked "rerun required" and the settler reruns it from its
//!     pinned inputs ([`Ledger::rerun_shard`]), correcting it like a dispute
//!     would (L31-F3, q13 (a)). A rerun that confirms a unanimous root records
//!     it as recomputed (L33-N3).
//! 6. Only [`ProofLevel::Recomputed`] claims can be disputed now; the other
//!    proof levels return [`DisputeError::Unsupported`] (docs/spec/disputes.md).

use crate::recompute::{no_valid_root_marker, RecomputeEnv, Reference};
use crate::validate::{Ledger, LedgerEntry, SettlePath, ValidationConfig, ValidatorSet, Verdict};
use onyx_core::{keccak256, Hash32, MinerAddress, RuleSet, ShardSpec};
use onyx_labels::attest::{eip191_recover, AttestCache};
use std::collections::{BTreeMap, BTreeSet};

/// Most appeal rounds a round config may allow; panels grow to 2k+1 per round.
pub const MAX_APPEALS: u32 = 4;

/// Largest panel a valid config can draw: the panel after the last appeal,
/// `(panel_size + 1) * 2^max_appeals - 1`. Keeps the on-chain draw and its
/// judging inside the 15M-gas budget at 128 validators (M6-L2-F2).
pub const MAX_PANEL: u32 = 63;

/// L29-F4: a validator can sit on a panel when its bond, less what is
/// already taken or at risk (`taken`), still covers `panel_slash`. The
/// contract's `OnyxRound.canSit`.
pub fn can_sit(bond: u128, taken: u128, panel_slash: u128) -> bool {
    bond.saturating_sub(taken) >= panel_slash
}

/// `panel_slash >= panel_fee * LAZY_PANEL_RATIO`. A lazy member who always
/// reveals the settled reference gains `q * fee - (1 - q) * slash` when a
/// share `q` of disputes is frivolous; this ratio keeps that below zero for
/// `q <= 0.9` (L29-F7, an assumption; panel canaries are M6 work).
pub const LAZY_PANEL_RATIO: u128 = 9;

/// Dispute timing and economics, in heights and token units. Part of the
/// round config and its tag: fixed when the round opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisputeConfig {
    /// Challenges accepted while `settled_at <= now < settled_at + challenge_window`.
    pub challenge_window: u64,
    /// Smallest challenge bond.
    pub min_challenge_bond: u128,
    /// First panel size (odd). Each appeal draws `2 * previous + 1`.
    pub panel_size: u32,
    /// Panel commits accepted for this many heights after the panel is drawn.
    pub panel_commit_window: u64,
    /// Panel reveals accepted for this many heights after commits close.
    pub panel_reveal_window: u64,
    /// Appeals accepted for this many heights after a panel's reveals close.
    pub appeal_window: u64,
    /// Most appeal rounds.
    pub max_appeals: u32,
    /// Fee to each panel member on the final majority.
    pub panel_fee: u128,
    /// Slash for a panel member who does not reveal or is not on the final majority.
    pub panel_slash: u128,
}

impl DisputeConfig {
    /// Odd, non-empty panel; non-zero windows, bond and slash; a lazy member
    /// must not profit (`LAZY_PANEL_RATIO`); bounded appeals; and the
    /// smallest challenge bond funds a panel's fees with room to spare,
    /// `min_challenge_bond >= panel_fee * (panel_size + 1)`. With appeal bonds
    /// doubling, every later panel is funded too (L29-F3). The challenge
    /// window outlasts one full panel (commit, reveal and appeal windows), so
    /// a challenge opened in the window can finish its first panel before the
    /// window ends (L33-N6). The last appeal's panel has at most
    /// [`MAX_PANEL`] members (M6-L2-F2).
    pub fn is_valid(&self) -> bool {
        self.panel_size % 2 == 1
            && self
                .panel_commit_window
                .checked_add(self.panel_reveal_window)
                .and_then(|w| w.checked_add(self.appeal_window))
                .is_some_and(|w| self.challenge_window > w)
            && self.panel_commit_window > 0
            && self.panel_reveal_window > 0
            && self.appeal_window > 0
            && self.min_challenge_bond > 0
            && self.panel_slash > 0
            && self
                .panel_fee
                .checked_mul(LAZY_PANEL_RATIO)
                .is_some_and(|f| self.panel_slash >= f)
            && self
                .panel_fee
                .checked_mul(u128::from(self.panel_size) + 1)
                .is_some_and(|f| self.min_challenge_bond >= f)
            && self.max_appeals <= MAX_APPEALS
            && (u64::from(self.panel_size) + 1)
                .checked_shl(self.max_appeals)
                .is_some_and(|n| n - 1 <= u64::from(MAX_PANEL))
    }

    /// Fees owed to a full panel of `size` members.
    pub fn panel_fees(&self, size: u32) -> u128 {
        self.panel_fee.saturating_mul(u128::from(size))
    }

    /// Smallest bond for the appeal that opens panel `level` (>= 1) of
    /// `size` members: `min_challenge_bond * 2^level`, and at least that
    /// panel's fees. Set by the config, never by the bond a challenger chose
    /// to post, so a rich challenger cannot price honest appellants out
    /// (M6-L2-F3). The contract's `OnyxRound.appealFloor`.
    pub fn appeal_floor(&self, level: u32, size: u32) -> u128 {
        let floor = 1u128
            .checked_shl(level)
            .map_or(u128::MAX, |p| self.min_challenge_bond.saturating_mul(p));
        floor.max(self.panel_fees(size))
    }
}

/// How strongly a data claim can be checked: the one proof-ladder enum
/// (`onyx_core::ProofLevel`, ADR-0027). A dispute records the level of the
/// claim it is about; only [`ProofLevel::Recomputed`] is disputable now. The
/// variant order and names are those of the former local enum, so challenge
/// messages and hashes are unchanged (L2R2-F7).
pub use onyx_core::ProofLevel;

/// Dispute error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DisputeError {
    /// Only recomputed claims can be disputed so far.
    #[error("proof level not supported yet")]
    Unsupported,
    /// No shard settled with a reference under this round in this ledger.
    #[error("shard not settled in this ledger")]
    NotSettledHere,
    /// The challenge names the round tag of another ledger or round.
    #[error("challenge is for another ledger or round")]
    WrongTag,
    /// Outside the window for this step.
    #[error("wrong phase")]
    WrongPhase,
    /// Bond below the minimum (for an appeal: below `appeal_floor`).
    #[error("bond too low")]
    BondTooLow,
    /// The claimed root is the settled reference.
    #[error("claimed root equals the reference")]
    SameRoot,
    /// A dispute on this shard is already open.
    #[error("a dispute is already open on this shard")]
    AlreadyOpen,
    /// Signature is not the signer's.
    #[error("bad signature")]
    BadSignature,
    /// No such dispute.
    #[error("unknown dispute")]
    Unknown,
    /// Not on the current panel, or a second vote.
    #[error("not a panel member, or already voted")]
    NotOnPanel,
    /// Reveal does not match its commit, or no commit.
    #[error("reveal does not match the commit")]
    CommitMismatch,
    /// Too few eligible validators left to draw a panel.
    #[error("no eligible panel members")]
    NoPanel,
    /// The current panel is unanimous or the appeal limit is reached.
    #[error("no appeal possible")]
    NoAppeal,
    /// The dispute is already resolved.
    #[error("dispute resolved")]
    Resolved,
    /// This signed challenge was already used in this ledger (L29-F1).
    #[error("challenge already used")]
    Replayed,
    /// The shard's dispute ended Upheld, Overturned or Corrected (L29-F6).
    #[error("shard closed to disputes")]
    Closed,
    /// The shard is marked for a rerun by the settler
    /// ([`Ledger::rerun_shard`]): no panel could be drawn (L31-F3), or a
    /// dispute on it ended without a majority (q13 (a)).
    #[error("the shard must be rerun")]
    RerunRequired,
    /// This machine cannot rerun the shard (inputs unavailable, foreign
    /// rules); the shard stays marked for a rerun.
    #[error("the shard cannot be rerun here")]
    Undecided,
}

/// What a ledger keeps about a shard settled with a reference: everything a
/// dispute judges and every entry it may have to reverse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShardRecord {
    pub(crate) round: u64,
    pub(crate) shard: Hash32,
    pub(crate) tag: Hash32,
    pub(crate) seed: Hash32,
    pub(crate) work: Hash32,
    pub(crate) reference: Hash32,
    pub(crate) path: SettlePath,
    pub(crate) reveals: Vec<(u64, MinerAddress, Hash32)>,
    pub(crate) bonds: BTreeMap<MinerAddress, u128>,
    pub(crate) canaries: BTreeSet<MinerAddress>,
    pub(crate) verdicts: Vec<(MinerAddress, MinerAddress, Verdict)>,
    pub(crate) verdict_givers: BTreeSet<MinerAddress>,
    pub(crate) effects: Vec<LedgerEntry>,
    pub(crate) spec: Option<ShardSpec>,
    pub(crate) validators: ValidatorSet,
    pub(crate) validation: ValidationConfig,
    pub(crate) dispute: DisputeConfig,
    pub(crate) settled_at: u64,
}

impl ShardRecord {
    /// The shard's current reference root.
    pub fn reference(&self) -> Hash32 {
        self.reference
    }

    /// How the shard's reference was reached (a rerun that confirms a
    /// unanimous root records it as recomputed, L33-N3).
    pub fn path(&self) -> SettlePath {
        self.path
    }

    /// The shard's pinned spec (what panel members rerun).
    pub fn spec(&self) -> Option<&ShardSpec> {
        self.spec.as_ref()
    }

    /// The dispute rules fixed when the shard's round opened.
    pub fn dispute_config(&self) -> DisputeConfig {
        self.dispute
    }
}

/// True for the settlement entries that depend on the reference root (a
/// dispute reverses exactly these).
pub(crate) fn depends_on_reference(e: &LedgerEntry) -> bool {
    let reason = match e {
        LedgerEntry::Reward { reason, .. } | LedgerEntry::Slash { reason, .. } => *reason,
    };
    matches!(
        reason,
        "first correct reveal" | "revealed a wrong root" | "correct verdict" | "wrong verdict"
    )
}

/// The result of a dispute. Every outcome is final except `RerunRequired`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisputeOutcome {
    /// The panel (or the rerun) kept the reference; the challenger lost its bond.
    Upheld,
    /// The panel (or the rerun) agreed with the challenger; the reference was
    /// replaced.
    Overturned {
        /// The new reference.
        reference: Hash32,
    },
    /// The panel (or the rerun) found a third root; the reference was
    /// replaced and the challenger lost its bond.
    Corrected {
        /// The new reference.
        reference: Hash32,
    },
    /// Not final (q13 (a)): no strict majority on the last panel. Funders
    /// paid their panels' fees (burned); nobody on the panels was slashed
    /// (L30-F1, L31-F1). The shard waits for [`Ledger::rerun_shard`], which
    /// replaces this with Upheld, Overturned or Corrected, or, if the rerun
    /// deadline passes first, with `Unresolved`.
    RerunRequired,
    /// Never final here (M5-L46-F4): the settler did not rerun the shard
    /// before its rerun deadline (`challenge_window` heights after the
    /// mark). Funders lost only their panels' burned fees; the rest of
    /// every bond stays with its owner; nobody else is paid or slashed. The
    /// shard is closed to disputes and [`Ledger::finality`] reports it
    /// [`Finality::Unresolved`].
    Unresolved,
}

/// Whether a settled shard's root is final in this ledger (M5-L46-F4).
/// Readers (customers, markets, the contract later) settle only on `Final`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finality {
    /// Not settled with a reference in this ledger.
    Unknown,
    /// Settled, but a dispute can still change the root: the challenge
    /// window is open or a dispute is open.
    Disputable {
        /// The current reference.
        reference: Hash32,
    },
    /// A split dispute or an undisputable challenge sent the shard to the
    /// settler's rerun; the root is not final until it runs.
    RerunPending {
        /// The current (disputed) reference.
        reference: Hash32,
        /// After this height the rerun can expire ([`Ledger::expire_rerun`]).
        deadline: u64,
    },
    /// The rerun deadline passed: the root is never final here.
    Unresolved {
        /// The disputed reference.
        reference: Hash32,
    },
    /// No dispute can change the root any more.
    Final {
        /// The final root ([`no_valid_root_marker`] if none is valid).
        reference: Hash32,
    },
}

/// Text a challenger signs (EIP-191). The round tag binds the ledger id.
pub fn challenge_message(
    tag: &Hash32,
    shard: &Hash32,
    claimed_root: &Hash32,
    bond: u128,
    level: ProofLevel,
) -> String {
    format!(
        "Onyx challenge v1\nround: {tag}\nshard: {shard}\nclaimed: {claimed_root}\nbond: {bond}\nlevel: {level:?}"
    )
}

/// A signed challenge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedChallenge {
    /// Round number.
    pub round: u64,
    /// Round tag (carries the ledger id).
    pub tag: Hash32,
    /// Shard.
    pub shard: Hash32,
    /// Challenger address.
    pub challenger: MinerAddress,
    /// The root the challenger claims is right.
    pub claimed_root: Hash32,
    /// Bond posted.
    pub bond: u128,
    /// Proof level of the disputed claim.
    pub level: ProofLevel,
    /// EIP-191 signature over [`challenge_message`].
    pub sig: [u8; 65],
}

/// Hash a panel member commits to.
pub fn panel_commit_hash(
    dispute: &Hash32,
    level: u32,
    member: &MinerAddress,
    root: &Hash32,
    salt: &Hash32,
) -> Hash32 {
    let mut b = b"onyx-panel-vote-v1".to_vec();
    b.extend_from_slice(&dispute.0);
    b.extend_from_slice(&level.to_be_bytes());
    b.extend_from_slice(member);
    b.extend_from_slice(&root.0);
    b.extend_from_slice(&salt.0);
    keccak256(&b)
}

/// Text a panel member signs to post a vote commit.
pub fn panel_vote_message(dispute: &Hash32, level: u32, commit: &Hash32) -> String {
    format!("Onyx panel vote v1\ndispute: {dispute}\nlevel: {level}\ncommit: {commit}")
}

/// A signed panel vote commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedPanelVote {
    /// Dispute id.
    pub dispute: Hash32,
    /// Panel level (0 = first panel, 1 = first appeal, ...).
    pub level: u32,
    /// Panel member.
    pub member: MinerAddress,
    /// [`panel_commit_hash`].
    pub commit: Hash32,
    /// EIP-191 signature over [`panel_vote_message`].
    pub sig: [u8; 65],
}

/// Text an appellant signs.
pub fn appeal_message(dispute: &Hash32, level: u32, root: &Hash32, bond: u128) -> String {
    format!("Onyx appeal v1\ndispute: {dispute}\nlevel: {level}\nbacks: {root}\nbond: {bond}")
}

/// A signed appeal against panel `level`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedAppeal {
    /// Dispute id.
    pub dispute: Hash32,
    /// The panel level appealed against (the current one).
    pub level: u32,
    /// Appellant.
    pub appellant: MinerAddress,
    /// The root the appellant backs.
    pub root: Hash32,
    /// Bond posted (at least `appeal_floor`).
    pub bond: u128,
    /// EIP-191 signature over [`appeal_message`].
    pub sig: [u8; 65],
}

/// One panel of a dispute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Panel {
    members: Vec<MinerAddress>,
    commit_close: u64,
    reveal_close: u64,
    appeal_close: u64,
    commits: BTreeMap<MinerAddress, Hash32>,
    reveals: BTreeMap<MinerAddress, Hash32>,
}

impl Panel {
    /// Members, in draw order.
    pub fn members(&self) -> &[MinerAddress] {
        &self.members
    }

    /// Revealed roots.
    pub fn reveals(&self) -> &BTreeMap<MinerAddress, Hash32> {
        &self.reveals
    }

    /// Commits close at this height.
    pub fn commit_close(&self) -> u64 {
        self.commit_close
    }

    /// Reveals close at this height.
    pub fn reveal_close(&self) -> u64 {
        self.reveal_close
    }

    /// Appeals close at this height.
    pub fn appeal_close(&self) -> u64 {
        self.appeal_close
    }

    /// The root a strict majority of members revealed, if any.
    fn majority(&self) -> Option<Hash32> {
        let mut count: BTreeMap<Hash32, usize> = BTreeMap::new();
        for r in self.reveals.values() {
            *count.entry(*r).or_default() += 1;
        }
        count
            .into_iter()
            .find(|(_, n)| n * 2 > self.members.len())
            .map(|(r, _)| r)
    }

    /// Every member revealed, and all the same root.
    fn unanimous(&self) -> bool {
        self.reveals.len() == self.members.len()
            && self.reveals.values().collect::<BTreeSet<_>>().len() == 1
    }
}

/// One dispute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dispute {
    id: Hash32,
    round: u64,
    shard: Hash32,
    level: ProofLevel,
    challenger: MinerAddress,
    claimed_root: Hash32,
    bond: u128,
    appeals: Vec<(MinerAddress, Hash32, u128)>,
    panels: Vec<Panel>,
    outcome: Option<DisputeOutcome>,
    paid_out: u128,
    forfeited: u128,
    /// What each funder (challenger, then appellants) has left after its
    /// panel's fees; kept while the dispute waits for a rerun.
    remainder: Vec<u128>,
    /// An appeal on a split panel found no panel: the dispute goes to the
    /// rerun, never to the split panel's majority (M5-L46-F2, q13 (a)).
    rerun_forced: bool,
}

impl Dispute {
    /// Total this dispute paid out (panel fees, challenge reward, upheld
    /// miner bonus); never more than [`Dispute::forfeited`] (L29-F3).
    pub fn paid_out(&self) -> u128 {
        self.paid_out
    }

    /// Total this dispute took: funders' fee payments and lost bonds, panel
    /// slashes, and net new miner slashes after a correction.
    pub fn forfeited(&self) -> u128 {
        self.forfeited
    }

    /// Dispute id.
    pub fn id(&self) -> Hash32 {
        self.id
    }

    /// The panels drawn so far (the last is current).
    pub fn panels(&self) -> &[Panel] {
        &self.panels
    }

    /// Current panel level.
    pub fn current_level(&self) -> u32 {
        u32::try_from(self.panels.len().saturating_sub(1)).unwrap_or(u32::MAX)
    }

    /// The outcome, once resolved; `RerunRequired` while it waits for the
    /// settler's rerun.
    pub fn outcome(&self) -> Option<DisputeOutcome> {
        self.outcome
    }

    /// The proof level of the disputed claim.
    pub fn proof_level(&self) -> ProofLevel {
        self.level
    }

    fn current(&self) -> Option<&Panel> {
        self.panels.last()
    }
}

/// Deterministic dispute id. The settlement contract numbers its cases with
/// the same layout (ADR-0024: the claim commitment in the claimed-root slot,
/// zero challenger and claim for a rerun case; shared vector `disputes.disputeId`).
pub fn dispute_id(
    ledger: &Hash32,
    round: u64,
    shard: &Hash32,
    challenger: &MinerAddress,
    claimed: &Hash32,
    n: usize,
) -> Hash32 {
    let mut b = b"onyx-dispute-v1".to_vec();
    b.extend_from_slice(&ledger.0);
    b.extend_from_slice(&round.to_be_bytes());
    b.extend_from_slice(&shard.0);
    b.extend_from_slice(challenger);
    b.extend_from_slice(&claimed.0);
    b.extend_from_slice(&(n as u64).to_be_bytes());
    keccak256(&b)
}

/// Commitment to a challenger's claimed root on chain (ADR-0024): the claim
/// is revealed only after the panel has voted, so a mempool copier gains
/// nothing. keccak256("onyx-claim-v1" || ledger id || round (u64 BE) ||
/// shard || challenger || root || salt). Shared vector `disputes.claimCommit`.
pub fn claim_commit(
    ledger: &Hash32,
    round: u64,
    shard: &Hash32,
    challenger: &MinerAddress,
    root: &Hash32,
    salt: &Hash32,
) -> Hash32 {
    let mut b = b"onyx-claim-v1".to_vec();
    b.extend_from_slice(&ledger.0);
    b.extend_from_slice(&round.to_be_bytes());
    b.extend_from_slice(&shard.0);
    b.extend_from_slice(challenger);
    b.extend_from_slice(&root.0);
    b.extend_from_slice(&salt.0);
    keccak256(&b)
}

/// Key of a signed challenge: it opens at most once per ledger (L29-F1).
fn challenge_key(c: &SignedChallenge) -> Hash32 {
    let mut b = b"onyx-challenge-key-v1".to_vec();
    b.extend_from_slice(&c.challenger);
    b.extend_from_slice(&c.round.to_be_bytes());
    b.extend_from_slice(&c.shard.0);
    b.extend_from_slice(&c.claimed_root.0);
    b.extend_from_slice(&c.bond.to_be_bytes());
    b.extend_from_slice(&borsh::to_vec(&c.level).unwrap_or_default());
    keccak256(&b)
}

/// Draw exactly `size` panel members from `candidates` (sorted by a keyed
/// hash), or none when fewer are available: a panel never shrinks (L29-F2).
pub fn draw_panel(
    seed: &Hash32,
    dispute: &Hash32,
    level: u32,
    beacon: &Hash32,
    candidates: &BTreeSet<MinerAddress>,
    size: u32,
) -> Vec<MinerAddress> {
    let mut keyed: Vec<(Hash32, MinerAddress)> = candidates
        .iter()
        .map(|c| {
            let mut b = b"onyx-panel-v1".to_vec();
            b.extend_from_slice(&seed.0);
            b.extend_from_slice(&dispute.0);
            b.extend_from_slice(&level.to_be_bytes());
            b.extend_from_slice(&beacon.0);
            b.extend_from_slice(c);
            (keccak256(&b), *c)
        })
        .collect();
    keyed.sort_unstable();
    let n = size as usize;
    if n == 0 || keyed.len() < n {
        return Vec::new();
    }
    keyed.into_iter().take(n).map(|(_, c)| c).collect()
}

/// The root an honest panel member reports: a rerun of the shard from its
/// pinned inputs, the no-valid-root marker when no root can be right, or
/// `None` when this machine cannot decide (it should then not vote). Like
/// settlement, it never reruns with a registry whose rules are not the
/// built-in ones for `R` (L29-F8).
pub fn honest_panel_root<R: RuleSet>(
    env: &RecomputeEnv<'_>,
    rec: &ShardRecord,
    cache: &AttestCache,
) -> Option<Hash32> {
    if !env.reg.rules_built_in::<R>() {
        return None;
    }
    match env.reference(rec.spec.as_ref()?, cache) {
        Reference::Root(r) => Some(r),
        Reference::NoValidRoot => Some(no_valid_root_marker(&rec.shard)),
        Reference::Undecided(_) => None,
    }
}

impl<R: RuleSet> Ledger<R> {
    /// The settled record of a shard, if this ledger settled it with a reference.
    pub fn shard_record(&self, round: u64, shard: &Hash32) -> Option<&ShardRecord> {
        self.settled.get(&(round, *shard))
    }

    /// A dispute by id.
    pub fn dispute(&self, id: &Hash32) -> Option<&Dispute> {
        self.disputes.get(id)
    }

    /// Open a dispute (rule 1) and draw its first panel with `beacon` (a
    /// block hash after the challenge; read from the chain in M6).
    pub fn open_dispute(
        &mut self,
        now: u64,
        c: &SignedChallenge,
        beacon: Hash32,
    ) -> Result<Hash32, DisputeError> {
        if c.level != ProofLevel::Recomputed {
            return Err(DisputeError::Unsupported);
        }
        let rec = self
            .settled
            .get(&(c.round, c.shard))
            .ok_or(DisputeError::NotSettledHere)?;
        if rec.tag != c.tag {
            return Err(DisputeError::WrongTag);
        }
        let msg = challenge_message(&c.tag, &c.shard, &c.claimed_root, c.bond, c.level);
        if eip191_recover(msg.as_bytes(), &c.sig) != Some(c.challenger) {
            return Err(DisputeError::BadSignature);
        }
        if now < rec.settled_at
            || now >= rec.settled_at.saturating_add(rec.dispute.challenge_window)
        {
            return Err(DisputeError::WrongPhase);
        }
        if c.bond < rec.dispute.min_challenge_bond {
            return Err(DisputeError::BondTooLow);
        }
        if c.claimed_root == rec.reference {
            return Err(DisputeError::SameRoot);
        }
        if self.closed_disputes.contains(&(c.round, c.shard)) {
            return Err(DisputeError::Closed);
        }
        if self.rerun_required.contains(&(c.round, c.shard)) {
            return Err(DisputeError::RerunRequired);
        }
        if self.open_disputes.contains_key(&(c.round, c.shard)) {
            return Err(DisputeError::AlreadyOpen);
        }
        let key = challenge_key(c);
        if self.consumed_challenges.contains(&key) {
            return Err(DisputeError::Replayed);
        }
        if c.bond < rec.dispute.panel_fees(rec.dispute.panel_size) {
            return Err(DisputeError::BondTooLow);
        }
        let n = self
            .disputes
            .values()
            .filter(|d| d.round == c.round && d.shard == c.shard)
            .count();
        let id = dispute_id(
            &self.id,
            c.round,
            &c.shard,
            &c.challenger,
            &c.claimed_root,
            n,
        );
        let mut d = Dispute {
            id,
            round: c.round,
            shard: c.shard,
            level: c.level,
            challenger: c.challenger,
            claimed_root: c.claimed_root,
            bond: c.bond,
            appeals: Vec::new(),
            panels: Vec::new(),
            outcome: None,
            paid_out: 0,
            forfeited: 0,
            remainder: Vec::new(),
            rerun_forced: false,
        };
        let slash = rec.dispute.panel_slash;
        let panel = match new_panel(
            rec,
            &d,
            now,
            &beacon,
            rec.dispute.panel_size,
            &self.validator_slashed,
            &self.panel_exposure,
        ) {
            Ok(p) => p,
            // A root nobody can dispute any more is not left final: the
            // settler must rerun the shard (L31-F3, q13 (a)).
            Err(DisputeError::NoPanel) => {
                self.consumed_challenges.insert(key);
                self.mark_rerun(c.round, c.shard, now, rec.dispute.challenge_window);
                return Err(DisputeError::RerunRequired);
            }
            Err(e) => return Err(e),
        };
        self.expose(&panel.members, slash);
        d.panels.push(panel);
        self.consumed_challenges.insert(key);
        self.open_disputes.insert((c.round, c.shard), id);
        self.disputes.insert(id, d);
        Ok(id)
    }

    /// Post a signed panel vote commit for the current panel.
    pub fn commit_panel_vote(&mut self, now: u64, v: &SignedPanelVote) -> Result<(), DisputeError> {
        let d = self
            .disputes
            .get_mut(&v.dispute)
            .ok_or(DisputeError::Unknown)?;
        if d.outcome.is_some() {
            return Err(DisputeError::Resolved);
        }
        if v.level != d.current_level() {
            return Err(DisputeError::WrongPhase);
        }
        let msg = panel_vote_message(&v.dispute, v.level, &v.commit);
        if eip191_recover(msg.as_bytes(), &v.sig) != Some(v.member) {
            return Err(DisputeError::BadSignature);
        }
        let p = d.panels.last_mut().ok_or(DisputeError::Unknown)?;
        if now >= p.commit_close {
            return Err(DisputeError::WrongPhase);
        }
        if !p.members.contains(&v.member) || p.commits.contains_key(&v.member) {
            return Err(DisputeError::NotOnPanel);
        }
        p.commits.insert(v.member, v.commit);
        Ok(())
    }

    /// Reveal a committed panel vote.
    pub fn reveal_panel_vote(
        &mut self,
        now: u64,
        dispute: &Hash32,
        member: MinerAddress,
        root: Hash32,
        salt: Hash32,
    ) -> Result<(), DisputeError> {
        let d = self
            .disputes
            .get_mut(dispute)
            .ok_or(DisputeError::Unknown)?;
        if d.outcome.is_some() {
            return Err(DisputeError::Resolved);
        }
        let level = d.current_level();
        let p = d.panels.last_mut().ok_or(DisputeError::Unknown)?;
        if now < p.commit_close || now >= p.reveal_close {
            return Err(DisputeError::WrongPhase);
        }
        let want = p.commits.get(&member).ok_or(DisputeError::CommitMismatch)?;
        if panel_commit_hash(dispute, level, &member, &root, &salt) != *want
            || p.reveals.contains_key(&member)
        {
            return Err(DisputeError::CommitMismatch);
        }
        p.reveals.insert(member, root);
        Ok(())
    }

    /// Appeal the current panel (rule 4) and draw the next, larger panel.
    pub fn appeal(
        &mut self,
        now: u64,
        a: &SignedAppeal,
        beacon: Hash32,
    ) -> Result<(), DisputeError> {
        let d = self.disputes.get(&a.dispute).ok_or(DisputeError::Unknown)?;
        if d.outcome.is_some() {
            return Err(DisputeError::Resolved);
        }
        let rec = self
            .settled
            .get(&(d.round, d.shard))
            .ok_or(DisputeError::NotSettledHere)?;
        if a.level != d.current_level() {
            return Err(DisputeError::WrongPhase);
        }
        let msg = appeal_message(&a.dispute, a.level, &a.root, a.bond);
        if eip191_recover(msg.as_bytes(), &a.sig) != Some(a.appellant) {
            return Err(DisputeError::BadSignature);
        }
        let p = d.current().ok_or(DisputeError::Unknown)?;
        if now < p.reveal_close || now >= p.appeal_close {
            return Err(DisputeError::WrongPhase);
        }
        if p.unanimous() || d.rerun_forced || d.appeals.len() >= rec.dispute.max_appeals as usize {
            return Err(DisputeError::NoAppeal);
        }
        let size = u32::try_from(p.members.len())
            .unwrap_or(u32::MAX)
            .saturating_mul(2)
            .saturating_add(1);
        // The appellant funds the next panel's fees whatever the outcome
        // (L29-F3); the floor doubles from the config's minimum, not from the
        // bond the challenger chose (M6-L2-F3).
        let level = u32::try_from(d.appeals.len() + 1).unwrap_or(u32::MAX);
        if a.bond < rec.dispute.appeal_floor(level, size) {
            return Err(DisputeError::BondTooLow);
        }
        let mut next = d.clone();
        next.appeals.push((a.appellant, a.root, a.bond));
        // Too few undrawn validators for a full panel (L29-F2): a panel
        // never shrinks, and a split panel never decides by its bare
        // majority, so the dispute goes to the settler's rerun when it
        // resolves (M5-L46-F2, q13 (a)). The appeal itself is not taken.
        let slash = rec.dispute.panel_slash;
        let panel = match new_panel(
            rec,
            &next,
            now,
            &beacon,
            size,
            &self.validator_slashed,
            &self.panel_exposure,
        ) {
            Ok(p) => p,
            Err(_) => {
                if let Some(stored) = self.disputes.get_mut(&a.dispute) {
                    stored.rerun_forced = true;
                }
                return Err(DisputeError::RerunRequired);
            }
        };
        self.expose(&panel.members, slash);
        next.panels.push(panel);
        self.disputes.insert(a.dispute, next);
        Ok(())
    }

    /// Resolve a dispute (rule 5) once the current panel's reveals have
    /// closed and no appeal is possible any more (unanimous panel, appeal
    /// limit reached, or the appeal window over). Appends the dispute's
    /// ledger entries and, if the reference changes, corrects the finalized
    /// root. Returns the outcome.
    pub fn resolve_dispute(
        &mut self,
        now: u64,
        id: &Hash32,
    ) -> Result<DisputeOutcome, DisputeError> {
        let d = self.disputes.get(id).ok_or(DisputeError::Unknown)?.clone();
        if d.outcome.is_some() {
            return Err(DisputeError::Resolved);
        }
        let rec = self
            .settled
            .get(&(d.round, d.shard))
            .ok_or(DisputeError::NotSettledHere)?
            .clone();
        let last = d.current().ok_or(DisputeError::Unknown)?;
        if now < last.reveal_close {
            return Err(DisputeError::WrongPhase);
        }
        let appealable = !last.unanimous()
            && !d.rerun_forced
            && d.appeals.len() < rec.dispute.max_appeals as usize;
        if appealable && now < last.appeal_close {
            return Err(DisputeError::WrongPhase);
        }
        // An appeal that found no panel leaves the split to the rerun
        // (M5-L46-F2).
        let majority = if d.rerun_forced {
            None
        } else {
            last.majority()
        };
        let round = d.round;
        // Validator slash caps count everything this ledger already took (L29-F5).
        let mut t = Tally {
            out: Vec::new(),
            paid: 0,
            forfeit: 0,
            caps: self.validator_slashed.clone(),
        };

        // Each panel is funded by whoever asked for it: level 0 by the
        // challenger, level L by appeal L-1 (L29-F3).
        let funder = |level: usize| -> (MinerAddress, Hash32, u128) {
            if level == 0 {
                (d.challenger, d.claimed_root, d.bond)
            } else {
                d.appeals[level - 1]
            }
        };
        let mut remainder: Vec<u128> = Vec::with_capacity(d.panels.len());
        for (level, p) in d.panels.iter().enumerate() {
            let (who, _, bond) = funder(level);
            let mut fees: u128 = 0;
            // Without a final majority nobody is judged: the rerun decides
            // and nobody is slashed for a split (L31-F1, q13 (a)). With one,
            // silence is slashed only on a panel where someone revealed the
            // majority root: a panel where nobody did could not rerun the
            // shard (inputs gone, rerun too slow, foreign rules), and one
            // sybil's junk reveal does not change that (L30-F1, L33-N4).
            let nobody_revealed = p.reveals.is_empty();
            let majority_here = majority.is_some_and(|maj| p.reveals.values().any(|r| *r == maj));
            for m in &p.members {
                let right = match (p.reveals.get(m), majority) {
                    (_, None) => None,
                    (None, Some(_)) if !majority_here => None,
                    (None, Some(_)) => Some(false),
                    (Some(r), Some(maj)) => Some(*r == maj),
                };
                match right {
                    Some(true) => {
                        let fee = rec.dispute.panel_fee.min(bond.saturating_sub(fees));
                        if fee > 0 {
                            fees += fee;
                            t.out.push(LedgerEntry::Reward {
                                round,
                                actor: *m,
                                amount: fee,
                                reason: "panel: on the majority",
                            });
                        }
                    }
                    Some(false) => {
                        let used = t.caps.entry(*m).or_insert(0);
                        let take = rec
                            .dispute
                            .panel_slash
                            .min(rec.validators.bond(m).saturating_sub(*used));
                        if take > 0 {
                            *used += take;
                            t.forfeit = t.forfeit.saturating_add(take);
                            t.out.push(LedgerEntry::Slash {
                                round,
                                actor: *m,
                                amount: take,
                                reason: "panel: no reveal or off the majority",
                            });
                        }
                    }
                    None => {}
                }
            }
            if fees > 0 {
                t.paid = t.paid.saturating_add(fees);
                t.forfeit = t.forfeit.saturating_add(fees);
                t.out.push(LedgerEntry::Slash {
                    round,
                    actor: who,
                    amount: fees,
                    reason: "panel fees paid",
                });
            }
            // No majority at the end, or a panel nobody revealed on: its
            // funder still pays the full panel's fees, burned, so a dispute
            // that goes to a rerun is never free (L30-F1).
            let burn = if majority.is_none() || nobody_revealed {
                rec.dispute
                    .panel_fees(u32::try_from(p.members.len()).unwrap_or(u32::MAX))
                    .min(bond.saturating_sub(fees))
            } else {
                0
            };
            if burn > 0 {
                t.forfeit = t.forfeit.saturating_add(burn);
                t.out.push(LedgerEntry::Slash {
                    round,
                    actor: who,
                    amount: burn,
                    reason: "panel fees burned",
                });
            }
            remainder.push(bond.saturating_sub(fees).saturating_sub(burn));
        }

        let outcome = match majority {
            // A split is not a final answer: the settler's rerun decides
            // (q13 (a)). The shard stays open to that rerun only.
            None => {
                self.mark_rerun(d.round, d.shard, now, rec.dispute.challenge_window);
                DisputeOutcome::RerunRequired
            }
            Some(maj) => {
                let o = self.decide(&d, &rec, maj, &remainder, &mut t);
                self.closed_disputes.insert((d.round, d.shard));
                o
            }
        };
        // `caps` now holds every validator slash net of reversals (L29-F5).
        self.validator_slashed = t.caps;
        self.entries.extend(t.out);
        self.open_disputes.remove(&(d.round, d.shard));
        // The dispute's panels no longer stake their members' bonds (L31-F2).
        let slash = rec.dispute.panel_slash;
        for p in &d.panels {
            self.unexpose(&p.members, slash);
        }
        if let Some(stored) = self.disputes.get_mut(id) {
            stored.outcome = Some(outcome);
            stored.paid_out = t.paid;
            stored.forfeited = t.forfeit;
            stored.remainder = remainder;
        }
        Ok(outcome)
    }

    /// Decide a dispute once its root is known, from a final panel majority
    /// or from the settler's rerun: appellants backing another root lose the
    /// rest of their bond, and the challenger wins or loses as rule 5 says.
    /// `remainder` is what each funder has left after its panel's fees.
    fn decide(
        &mut self,
        d: &Dispute,
        rec: &ShardRecord,
        root: Hash32,
        remainder: &[u128],
        t: &mut Tally,
    ) -> DisputeOutcome {
        let round = d.round;
        for (i, (who, backed, _)) in d.appeals.iter().enumerate() {
            let rest = remainder.get(i + 1).copied().unwrap_or(0);
            if *backed != root && rest > 0 {
                t.forfeit = t.forfeit.saturating_add(rest);
                t.out.push(LedgerEntry::Slash {
                    round,
                    actor: *who,
                    amount: rest,
                    reason: "appeal lost",
                });
            }
        }
        let challenger_rest = remainder.first().copied().unwrap_or(0);
        if root == rec.reference {
            if challenger_rest > 0 {
                t.forfeit = t.forfeit.saturating_add(challenger_rest);
                t.out.push(LedgerEntry::Slash {
                    round,
                    actor: d.challenger,
                    amount: challenger_rest,
                    reason: "challenge lost",
                });
            }
            if let Some(first) = first_correct(rec, &rec.reference) {
                let bonus = challenger_rest / 2;
                if bonus > 0 {
                    t.paid = t.paid.saturating_add(bonus);
                    t.out.push(LedgerEntry::Reward {
                        round,
                        actor: first,
                        amount: bonus,
                        reason: "challenge lost: half to the upheld miner",
                    });
                }
            }
            return DisputeOutcome::Upheld;
        }
        // Reverse what the old reference caused, then judge again.
        let net_new_miner_slashes = self.correct_reference(rec, root, &mut t.caps, &mut t.out);
        t.forfeit = t.forfeit.saturating_add(net_new_miner_slashes);
        if root == d.claimed_root {
            let reward = net_new_miner_slashes / 2;
            if reward > 0 {
                t.paid = t.paid.saturating_add(reward);
                t.out.push(LedgerEntry::Reward {
                    round,
                    actor: d.challenger,
                    amount: reward,
                    reason: "challenge won",
                });
            }
            return DisputeOutcome::Overturned { reference: root };
        }
        if challenger_rest > 0 {
            t.forfeit = t.forfeit.saturating_add(challenger_rest);
            t.out.push(LedgerEntry::Slash {
                round,
                actor: d.challenger,
                amount: challenger_rest,
                reason: "challenge lost",
            });
        }
        DisputeOutcome::Corrected { reference: root }
    }
}

/// Ledger entries a dispute step appends, with its running totals.
struct Tally {
    out: Vec<LedgerEntry>,
    paid: u128,
    forfeit: u128,
    caps: BTreeMap<MinerAddress, u128>,
}

impl<R: RuleSet> Ledger<R> {
    /// `panel_slash` each member of an open panel stakes until it resolves (L31-F2).
    pub fn panel_exposure(&self, v: &MinerAddress) -> u128 {
        self.panel_exposure.get(v).copied().unwrap_or(0)
    }

    /// Shards marked for a rerun: no panel could dispute them (L31-F3), or a
    /// dispute on them ended without a majority (q13 (a)).
    pub fn rerun_required(&self) -> &BTreeSet<(u64, Hash32)> {
        &self.rerun_required
    }

    /// Mark a shard for the settler's rerun, due within `timeout` heights
    /// (the shard's `challenge_window`, M5-L46-F4).
    fn mark_rerun(&mut self, round: u64, shard: Hash32, now: u64, timeout: u64) {
        if self.rerun_required.insert((round, shard)) {
            self.rerun_deadline
                .insert((round, shard), now.saturating_add(timeout));
        }
    }

    /// Whether a settled shard's root is final at height `now` (M5-L46-F4).
    /// A root is final once no dispute can change it: its challenge window
    /// is over with no dispute open and no rerun pending, or a dispute or
    /// rerun decided it. A shard waiting for a rerun is `RerunPending`; one
    /// whose rerun deadline passed is `Unresolved`, never final.
    pub fn finality(&self, now: u64, round: u64, shard: &Hash32) -> Finality {
        let key = (round, *shard);
        let Some(rec) = self.settled.get(&key) else {
            return Finality::Unknown;
        };
        let reference = rec.reference;
        if self.unresolved.contains(&key) {
            return Finality::Unresolved { reference };
        }
        if self.rerun_required.contains(&key) {
            let deadline = self.rerun_deadline.get(&key).copied().unwrap_or(u64::MAX);
            return Finality::RerunPending {
                reference,
                deadline,
            };
        }
        if self.open_disputes.contains_key(&key) {
            return Finality::Disputable { reference };
        }
        let window_over = now >= rec.settled_at.saturating_add(rec.dispute.challenge_window);
        if self.closed_disputes.contains(&key) || window_over {
            Finality::Final { reference }
        } else {
            Finality::Disputable { reference }
        }
    }

    /// The final root of a work id at height `now`: `Some` only when the
    /// shard that finalized it is [`Finality::Final`] (M5-L46-F4).
    /// [`Ledger::finalized`] also lists roots a dispute or rerun may still
    /// change; readers who pay or settle on a root use this.
    pub fn final_root(&self, now: u64, work: &Hash32) -> Option<Hash32> {
        let (round, root) = self.finalized.get(work)?;
        let rec = self
            .settled
            .values()
            .find(|r| r.round == *round && r.work == *work)?;
        match self.finality(now, *round, &rec.shard) {
            Finality::Final { .. } => Some(*root),
            _ => None,
        }
    }

    /// Expire a rerun the settler did not run by its deadline (M5-L46-F4).
    /// The shard becomes [`Finality::Unresolved`] and is closed to disputes;
    /// a dispute waiting on it ends `Unresolved`: its funders keep what is
    /// left of their bonds after the burned panel fees, and nobody else is
    /// paid or slashed. Held no-valid-root work is released at the next
    /// round open (no rerun can change it any more). `WrongPhase` before
    /// the deadline, `Unknown` if no rerun is pending.
    pub fn expire_rerun(
        &mut self,
        now: u64,
        round: u64,
        shard: &Hash32,
    ) -> Result<DisputeOutcome, DisputeError> {
        let key = (round, *shard);
        if !self.rerun_required.contains(&key) {
            return Err(DisputeError::Unknown);
        }
        let deadline = self.rerun_deadline.get(&key).copied().unwrap_or(u64::MAX);
        if now < deadline {
            return Err(DisputeError::WrongPhase);
        }
        self.rerun_required.remove(&key);
        self.rerun_deadline.remove(&key);
        self.unresolved.insert(key);
        self.closed_disputes.insert(key);
        if let Some(d) = self.disputes.values_mut().find(|d| {
            d.round == round
                && d.shard == *shard
                && d.outcome == Some(DisputeOutcome::RerunRequired)
        }) {
            d.outcome = Some(DisputeOutcome::Unresolved);
        }
        Ok(DisputeOutcome::Unresolved)
    }

    fn expose(&mut self, members: &[MinerAddress], slash: u128) {
        for m in members {
            let e = self.panel_exposure.entry(*m).or_insert(0);
            *e = e.saturating_add(slash);
        }
    }

    fn unexpose(&mut self, members: &[MinerAddress], slash: u128) {
        for m in members {
            if let Some(e) = self.panel_exposure.get_mut(m) {
                *e = e.saturating_sub(slash);
                if *e == 0 {
                    self.panel_exposure.remove(m);
                }
            }
        }
    }

    /// Rerun a shard marked "rerun required" from its pinned inputs, as
    /// settlement would, and correct its reference if the rerun gives another
    /// root (the correction reverses what the old reference caused and judges
    /// miners and validators again). A dispute waiting on the shard (a split
    /// final panel, q13 (a)) is decided by the rerun root exactly as a panel
    /// majority would decide it, and its outcome is returned; with no dispute
    /// nobody is paid and the result is `Upheld` or `Corrected`. Either way
    /// the shard is then closed to disputes, and a confirmed unanimous root is
    /// recorded as recomputed, so sham challenges cannot force another rerun
    /// (L33-N3). `Undecided` (and the mark stays) when this machine cannot
    /// rerun.
    pub fn rerun_shard(
        &mut self,
        round: u64,
        shard: &Hash32,
        env: &RecomputeEnv<'_>,
        cache: &AttestCache,
    ) -> Result<DisputeOutcome, DisputeError> {
        if !self.rerun_required.contains(&(round, *shard)) {
            return Err(DisputeError::Unknown);
        }
        let rec = self
            .settled
            .get(&(round, *shard))
            .ok_or(DisputeError::NotSettledHere)?
            .clone();
        if !env.reg.rules_built_in::<R>() {
            return Err(DisputeError::Undecided);
        }
        let spec = rec.spec.as_ref().ok_or(DisputeError::Undecided)?;
        let root = match env.reference(spec, cache) {
            Reference::Root(r) => r,
            Reference::NoValidRoot => no_valid_root_marker(shard),
            Reference::Undecided(_) => return Err(DisputeError::Undecided),
        };
        self.rerun_required.remove(&(round, *shard));
        self.rerun_deadline.remove(&(round, *shard));
        let waiting = self
            .disputes
            .values()
            .find(|d| {
                d.round == round
                    && d.shard == *shard
                    && d.outcome == Some(DisputeOutcome::RerunRequired)
            })
            .cloned();
        let mut t = Tally {
            out: Vec::new(),
            paid: 0,
            forfeit: 0,
            caps: self.validator_slashed.clone(),
        };
        let outcome = match &waiting {
            Some(d) => self.decide(d, &rec, root, &d.remainder, &mut t),
            None if root == rec.reference => DisputeOutcome::Upheld,
            None => {
                self.correct_reference(&rec, root, &mut t.caps, &mut t.out);
                DisputeOutcome::Corrected { reference: root }
            }
        };
        if root == rec.reference {
            if let Some(r) = self.settled.get_mut(&(round, *shard)) {
                if r.path == SettlePath::Unanimous {
                    r.path = SettlePath::Recomputed;
                }
            }
        }
        self.validator_slashed = t.caps;
        self.entries.extend(t.out);
        self.closed_disputes.insert((round, *shard));
        if let Some(stored) = waiting.and_then(|d| self.disputes.get_mut(&d.id)) {
            stored.outcome = Some(outcome);
            stored.paid_out = stored.paid_out.saturating_add(t.paid);
            stored.forfeited = stored.forfeited.saturating_add(t.forfeit);
        }
        Ok(outcome)
    }

    /// Replace a shard's reference with `new`: reverse every entry the old
    /// reference caused (compensating entries, never edits), judge miners and
    /// validators again, update the record and the finalized root. Returns
    /// the net new miner slashes (L29-F9). A correction to the no-valid-root
    /// marker holds the work until the shard's challenge window ends, like
    /// settling with no valid root; a correction to a valid root clears any
    /// hold or burn (L31-F4). Never touches a finalization another round
    /// owns (L30-F4).
    fn correct_reference(
        &mut self,
        rec: &ShardRecord,
        new: Hash32,
        caps: &mut BTreeMap<MinerAddress, u128>,
        out: &mut Vec<LedgerEntry>,
    ) -> u128 {
        let round = rec.round;
        for e in &rec.effects {
            out.push(match e {
                LedgerEntry::Reward { actor, amount, .. } => LedgerEntry::Slash {
                    round,
                    actor: *actor,
                    amount: *amount,
                    reason: "dispute: reward reversed",
                },
                LedgerEntry::Slash {
                    actor,
                    amount,
                    reason,
                    ..
                } => {
                    if crate::validate::is_validator_slash(reason) {
                        let t = caps.entry(*actor).or_insert(0);
                        *t = t.saturating_sub(*amount);
                    }
                    LedgerEntry::Reward {
                        round,
                        actor: *actor,
                        amount: *amount,
                        reason: "dispute: slash reversed",
                    }
                }
            });
        }
        let (effects, net_new_miner_slashes) = rejudge(rec, &new, caps);
        out.extend(effects.iter().cloned());
        let marker = no_valid_root_marker(&rec.shard);
        if let Some(r) = self.settled.get_mut(&(rec.round, rec.shard)) {
            r.reference = new;
            r.effects = effects;
            r.path = if new == marker {
                SettlePath::NoValidRoot
            } else {
                SettlePath::Recomputed
            };
        }
        let elsewhere = self
            .finalized
            .get(&rec.work)
            .is_some_and(|(r, _)| *r != rec.round);
        if !elsewhere {
            if new == marker {
                self.finalized.remove(&rec.work);
                self.held.insert(
                    rec.work,
                    (
                        rec.settled_at.saturating_add(rec.dispute.challenge_window),
                        rec.round,
                        rec.shard,
                    ),
                );
            } else {
                self.burned.remove(&rec.work);
                self.held.remove(&rec.work);
                self.finalized.insert(rec.work, (rec.round, new));
            }
        }
        net_new_miner_slashes
    }
}

/// Draw a panel for `d` at its next level.
fn new_panel(
    rec: &ShardRecord,
    d: &Dispute,
    now: u64,
    beacon: &Hash32,
    size: u32,
    slashed: &BTreeMap<MinerAddress, u128>,
    exposure: &BTreeMap<MinerAddress, u128>,
) -> Result<Panel, DisputeError> {
    let mut excluded: BTreeSet<MinerAddress> = rec.reveals.iter().map(|(_, m, _)| *m).collect();
    excluded.insert(d.challenger);
    excluded.extend(d.appeals.iter().map(|(a, _, _)| *a));
    excluded.extend(rec.verdict_givers.iter().copied());
    for p in &d.panels {
        excluded.extend(p.members.iter().copied());
    }
    let candidates: BTreeSet<MinerAddress> = rec
        .validators
        .members()
        .filter(|v| !excluded.contains(*v))
        // A member must have enough bond left to be slashed (L30-F3), after
        // what the ledger took and what its open panels already stake (L31-F2).
        .filter(|v| {
            rec.validators
                .bond(v)
                .saturating_sub(slashed.get(*v).copied().unwrap_or(0))
                .saturating_sub(exposure.get(*v).copied().unwrap_or(0))
                >= rec.dispute.panel_slash
        })
        .copied()
        .collect();
    let level = u32::try_from(d.panels.len()).unwrap_or(u32::MAX);
    let members = draw_panel(&rec.seed, &d.id, level, beacon, &candidates, size);
    if members.is_empty() {
        return Err(DisputeError::NoPanel);
    }
    let cfg = rec.dispute;
    let commit_close = now.saturating_add(cfg.panel_commit_window);
    let reveal_close = commit_close.saturating_add(cfg.panel_reveal_window);
    Ok(Panel {
        members,
        commit_close,
        reveal_close,
        appeal_close: reveal_close.saturating_add(cfg.appeal_window),
        commits: BTreeMap::new(),
        reveals: BTreeMap::new(),
    })
}

/// First non-canary reveal (commit order) whose root is `reference`.
fn first_correct(rec: &ShardRecord, reference: &Hash32) -> Option<MinerAddress> {
    // The no-valid-root marker is a public literal: revealing it earns nothing (L29-F9).
    if *reference == no_valid_root_marker(&rec.shard) {
        return None;
    }
    rec.reveals
        .iter()
        .find(|(_, m, r)| r == reference && !rec.canaries.contains(m))
        .map(|(_, m, _)| *m)
}

/// Judge miners and validators of a shard again against `reference`, with
/// the rules `settle` uses (validate.rs, rule 7). Validator slashes are capped
/// with `caps` (net slashes per validator in the ledger, L29-F5). Returns the
/// entries and the net new miner slashes: what miners lose now beyond what
/// the old reference already took from them (L29-F9).
fn rejudge(
    rec: &ShardRecord,
    reference: &Hash32,
    caps: &mut BTreeMap<MinerAddress, u128>,
) -> (Vec<LedgerEntry>, u128) {
    let round = rec.round;
    let cfg = rec.validation;
    let marker = no_valid_root_marker(&rec.shard);
    let valid = *reference != marker;
    let mut out = Vec::new();
    let mut old_slash: BTreeMap<MinerAddress, u128> = BTreeMap::new();
    for e in &rec.effects {
        if let LedgerEntry::Slash {
            actor,
            amount,
            reason: "revealed a wrong root",
            ..
        } = e
        {
            *old_slash.entry(*actor).or_insert(0) += *amount;
        }
    }
    let mut miner_slashes = 0u128;
    let mut rewarded = false;
    for (_, m, r) in &rec.reveals {
        if rec.canaries.contains(m) {
            continue;
        }
        if valid && r == reference {
            if !rewarded {
                rewarded = true;
                out.push(LedgerEntry::Reward {
                    round,
                    actor: *m,
                    amount: cfg.miner_reward,
                    reason: "first correct reveal",
                });
            }
        } else {
            let bond = rec.bonds.get(m).copied().unwrap_or(0);
            let before = old_slash.get(m).copied().unwrap_or(0);
            miner_slashes = miner_slashes.saturating_add(bond.saturating_sub(before));
            out.push(LedgerEntry::Slash {
                round,
                actor: *m,
                amount: bond,
                reason: "revealed a wrong root",
            });
        }
    }
    let revealed: BTreeMap<MinerAddress, Hash32> =
        rec.reveals.iter().map(|(_, m, r)| (*m, *r)).collect();
    let mut paid: BTreeSet<(MinerAddress, Hash32)> = BTreeSet::new();
    for (v, m, verdict) in &rec.verdicts {
        let Some(r) = revealed.get(m) else { continue };
        let right = match (verdict, valid) {
            (Verdict::Abstain, _) => continue,
            (Verdict::NoValidRoot, false) => true,
            (_, false) | (Verdict::NoValidRoot, true) => false,
            (Verdict::Agree, true) => r == reference,
            (Verdict::Disagree { root }, true) => r != reference && root == reference,
        };
        if right {
            let key = if rec.canaries.contains(m) {
                Hash32::ZERO
            } else {
                *r
            };
            if paid.insert((*v, key)) {
                out.push(LedgerEntry::Reward {
                    round,
                    actor: *v,
                    amount: cfg.validator_fee,
                    reason: "correct verdict",
                });
            }
        } else {
            let used = caps.entry(*v).or_insert(0);
            let take = cfg
                .validator_slash
                .min(rec.validators.bond(v).saturating_sub(*used));
            if take > 0 {
                *used += take;
                out.push(LedgerEntry::Slash {
                    round,
                    actor: *v,
                    amount: take,
                    reason: "wrong verdict",
                });
            }
        }
    }
    (out, miner_slashes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_draw_is_deterministic_odd_and_excludes_nobody_twice() {
        let cands: BTreeSet<MinerAddress> = (0u8..10).map(|i| [i; 20]).collect();
        let seed = keccak256(b"seed");
        let id = keccak256(b"dispute");
        let beacon = keccak256(b"beacon");
        let a = draw_panel(&seed, &id, 0, &beacon, &cands, 5);
        let b = draw_panel(&seed, &id, 0, &beacon, &cands, 5);
        assert_eq!(a, b);
        assert_eq!(a.len(), 5);
        assert_eq!(a.iter().collect::<BTreeSet<_>>().len(), 5);
        // Another level draws another order.
        assert_ne!(a, draw_panel(&seed, &id, 1, &beacon, &cands, 5));
        // Fewer candidates than the size: no panel at all (L29-F2).
        let four: BTreeSet<MinerAddress> = (0u8..4).map(|i| [i; 20]).collect();
        assert!(draw_panel(&seed, &id, 0, &beacon, &four, 5).is_empty());
        assert_eq!(draw_panel(&seed, &id, 0, &beacon, &four, 3).len(), 3);
        assert!(draw_panel(&seed, &id, 0, &beacon, &BTreeSet::new(), 5).is_empty());
    }

    #[test]
    fn config_rules() {
        let ok = DisputeConfig {
            challenge_window: 151,
            min_challenge_bond: 1_000,
            panel_size: 5,
            panel_commit_window: 50,
            panel_reveal_window: 50,
            appeal_window: 50,
            max_appeals: 2,
            panel_fee: 10,
            panel_slash: 500,
        };
        assert!(ok.is_valid());
        assert!(!DisputeConfig {
            panel_size: 4,
            ..ok
        }
        .is_valid());
        assert!(!DisputeConfig {
            panel_fee: 600,
            ..ok
        }
        .is_valid());
        // L29-F7: the slash must be at least LAZY_PANEL_RATIO fees.
        assert!(!DisputeConfig {
            panel_fee: 60,
            ..ok
        }
        .is_valid());
        // L29-F3: the smallest bond must fund a panel's fees with room to spare.
        assert!(!DisputeConfig {
            min_challenge_bond: 59,
            ..ok
        }
        .is_valid());
        assert!(!DisputeConfig {
            max_appeals: MAX_APPEALS + 1,
            ..ok
        }
        .is_valid());
        assert!(!DisputeConfig {
            challenge_window: 0,
            ..ok
        }
        .is_valid());
        // L33-N6: the window must outlast one panel's commit, reveal and appeal.
        assert!(!DisputeConfig {
            challenge_window: 150,
            ..ok
        }
        .is_valid());
        assert!(!DisputeConfig {
            appeal_window: u64::MAX,
            ..ok
        }
        .is_valid());
        // M6-L2-F2: the last appeal's panel is at most MAX_PANEL (63).
        let wide = DisputeConfig {
            min_challenge_bond: 10 * 128,
            ..ok
        };
        assert!(DisputeConfig {
            panel_size: 63,
            max_appeals: 0,
            ..wide
        }
        .is_valid());
        assert!(!DisputeConfig {
            panel_size: 65,
            max_appeals: 0,
            ..wide
        }
        .is_valid());
        assert!(DisputeConfig {
            panel_size: 15,
            max_appeals: 2,
            ..wide
        }
        .is_valid());
        assert!(!DisputeConfig {
            panel_size: 17,
            max_appeals: 2,
            ..wide
        }
        .is_valid());
        assert!(!DisputeConfig {
            panel_size: 3,
            max_appeals: 5,
            ..wide
        }
        .is_valid());
    }

    /// M6-L2-F3: the appeal floor doubles from the config's minimum and covers the
    /// panel's fees; the bond a challenger chose never raises it.
    #[test]
    fn appeal_floor_is_set_by_the_config() {
        let d = DisputeConfig {
            challenge_window: 200,
            min_challenge_bond: 1_000,
            panel_size: 3,
            panel_commit_window: 50,
            panel_reveal_window: 50,
            appeal_window: 50,
            max_appeals: 2,
            panel_fee: 10,
            panel_slash: 500,
        };
        assert_eq!(d.appeal_floor(1, 7), 2_000);
        assert_eq!(d.appeal_floor(2, 15), 4_000);
        assert_eq!(
            DisputeConfig {
                panel_fee: 400,
                panel_slash: 3_600,
                ..d
            }
            .appeal_floor(1, 7),
            2_800,
            "fees above the doubled minimum"
        );
        assert_eq!(d.appeal_floor(200, 7), u128::MAX, "saturates");
        assert!(can_sit(1_000, 500, 500));
        assert!(!can_sit(1_000, 501, 500));
        assert!(
            !can_sit(100, 500, 1),
            "taken above the bond saturates to zero"
        );
    }
}
