//! Repair planning (R-STOR-12): which shard copies must move when the node
//! list or the exclusions change, and where each new holder fetches from.
//!
//! A move is `(deal, shard) -> new holder`, with the old holders that still
//! hold it as sources (any of them, or the publisher, serves the file; the
//! new holder checks every byte against the manifest). Rendezvous placement
//! keeps moves to the shards whose top R changed, and spreads a leaving
//! node's shards over the remaining nodes rather than onto one
//! (THREATS STORAGE-9, repair storms).

use crate::ids::NodeId;
use crate::placement::{holders_at, Exclusions};
use onyx_core::Hash32;

/// What placement needs of a deal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DealPlace {
    /// Deal id.
    pub id: u64,
    /// The deal's placement salt.
    pub salt: Hash32,
    /// Shards in its manifest.
    pub shard_count: u32,
    /// Replicas.
    pub r: u8,
}

/// One copy to make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Move {
    /// Deal.
    pub deal: u64,
    /// Shard index.
    pub index: u32,
    /// The new holder.
    pub to: NodeId,
    /// Holders before that still hold it (fetch sources, in rank order).
    pub sources: Vec<NodeId>,
}

/// Every (deal, shard) `node` holds under `nodes` at `epoch`.
pub fn assignments(
    node: &NodeId,
    deals: &[DealPlace],
    nodes: &[NodeId],
    epoch: u64,
    excl: &Exclusions,
) -> Vec<(u64, u32)> {
    let mut out = Vec::new();
    if !nodes.contains(node) {
        return out;
    }
    for d in deals {
        for i in 0..d.shard_count {
            if holders_at(&d.salt, d.id, i, nodes, d.r, epoch, excl).contains(node) {
                out.push((d.id, i));
            }
        }
    }
    out
}

/// The moves from the placement of (`before`, `epoch_before`) to that of
/// (`after`, `epoch_after`), with the same exclusion record.
pub fn plan(
    deals: &[DealPlace],
    before: &[NodeId],
    epoch_before: u64,
    after: &[NodeId],
    epoch_after: u64,
    excl: &Exclusions,
) -> Vec<Move> {
    let mut out = Vec::new();
    for d in deals {
        for i in 0..d.shard_count {
            let old = holders_at(&d.salt, d.id, i, before, d.r, epoch_before, excl);
            let new = holders_at(&d.salt, d.id, i, after, d.r, epoch_after, excl);
            let sources: Vec<NodeId> = old
                .iter()
                .copied()
                .filter(|n| {
                    after.contains(n) && excl.from(d.id, i, n).is_none_or(|f| f > epoch_after)
                })
                .collect();
            for n in new.iter().filter(|n| !old.contains(n)) {
                out.push(Move {
                    deal: d.id,
                    index: i,
                    to: *n,
                    sources: sources.clone(),
                });
            }
        }
    }
    out
}

/// Moves per receiving node, sorted by node.
pub fn load(moves: &[Move]) -> Vec<(NodeId, u32)> {
    let mut v: Vec<(NodeId, u32)> = Vec::new();
    for m in moves {
        match v.binary_search_by(|e| e.0.cmp(&m.to)) {
            Ok(i) => v[i].1 += 1,
            Err(i) => v.insert(i, (m.to, 1)),
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::node_id;

    fn nodes(n: u64) -> Vec<NodeId> {
        (0..n)
            .map(|i| node_id(&[2; 20], i, &Hash32([6; 32])))
            .collect()
    }

    fn deals() -> Vec<DealPlace> {
        vec![
            DealPlace {
                id: 1,
                salt: Hash32([1; 32]),
                shard_count: 1_500,
                r: 3,
            },
            DealPlace {
                id: 2,
                salt: Hash32([2; 32]),
                shard_count: 1_500,
                r: 3,
            },
        ]
    }

    #[test]
    fn a_failed_node_moves_exactly_its_shards_spread_over_the_rest() {
        let ns = nodes(10);
        let ex = Exclusions::new();
        let gone = ns[3];
        let held = assignments(&gone, &deals(), &ns, 5, &ex);
        let rest: Vec<NodeId> = ns.iter().copied().filter(|n| *n != gone).collect();
        let moves = plan(&deals(), &ns, 5, &rest, 6, &ex);
        assert_eq!(moves.len(), held.len());
        for m in &moves {
            assert_eq!(m.sources.len(), 2, "two surviving copies to fetch from");
            assert!(!m.sources.contains(&gone));
            assert!(held.contains(&(m.deal, m.index)));
        }
        // No repair storm onto one node: 9 receivers, none takes more than
        // twice its fair share.
        let l = load(&moves);
        assert_eq!(l.len(), 9);
        let fair = held.len() as u32 / 9;
        assert!(l.iter().all(|(_, c)| *c <= 2 * fair), "{l:?}");
    }

    #[test]
    fn an_exclusion_moves_one_copy() {
        let ns = nodes(8);
        let mut ex = Exclusions::new();
        let d = deals();
        let h = holders_at(&d[0].salt, 1, 42, &ns, 3, 5, &ex);
        ex.insert(1, 42, h[1], 6);
        let moves = plan(&d, &ns, 5, &ns, 6, &ex);
        assert_eq!(moves.len(), 1);
        assert_eq!((moves[0].deal, moves[0].index), (1, 42));
        assert_eq!(moves[0].sources, vec![h[0], h[2]]);
    }

    #[test]
    fn nothing_moves_when_nothing_changes() {
        let ns = nodes(6);
        assert!(plan(&deals(), &ns, 3, &ns, 4, &Exclusions::new()).is_empty());
    }
}
