//! Shard placement by rendezvous (highest-random-weight) hashing.
//!
//! Every node gets a weight per shard ([`ids::weight`]); the shard's holders
//! are the R nodes with the highest weights, skipping nodes excluded from
//! that shard (a node that failed a proof for it). A node that joins takes
//! only the shards where it now ranks in the top R; a node that leaves gives
//! up only its own shards, each to the next node in that shard's ranking.
//! Nothing else moves (tested below and in [`crate::repair`]).
//!
//! Epoch rule (R-STOR-6): challenges in epoch `e` use the node list of epoch
//! `e - 1` (`P(e-1)`, fixed when epoch `e - 2` ended), so every holder had
//! all of epoch `e - 1` to fetch what it newly holds. [`challenged`] is that
//! rule, exactly as `OnyxStorage._challenged` computes it.

use crate::ids::{weight, NodeId};
use onyx_core::Hash32;

/// `nodes` ordered by weight for `shard_key`, highest first (ties, which
/// need a Keccak collision, by id).
pub fn ranked(shard_key: &Hash32, nodes: &[NodeId]) -> Vec<NodeId> {
    let mut w: Vec<(Hash32, NodeId)> = nodes.iter().map(|n| (weight(shard_key, n), *n)).collect();
    w.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    w.into_iter().map(|(_, n)| n).collect()
}

/// The first `r` nodes of [`ranked`] that `excluded` does not name.
pub fn holders(
    shard_key: &Hash32,
    nodes: &[NodeId],
    r: u8,
    excluded: impl Fn(&NodeId) -> bool,
) -> Vec<NodeId> {
    ranked(shard_key, nodes)
        .into_iter()
        .filter(|n| !excluded(n))
        .take(usize::from(r))
        .collect()
}

/// The shard index a node declines a whole deal with (`OnyxStorage.WHOLE_DEAL`).
pub const WHOLE_DEAL: u32 = u32::MAX;

/// Per-(deal, shard) exclusions: the epoch from which a node no longer holds
/// a shard it failed or declined, or a whole deal it declined
/// ([`WHOLE_DEAL`]; `OnyxStorage.exclusionFrom`). Sorted, one entry per
/// (deal, shard, node).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exclusions(Vec<(u64, u32, NodeId, u64)>);

impl Exclusions {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `node` is excluded from shard `index` of `deal` from
    /// epoch `from` on (the first record wins, as on chain).
    pub fn insert(&mut self, deal: u64, index: u32, node: NodeId, from: u64) {
        let k = (deal, index, node);
        match self.0.binary_search_by(|e| (e.0, e.1, e.2).cmp(&k)) {
            Ok(_) => {}
            Err(at) => self.0.insert(at, (deal, index, node, from)),
        }
    }

    fn exact(&self, deal: u64, index: u32, node: &NodeId) -> Option<u64> {
        let k = (deal, index, *node);
        self.0
            .binary_search_by(|e| (e.0, e.1, e.2).cmp(&k))
            .ok()
            .map(|i| self.0[i].3)
    }

    /// The epoch from which `node` is excluded from the shard, if any: the
    /// earlier of its exclusion from that shard and its decline of the whole
    /// deal ([`WHOLE_DEAL`]), as `OnyxStorage.exclusionFrom`.
    pub fn from(&self, deal: u64, index: u32, node: &NodeId) -> Option<u64> {
        let a = self.exact(deal, index, node);
        let b = self.exact(deal, WHOLE_DEAL, node);
        match (a, b) {
            (Some(x), Some(y)) => Some(x.min(y)),
            (x, y) => x.or(y),
        }
    }

    /// Every entry, sorted.
    pub fn entries(&self) -> &[(u64, u32, NodeId, u64)] {
        &self.0
    }
}

/// Jailed nodes and the epoch from which they are out (sorted by id).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Jailed(Vec<(NodeId, u64)>);

impl Jailed {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a jailed node.
    pub fn insert(&mut self, node: NodeId, from: u64) {
        match self.0.binary_search_by(|e| e.0.cmp(&node)) {
            Ok(_) => {}
            Err(at) => self.0.insert(at, (node, from)),
        }
    }

    /// From when `node` is jailed, if it is.
    pub fn from(&self, node: &NodeId) -> Option<u64> {
        self.0
            .binary_search_by(|e| e.0.cmp(node))
            .ok()
            .map(|i| self.0[i].1)
    }
}

/// Holders of shard `index` of a deal for the placement of epoch `epoch`
/// (`nodes` = that epoch's list): the top `r` skipping nodes excluded from
/// the shard by `epoch`.
pub fn holders_at(
    salt: &Hash32,
    deal: u64,
    index: u32,
    nodes: &[NodeId],
    r: u8,
    epoch: u64,
    excl: &Exclusions,
) -> Vec<NodeId> {
    let key = crate::ids::shard_key(salt, index);
    holders(&key, nodes, r, |n| {
        excl.from(deal, index, n).is_some_and(|f| f <= epoch)
    })
}

/// The nodes challenged for shard `index` of a deal in epoch `epoch`
/// (R-STOR-6): the holders under `prev_nodes` (the list of epoch
/// `epoch - 1`) with the exclusions in force at `epoch - 1`, less any node
/// excluded from the shard or jailed by `epoch` itself (a node just caught is
/// not asked again in the epoch its replacement is still fetching).
#[allow(clippy::too_many_arguments)]
pub fn challenged(
    salt: &Hash32,
    deal: u64,
    index: u32,
    prev_nodes: &[NodeId],
    r: u8,
    epoch: u64,
    excl: &Exclusions,
    jailed: &Jailed,
) -> Vec<NodeId> {
    holders_at(
        salt,
        deal,
        index,
        prev_nodes,
        r,
        epoch.saturating_sub(1),
        excl,
    )
    .into_iter()
    .filter(|n| {
        !excl.from(deal, index, n).is_some_and(|f| f <= epoch)
            && !jailed.from(n).is_some_and(|f| f <= epoch)
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{node_id, shard_key};

    pub(crate) fn nodes(n: u64) -> Vec<NodeId> {
        (0..n)
            .map(|i| node_id(&[9; 20], i, &Hash32([3; 32])))
            .collect()
    }

    fn keys(n: u32) -> Vec<Hash32> {
        (0..n).map(|i| shard_key(&Hash32([5; 32]), i)).collect()
    }

    #[test]
    fn holders_are_r_distinct_and_order_free() {
        let ns = nodes(10);
        let mut rev = ns.clone();
        rev.reverse();
        for k in keys(200) {
            let h = holders(&k, &ns, 3, |_| false);
            assert_eq!(h.len(), 3);
            let mut d = h.clone();
            d.sort();
            d.dedup();
            assert_eq!(d.len(), 3);
            // The list order never changes the answer.
            assert_eq!(h, holders(&k, &rev, 3, |_| false));
        }
        // Fewer nodes than R: every node.
        assert_eq!(holders(&keys(1)[0], &ns[..2], 3, |_| false).len(), 2);
    }

    #[test]
    fn a_join_moves_only_shards_the_new_node_wins() {
        let ns = nodes(10);
        let mut more = ns.clone();
        let newcomer = node_id(&[8; 20], 0, &Hash32([4; 32]));
        more.push(newcomer);
        let ks = keys(5_000);
        let mut moved = 0;
        for k in &ks {
            let a = holders(k, &ns, 3, |_| false);
            let b = holders(k, &more, 3, |_| false);
            if a != b {
                // The only change: the newcomer in, one old holder out.
                assert!(b.contains(&newcomer));
                assert_eq!(a.iter().filter(|n| !b.contains(n)).count(), 1);
                moved += 1;
            } else {
                assert!(!b.contains(&newcomer));
            }
        }
        // Expected share R / (N + 1) = 3/11 = 27 %: within 22..33 % for 5,000 shards.
        assert!((1_100..=1_650).contains(&moved), "moved {moved}");
    }

    #[test]
    fn a_leave_moves_only_the_leavers_shards() {
        let ns = nodes(10);
        let gone = ns[4];
        let rest: Vec<NodeId> = ns.iter().copied().filter(|n| *n != gone).collect();
        for k in keys(2_000) {
            let a = holders(&k, &ns, 3, |_| false);
            let b = holders(&k, &rest, 3, |_| false);
            if a.contains(&gone) {
                // The two other holders stay; one new node joins them.
                assert_eq!(a.iter().filter(|n| b.contains(n)).count(), 2);
            } else {
                assert_eq!(a, b);
            }
        }
    }

    #[test]
    fn load_is_even() {
        // 10 nodes, R = 3, 10,000 shards: each node holds about 3,000.
        let ns = nodes(10);
        let mut count = vec![0u32; 10];
        for k in keys(10_000) {
            for h in holders(&k, &ns, 3, |_| false) {
                let i = ns.iter().position(|n| *n == h).expect("listed");
                count[i] += 1;
            }
        }
        for c in count {
            assert!((2_700..=3_300).contains(&c), "{c}");
        }
    }

    /// THREATS STORAGE-4: an operator running 3 of 10 nodes (ids it cannot
    /// choose) holds all three copies of a shard about (3/10)(2/9)(1/8) =
    /// 0.83% of the time, not more: rendezvous gives it no say.
    #[test]
    fn colluding_nodes_hold_all_copies_only_by_chance() {
        let ns = nodes(10);
        let bad = &ns[..3];
        let total = 20_000u32;
        let all = keys(total)
            .iter()
            .filter(|k| {
                holders(k, &ns, 3, |_| false)
                    .iter()
                    .all(|h| bad.contains(h))
            })
            .count() as u32;
        // Expected 167 of 20,000; allow for sampling noise.
        assert!((100..=240).contains(&all), "{all}");
    }

    #[test]
    fn exclusion_moves_one_shard_to_the_next_ranked() {
        let ns = nodes(6);
        let salt = Hash32([5; 32]);
        let mut ex = Exclusions::new();
        let before = holders_at(&salt, 1, 7, &ns, 3, 10, &ex);
        ex.insert(1, 7, before[0], 11);
        // Not yet in force at epoch 10; in force from 11.
        assert_eq!(holders_at(&salt, 1, 7, &ns, 3, 10, &ex), before);
        let after = holders_at(&salt, 1, 7, &ns, 3, 11, &ex);
        let ranking = ranked(&shard_key(&salt, 7), &ns);
        assert_eq!(after, ranking[1..4].to_vec());
        // Other shards keep their holders.
        assert_eq!(
            holders_at(&salt, 1, 8, &ns, 3, 11, &ex),
            holders_at(&salt, 1, 8, &ns, 3, 11, &Exclusions::new())
        );
        // The first record wins.
        ex.insert(1, 7, before[0], 99);
        assert_eq!(ex.from(1, 7, &before[0]), Some(11));
    }

    /// THREATS STORAGE-16: a node that declines a whole deal leaves every
    /// shard of it from the declined epoch on; the earlier exclusion wins.
    #[test]
    fn declining_a_whole_deal_excludes_every_shard() {
        let ns = nodes(6);
        let salt = Hash32([5; 32]);
        let mut ex = Exclusions::new();
        let me = holders_at(&salt, 3, 0, &ns, 3, 1, &ex)[0];
        ex.insert(3, WHOLE_DEAL, me, 4);
        for i in 0..50 {
            assert!(!holders_at(&salt, 3, i, &ns, 3, 4, &ex).contains(&me));
            assert_eq!(ex.from(3, i, &me), Some(4));
            // Another deal is untouched.
            assert_eq!(ex.from(4, i, &me), None);
        }
        assert_eq!(
            holders_at(&salt, 3, 0, &ns, 3, 3, &ex)[0],
            me,
            "not before its epoch"
        );
        ex.insert(3, 7, me, 2);
        assert_eq!(ex.from(3, 7, &me), Some(2));
    }

    #[test]
    fn challenged_follows_the_epoch_rule() {
        let ns = nodes(6);
        let salt = Hash32([5; 32]);
        let mut ex = Exclusions::new();
        let jail = Jailed::new();
        let h = holders_at(&salt, 1, 2, &ns, 3, 4, &ex);
        assert_eq!(challenged(&salt, 1, 2, &ns, 3, 5, &ex, &jail), h);
        // Caught in epoch 5 (excluded from 6): not asked in 6, its
        // replacement is fetching; the replacement is asked from 7.
        ex.insert(1, 2, h[0], 6);
        let at6 = challenged(&salt, 1, 2, &ns, 3, 6, &ex, &jail);
        assert_eq!(at6, h[1..].to_vec());
        let at7 = challenged(&salt, 1, 2, &ns, 3, 7, &ex, &jail);
        assert_eq!(at7.len(), 3);
        assert!(!at7.contains(&h[0]));
        // A jailed node is not asked from its jail epoch.
        let mut jail = Jailed::new();
        jail.insert(h[1], 5);
        assert!(!challenged(&salt, 1, 2, &ns, 3, 5, &Exclusions::new(), &jail).contains(&h[1]));
    }
}
