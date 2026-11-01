//! Epoch accounting: who was asked for what, who passed, failed or missed,
//! and what each node earned and lost (R-STOR-10). Built from the
//! challenges the placement implies ([`expected`]) and the contract's
//! `Answered` and `Slashed` events ([`Outcome`]).

use crate::ids::{slot_shard, NodeId};
use crate::placement::{challenged, Exclusions, Jailed};
use onyx_core::Hash32;

/// One challenge: a node asked for a slot of a deal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ask {
    /// Deal.
    pub deal: u64,
    /// Slot.
    pub slot: u32,
    /// The challenged node.
    pub node: NodeId,
    /// The shard the slot picked.
    pub index: u32,
}

/// What a deal contributes to the epoch's challenges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DealAsk {
    /// Deal id.
    pub id: u64,
    /// Placement salt.
    pub salt: Hash32,
    /// Shards.
    pub shard_count: u32,
    /// Replicas.
    pub r: u8,
}

/// Every challenge of epoch `epoch` (seed `seed`, node list of the epoch
/// before `prev_nodes`), sorted.
pub fn expected(
    epoch: u64,
    seed: &Hash32,
    deals: &[DealAsk],
    challenges_per_deal: u32,
    prev_nodes: &[NodeId],
    excl: &Exclusions,
    jailed: &Jailed,
) -> Vec<Ask> {
    let mut out = Vec::new();
    for d in deals {
        for slot in 0..challenges_per_deal {
            let index = slot_shard(seed, d.id, slot, d.shard_count);
            for node in challenged(&d.salt, d.id, index, prev_nodes, d.r, epoch, excl, jailed) {
                out.push(Ask {
                    deal: d.id,
                    slot,
                    node,
                    index,
                });
            }
        }
    }
    out.sort();
    out
}

/// How a challenge ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Answered and proven: paid.
    Passed(u128),
    /// Answered with a proof that does not hold: slashed.
    Failed(u128),
    /// Not answered in the epoch, settled later: slashed.
    Missed(u128),
}

/// One node's line in the epoch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Line {
    /// Challenges asked.
    pub asked: u32,
    /// Passed.
    pub passed: u32,
    /// Failed.
    pub failed: u32,
    /// Missed (settled).
    pub missed: u32,
    /// Earned.
    pub earned: u128,
    /// Slashed.
    pub slashed: u128,
}

/// The accounts of one epoch, by node (sorted).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EpochAccounts {
    lines: Vec<(NodeId, Line)>,
    open: Vec<Ask>,
}

impl EpochAccounts {
    /// Accounts for these challenges, none answered yet.
    pub fn new(asks: &[Ask]) -> Self {
        let mut a = EpochAccounts {
            lines: Vec::new(),
            open: asks.to_vec(),
        };
        a.open.sort();
        for k in asks {
            a.line_mut(&k.node).asked += 1;
        }
        a
    }

    fn line_mut(&mut self, n: &NodeId) -> &mut Line {
        let i = match self.lines.binary_search_by(|e| e.0.cmp(n)) {
            Ok(i) => i,
            Err(i) => {
                self.lines.insert(i, (*n, Line::default()));
                i
            }
        };
        &mut self.lines[i].1
    }

    /// Record an outcome for (deal, slot, node). An outcome for a challenge
    /// that was not asked, or asked and already recorded, is refused (the
    /// contract refuses it too).
    pub fn record(&mut self, deal: u64, slot: u32, node: &NodeId, o: Outcome) -> bool {
        let Some(i) = self
            .open
            .iter()
            .position(|a| a.deal == deal && a.slot == slot && a.node == *node)
        else {
            return false;
        };
        self.open.remove(i);
        let l = self.line_mut(node);
        match o {
            Outcome::Passed(x) => {
                l.passed += 1;
                l.earned += x;
            }
            Outcome::Failed(x) => {
                l.failed += 1;
                l.slashed += x;
            }
            Outcome::Missed(x) => {
                l.missed += 1;
                l.slashed += x;
            }
        }
        true
    }

    /// Challenges not answered or settled yet.
    pub fn open(&self) -> &[Ask] {
        &self.open
    }

    /// A node's line.
    pub fn line(&self, n: &NodeId) -> Line {
        self.lines
            .binary_search_by(|e| e.0.cmp(n))
            .map(|i| self.lines[i].1)
            .unwrap_or_default()
    }

    /// Every line.
    pub fn lines(&self) -> &[(NodeId, Line)] {
        &self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::node_id;

    #[test]
    fn every_ask_is_accounted_once() {
        let ns: Vec<NodeId> = (0..5)
            .map(|i| node_id(&[1; 20], i, &Hash32([2; 32])))
            .collect();
        let deals = vec![
            DealAsk {
                id: 1,
                salt: Hash32([3; 32]),
                shard_count: 7,
                r: 3,
            },
            DealAsk {
                id: 2,
                salt: Hash32([4; 32]),
                shard_count: 1,
                r: 3,
            },
        ];
        let asks = expected(
            9,
            &Hash32([5; 32]),
            &deals,
            2,
            &ns,
            &Exclusions::new(),
            &Jailed::new(),
        );
        assert_eq!(asks.len(), 2 * 2 * 3);
        let mut acc = EpochAccounts::new(&asks);
        let total: u32 = acc.lines().iter().map(|(_, l)| l.asked).sum();
        assert_eq!(total, 12);
        let a = asks[0];
        assert!(acc.record(a.deal, a.slot, &a.node, Outcome::Passed(10)));
        assert!(
            !acc.record(a.deal, a.slot, &a.node, Outcome::Passed(10)),
            "twice"
        );
        let b = asks[1];
        assert!(acc.record(b.deal, b.slot, &b.node, Outcome::Missed(4)));
        assert_eq!(acc.open().len(), 10);
        let stranger = node_id(&[9; 20], 0, &Hash32([2; 32]));
        assert!(!acc.record(1, 0, &stranger, Outcome::Passed(1)));
        let earned: u128 = acc.lines().iter().map(|(_, l)| l.earned).sum();
        assert_eq!(earned, 10);
    }
}
