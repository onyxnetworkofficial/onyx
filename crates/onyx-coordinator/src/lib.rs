//! Round lifecycle. Phase order fixed in M0; commit-reveal in M3, ledger in M4,
//! challenges in M5.

pub mod cas;
pub mod cast_chain;
pub mod dispute;
pub mod keyfile;
pub mod observe;
pub mod observer_link;
pub mod origin_budget;
pub mod recompute;
pub mod revenue;
pub mod round;
pub mod sealed;
pub mod signed;
pub mod signed_chain;
pub mod signed_intake;
pub mod tiers;
pub mod timelock;
pub mod track_chain;
pub mod validate;

#[cfg(test)]
pub(crate) mod test_fixture;

use borsh::{BorshDeserialize, BorshSerialize};

/// Phases of a round, in order. A round never moves backwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, BorshSerialize, BorshDeserialize)]
pub enum RoundPhase {
    /// Miners post `onyx_core::commit_hash` (ADR-0011): round, shard, commitment
    /// binding hash, miner address and salt.
    Commit,
    /// Miners reveal the full commitment and salt; the commit hash must match.
    Reveal,
    /// Validators sample, rerun and score; canaries mixed in.
    Validate,
    /// Anyone may post a bonded challenge.
    Challenge,
    /// Root final; rewards and slashes applied.
    Finalized,
}

impl RoundPhase {
    /// The next phase, or `None` when final.
    pub fn next(self) -> Option<RoundPhase> {
        match self {
            RoundPhase::Commit => Some(RoundPhase::Reveal),
            RoundPhase::Reveal => Some(RoundPhase::Validate),
            RoundPhase::Validate => Some(RoundPhase::Challenge),
            RoundPhase::Challenge => Some(RoundPhase::Finalized),
            RoundPhase::Finalized => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_only_move_forward() {
        let mut p = RoundPhase::Commit;
        let mut seen = vec![p];
        while let Some(n) = p.next() {
            assert!(n > p);
            p = n;
            seen.push(p);
        }
        assert_eq!(seen.len(), 5);
    }
}
