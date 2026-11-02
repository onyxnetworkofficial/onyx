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
use std::collections::{BTreeMap, BTreeSet};

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

/// The window K of ADR-0060 amendment 2 item 2 (`OnyxStorage.NOTICE_EPOCHS`):
/// a decline of a shard the node passed a check on in the `NOTICE_EPOCHS`
/// epochs before the decline takes effect one epoch later.
pub const NOTICE_EPOCHS: u64 = 7;

/// A node's raw record for one shard of a deal (`OnyxStorage.Excl`); the
/// record of [`WHOLE_DEAL`] uses only `decline_from`. 0 means none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Excl {
    /// The epoch after the node's decline, before any notice.
    pub decline_from: u64,
    /// The epoch after its first slash on the shard.
    pub slash_from: u64,
    /// The last epoch it passed a check on the shard.
    pub last_pass: u64,
}

/// A decline effective from `f` (0: none), one epoch later when the last
/// pass `p` (0: none) is at most [`NOTICE_EPOCHS`] before the decline's
/// epoch `f - 1` (`OnyxStorage._noticed`).
fn noticed(f: u64, p: u64) -> u64 {
    if f == 0 {
        return 0;
    }
    if p != 0 && p.saturating_add(NOTICE_EPOCHS) >= f - 1 {
        f + 1
    } else {
        f
    }
}

/// The earlier of two epochs, 0 meaning none.
fn earlier(a: u64, b: u64) -> u64 {
    match (a, b) {
        (0, x) | (x, 0) => x,
        (x, y) => x.min(y),
    }
}

/// The raw records behind `OnyxStorage.exclusionFrom`, per (deal, shard,
/// node): a decline, the first slash and the last pass, from the
/// `Declined`, `Slashed` and passed `Answered` events. The order the events
/// are read in never changes the result: a decline and a slash keep the
/// earliest epoch, a pass the latest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exclusions(BTreeMap<(u64, u32, NodeId), Excl>);

impl Exclusions {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    fn at(&mut self, deal: u64, index: u32, node: NodeId) -> &mut Excl {
        self.0.entry((deal, index, node)).or_default()
    }

    /// A `Declined` event: `node` gave up shard `index` of `deal` (or the
    /// whole deal, [`WHOLE_DEAL`]) with the raw epoch `from`.
    pub fn decline(&mut self, deal: u64, index: u32, node: NodeId, from: u64) {
        let x = self.at(deal, index, node);
        x.decline_from = earlier(x.decline_from, from);
    }

    /// A `Slashed` event: `node` is out of shard `index` from `from` (raw).
    pub fn slash(&mut self, deal: u64, index: u32, node: NodeId, from: u64) {
        let x = self.at(deal, index, node);
        x.slash_from = earlier(x.slash_from, from);
    }

    /// A passed `Answered` event: `node` passed a check on shard `index` in
    /// epoch `epoch`.
    pub fn pass(&mut self, deal: u64, index: u32, node: NodeId, epoch: u64) {
        let x = self.at(deal, index, node);
        x.last_pass = x.last_pass.max(epoch);
    }

    /// The raw record of (deal, index, node), if any.
    pub fn record(&self, deal: u64, index: u32, node: &NodeId) -> Option<Excl> {
        self.0.get(&(deal, index, *node)).copied()
    }

    /// The epoch from which `node` is excluded from shard `index`, if any,
    /// exactly as `OnyxStorage.exclusionFrom`: the earliest of its slash on
    /// the shard, its decline of the shard and its decline of the whole deal,
    /// each decline one epoch later when the node passed this shard in the
    /// [`NOTICE_EPOCHS`] epochs before the decline.
    pub fn from(&self, deal: u64, index: u32, node: &NodeId) -> Option<u64> {
        let x = self.record(deal, index, node).unwrap_or_default();
        let mut f = earlier(x.slash_from, noticed(x.decline_from, x.last_pass));
        if index != WHOLE_DEAL {
            let w = self
                .record(deal, WHOLE_DEAL, node)
                .map_or(0, |r| r.decline_from);
            f = earlier(f, noticed(w, x.last_pass));
        }
        (f != 0).then_some(f)
    }

    /// True when `node` is out of shard `index` by epoch `e` even if every
    /// decline of it took the notice (counted one epoch late): the contract's
    /// `exclusionFrom` is at most that, so it cannot ask the node in `e`
    /// whatever passes the view missed. The node asks the contract only when
    /// this is false (review STORAGE-S16-R7 L2).
    pub fn out_by_any_notice(&self, deal: u64, index: u32, node: &NodeId, e: u64) -> bool {
        let late = |f: u64| if f == 0 { 0 } else { f + 1 };
        let x = self.record(deal, index, node).unwrap_or_default();
        let mut f = earlier(x.slash_from, late(x.decline_from));
        if index != WHOLE_DEAL {
            let w = self
                .record(deal, WHOLE_DEAL, node)
                .map_or(0, |r| r.decline_from);
            f = earlier(f, late(w));
        }
        f != 0 && f <= e
    }

    /// The records that can change a placement: every one with a decline or
    /// a slash, and the passes of nodes that declined that deal whole. A pass
    /// alone changes nothing, so a node keeps its placement when only passes
    /// were added (review STORAGE-R2-5).
    pub fn placement_view(&self) -> Exclusions {
        let whole: BTreeSet<(u64, NodeId)> = self
            .0
            .iter()
            .filter(|(k, x)| k.1 == WHOLE_DEAL && x.decline_from != 0)
            .map(|(k, _)| (k.0, k.2))
            .collect();
        Exclusions(
            self.0
                .iter()
                .filter(|(k, x)| {
                    x.decline_from != 0 || x.slash_from != 0 || whole.contains(&(k.0, k.2))
                })
                .map(|(k, x)| (*k, *x))
                .collect(),
        )
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
        ex.slash(1, 7, before[0], 11);
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
        // The earliest slash wins, whatever order the events are read in.
        ex.slash(1, 7, before[0], 99);
        assert_eq!(ex.from(1, 7, &before[0]), Some(11));
        ex.slash(1, 7, before[0], 9);
        assert_eq!(ex.from(1, 7, &before[0]), Some(9));
    }

    /// THREATS STORAGE-16: a node that declines a whole deal leaves every
    /// shard of it from the declined epoch on; the earlier exclusion wins.
    #[test]
    fn declining_a_whole_deal_excludes_every_shard() {
        let ns = nodes(6);
        let salt = Hash32([5; 32]);
        let mut ex = Exclusions::new();
        let me = holders_at(&salt, 3, 0, &ns, 3, 1, &ex)[0];
        ex.decline(3, WHOLE_DEAL, me, 4);
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
        ex.slash(3, 7, me, 2);
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
        ex.slash(1, 2, h[0], 6);
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

    /// ADR-0060 amendment 2 item 2, as `OnyxStorageNotice.t.sol`: the same
    /// epochs give the same `exclusionFrom`.
    #[test]
    fn a_decline_after_a_recent_pass_is_noticed() {
        let ns = nodes(5);
        let salt = Hash32([5; 32]);
        let jail = Jailed::new();
        let e = 2;
        let x = holders_at(&salt, 0, 0, &ns, 3, e, &Exclusions::new())[0];
        // Passed at e, declined at c = e + 1 (raw c + 1): out from c + 2.
        let mut ex = Exclusions::new();
        ex.pass(0, 0, x, e);
        let c = e + 1;
        ex.decline(0, 0, x, c + 1);
        assert_eq!(ex.from(0, 0, &x), Some(c + 2));
        // Still asked at c + 1; a pass then does not stretch the notice.
        assert!(challenged(&salt, 0, 0, &ns, 3, c + 1, &ex, &jail).contains(&x));
        ex.pass(0, 0, x, c + 1);
        assert_eq!(ex.from(0, 0, &x), Some(c + 2));
        let at2 = challenged(&salt, 0, 0, &ns, 3, c + 2, &ex, &jail);
        assert!(!at2.contains(&x));
        assert_eq!(at2.len(), 2);
        let at3 = challenged(&salt, 0, 0, &ns, 3, c + 3, &ex, &jail);
        assert!(!at3.contains(&x));
        assert_eq!(at3.len(), 3);
    }

    #[test]
    fn no_notice_without_a_pass() {
        let x = nodes(1)[0];
        let mut ex = Exclusions::new();
        ex.decline(0, 0, x, 3);
        assert_eq!(ex.from(0, 0, &x), Some(3));
        // A pass on another shard or another deal is no pass on this one.
        ex.pass(0, 1, x, 2);
        ex.pass(1, 0, x, 2);
        assert_eq!(ex.from(0, 0, &x), Some(3));
    }

    #[test]
    fn no_notice_when_the_last_pass_is_older_than_the_window() {
        let ns = nodes(2);
        let (x, y, e, k) = (ns[0], ns[1], 2, NOTICE_EPOCHS);
        let mut ex = Exclusions::new();
        ex.pass(0, 0, x, e);
        ex.pass(0, 0, y, e);
        // x declines at e + K (in the window), y at e + K + 1 (one epoch too old).
        ex.decline(0, 0, x, e + k + 1);
        ex.decline(0, 0, y, e + k + 2);
        assert_eq!(ex.from(0, 0, &x), Some(e + k + 2));
        assert_eq!(ex.from(0, 0, &y), Some(e + k + 2));
    }

    #[test]
    fn a_whole_deal_decline_delays_only_shards_passed_in_the_window() {
        let ns = nodes(2);
        let (x, z, e) = (ns[0], ns[1], 2);
        let c = e + 1;
        let mut ex = Exclusions::new();
        ex.pass(0, 1, x, e);
        ex.decline(0, WHOLE_DEAL, x, c + 1);
        assert_eq!(ex.from(0, 1, &x), Some(c + 2), "the passed shard");
        assert_eq!(ex.from(0, 0, &x), Some(c + 1), "the shard never passed");
        assert_eq!(ex.from(0, WHOLE_DEAL, &x), Some(c + 1), "the raw record");
        ex.decline(0, WHOLE_DEAL, z, c + 1);
        assert_eq!(ex.from(0, 0, &z), Some(c + 1));
        assert_eq!(ex.from(0, 1, &z), Some(c + 1));
    }

    #[test]
    fn a_pass_in_the_decline_epoch_after_the_decline_delays_it() {
        let (x, e) = (nodes(1)[0], 2);
        let mut ex = Exclusions::new();
        ex.decline(0, 0, x, e + 1);
        assert_eq!(ex.from(0, 0, &x), Some(e + 1));
        ex.pass(0, 0, x, e);
        assert_eq!(ex.from(0, 0, &x), Some(e + 2));
        ex.pass(0, 0, x, e + 1);
        assert_eq!(ex.from(0, 0, &x), Some(e + 2));
    }

    /// Review STORAGE-S16-R6 M2: pass at c - 1, miss at c, decline at c + 1
    /// (noticed: c + 3), the miss settled at c + 1 (slash: c + 2): the slash
    /// wins, whichever event is read first.
    #[test]
    fn a_slash_after_a_noticed_decline_pulls_the_exclusion_in() {
        let (x, e) = (nodes(1)[0], 2);
        let c = e + 1;
        let mut a = Exclusions::new();
        a.pass(0, 0, x, e);
        a.decline(0, 0, x, c + 2);
        assert_eq!(a.from(0, 0, &x), Some(c + 3));
        a.slash(0, 0, x, c + 2);
        assert_eq!(a.from(0, 0, &x), Some(c + 2));
        let mut b = Exclusions::new();
        b.slash(0, 0, x, c + 2);
        b.decline(0, 0, x, c + 2);
        b.pass(0, 0, x, e);
        assert_eq!(a, b);
    }

    /// Logs come in any order: the latest pass counts, never the first or
    /// the last read.
    #[test]
    fn the_latest_pass_counts_whatever_the_order() {
        let x = nodes(1)[0];
        for order in [[2, 20], [20, 2]] {
            let mut ex = Exclusions::new();
            for p in order {
                ex.pass(0, 0, x, p);
            }
            // Declined at 20: the pass at 20 is in the window, the one at 2 is not.
            ex.decline(0, 0, x, 21);
            assert_eq!(ex.from(0, 0, &x), Some(22), "{order:?}");
        }
    }

    /// Passes alone never change the placement's inputs; a pass of a node
    /// that declined the deal whole does.
    #[test]
    fn the_placement_view_drops_passes_that_change_nothing() {
        let ns = nodes(2);
        let mut ex = Exclusions::new();
        ex.pass(0, 0, ns[0], 4);
        ex.pass(0, 1, ns[1], 4);
        assert_eq!(ex.placement_view(), Exclusions::new());
        ex.decline(0, WHOLE_DEAL, ns[1], 6);
        let v = ex.placement_view();
        assert_eq!(v.record(0, 1, &ns[1]).map(|r| r.last_pass), Some(4));
        assert_eq!(v.record(0, 0, &ns[0]), None);
        for i in [0, 1] {
            for n in &ns {
                assert_eq!(v.from(0, i, n), ex.from(0, i, n));
            }
        }
    }
}

#[cfg(test)]
mod notice_bound_tests {
    use super::*;
    use onyx_core::Hash32;

    /// `out_by_any_notice` is never true where `from` would let the contract
    /// ask, whatever the passes (review STORAGE-S16-R7 L2).
    #[test]
    fn out_by_any_notice_bounds_every_reading() {
        let n = NodeId(Hash32([3; 32]));
        for pass in [0u64, 1, 4, 9] {
            for decline in [0u64, 5, 10] {
                for whole in [0u64, 6] {
                    for slash in [0u64, 7] {
                        let mut x = Exclusions::new();
                        if decline != 0 {
                            x.decline(1, 2, n, decline);
                        }
                        if whole != 0 {
                            x.decline(1, WHOLE_DEAL, n, whole);
                        }
                        if slash != 0 {
                            x.slash(1, 2, n, slash);
                        }
                        if pass != 0 {
                            x.pass(1, 2, n, pass);
                        }
                        for e in 0..14u64 {
                            if x.out_by_any_notice(1, 2, &n, e) {
                                let f = x.from(1, 2, &n).expect("excluded");
                                assert!(f <= e, "pass {pass} decline {decline} whole {whole} slash {slash} e {e}");
                            }
                        }
                        // The bound is at most one epoch later than the real exclusion.
                        if let Some(f) = x.from(1, 2, &n) {
                            assert!(x.out_by_any_notice(1, 2, &n, f + 1));
                        }
                    }
                }
            }
        }
    }
}
