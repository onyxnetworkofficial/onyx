//! The hashes `OnyxStorage` recomputes, in the fixed packing of ADR-0011
//! (`abi.encodePacked`: an ASCII domain tag, then fixed-width fields,
//! integers big-endian). Each function names its Solidity twin.

use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::{keccak256, Hash32};
use std::fmt;

/// Domain of [`node_id`].
pub const NODE_DOMAIN: &[u8] = b"onyx-storage-node-v1";
/// Domain of [`deal_salt`].
pub const SALT_DOMAIN: &[u8] = b"onyx-storage-salt-v1";
/// Domain of [`shard_key`].
pub const SHARD_DOMAIN: &[u8] = b"onyx-storage-shard-v1";
/// Domain of [`weight`].
pub const HRW_DOMAIN: &[u8] = b"onyx-storage-hrw-v1";
/// Domain of [`epoch_seed`].
pub const SEED_DOMAIN: &[u8] = b"onyx-storage-seed-v1";
/// Domain of the slot's shard pick.
pub const SLOT_DOMAIN: &[u8] = b"onyx-storage-slot-v1";
/// Domain of the slot's row pick for one holder (v2 adds the node id: ADR-0060
/// amendment 1 item 1).
pub const ROW_DOMAIN: &[u8] = b"onyx-storage-row-v2";

/// A storage node's id: fixed by the chain when the node is activated, never
/// chosen by its operator (THREATS STORAGE-3).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, BorshSerialize, BorshDeserialize)]
pub struct NodeId(pub Hash32);

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn packed(parts: &[&[u8]]) -> Hash32 {
    let mut b = Vec::with_capacity(parts.iter().map(|p| p.len()).sum());
    for p in parts {
        b.extend_from_slice(p);
    }
    keccak256(&b)
}

/// `keccak256("onyx-storage-node-v1" ‖ operator ‖ uint64 index ‖
/// blockhash(registration block))` (`OnyxStorage.activate`). The block hash
/// is of the block that held the registration, unknown when it was sent.
pub fn node_id(operator: &[u8; 20], index: u64, reg_block_hash: &Hash32) -> NodeId {
    NodeId(packed(&[
        NODE_DOMAIN,
        operator,
        &index.to_be_bytes(),
        &reg_block_hash.0,
    ]))
}

/// `keccak256("onyx-storage-salt-v1" ‖ blockhash(block - 1) ‖ uint64 dealId)`
/// (`OnyxStorage.createDeal`).
pub fn deal_salt(prev_block_hash: &Hash32, deal_id: u64) -> Hash32 {
    packed(&[SALT_DOMAIN, &prev_block_hash.0, &deal_id.to_be_bytes()])
}

/// `keccak256("onyx-storage-shard-v1" ‖ salt ‖ uint32 index)`: the placement
/// key of shard `index` of the deal with `salt`.
pub fn shard_key(salt: &Hash32, index: u32) -> Hash32 {
    packed(&[SHARD_DOMAIN, &salt.0, &index.to_be_bytes()])
}

/// `keccak256("onyx-storage-hrw-v1" ‖ shardKey ‖ nodeId)`, read as a
/// big-endian 256-bit number: the node's rendezvous weight for the shard.
pub fn weight(shard_key: &Hash32, node: &NodeId) -> Hash32 {
    packed(&[HRW_DOMAIN, &shard_key.0, &node.0 .0])
}

/// `keccak256("onyx-storage-seed-v1" ‖ blockhash(start block of epoch) ‖
/// uint64 epoch)` (`OnyxStorage.recordSeed`).
pub fn epoch_seed(start_block_hash: &Hash32, epoch: u64) -> Hash32 {
    packed(&[SEED_DOMAIN, &start_block_hash.0, &epoch.to_be_bytes()])
}

/// `keccak256(nodes[0] ‖ nodes[1] ‖ ...)`: the hash of a node list in its
/// stored order (`OnyxStorage` checkpoints it per epoch).
pub fn set_hash(nodes: &[NodeId]) -> Hash32 {
    let mut b = Vec::with_capacity(32 * nodes.len());
    for n in nodes {
        b.extend_from_slice(&n.0 .0);
    }
    keccak256(&b)
}

/// A 256-bit big-endian number modulo `m` (`uint256(h) % m`); 0 when `m` is 0.
pub fn mod_u256(h: &Hash32, m: u64) -> u64 {
    if m == 0 {
        return 0;
    }
    let m = u128::from(m);
    let mut r: u128 = 0;
    for b in h.0 {
        r = ((r << 8) | u128::from(b)) % m;
    }
    // r < m <= u64::MAX.
    r as u64
}

/// `uint256(keccak256("onyx-storage-slot-v1" ‖ seed ‖ uint64 dealId ‖ uint32
/// slot)) % shardCount`: the shard slot `slot` of the deal challenges.
pub fn slot_shard(seed: &Hash32, deal_id: u64, slot: u32, shard_count: u32) -> u32 {
    let h = packed(&[
        SLOT_DOMAIN,
        &seed.0,
        &deal_id.to_be_bytes(),
        &slot.to_be_bytes(),
    ]);
    // Below shard_count, a u32.
    mod_u256(&h, u64::from(shard_count)) as u32
}

/// `uint256(keccak256("onyx-storage-row-v2" ‖ seed ‖ uint64 dealId ‖ uint32
/// slot ‖ bytes32 nodeId)) % rows`: the row the slot asks holder `node` for in
/// its shard. Each holder is asked for its own row, so one answer (public in
/// calldata) answers for no other holder (THREATS STORAGE-S16-R2-2).
pub fn slot_row(seed: &Hash32, deal_id: u64, slot: u32, node: &NodeId, rows: u64) -> u64 {
    let h = packed(&[
        ROW_DOMAIN,
        &seed.0,
        &deal_id.to_be_bytes(),
        &slot.to_be_bytes(),
        &node.0 .0,
    ]);
    mod_u256(&h, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mod_matches_small_numbers() {
        let mut h = Hash32::ZERO;
        h.0[31] = 200;
        assert_eq!(mod_u256(&h, 7), 200 % 7);
        h.0[30] = 1; // 456
        assert_eq!(mod_u256(&h, 1000), 456);
        assert_eq!(mod_u256(&h, 0), 0);
        let all = Hash32([0xff; 32]);
        // 2^256 - 1 mod 2^64 = 2^64 - 1 ... mod u64::MAX = 0 (since 2^64 ≡ 1).
        assert_eq!(mod_u256(&all, u64::MAX), 0);
        assert_eq!(mod_u256(&all, 2), 1);
    }

    #[test]
    fn domains_separate() {
        let s = Hash32([7; 32]);
        assert_ne!(shard_key(&s, 0), shard_key(&s, 1));
        assert_ne!(epoch_seed(&s, 1), deal_salt(&s, 1));
        let a = node_id(&[1; 20], 0, &s);
        let b = node_id(&[1; 20], 1, &s);
        assert_ne!(a, b);
        assert_ne!(set_hash(&[a, b]), set_hash(&[b, a]));
    }

    /// ADR-0060 amendment 1 item 1: the row depends on the holder. Ten
    /// holders of one slot, 2^40 rows: ten different rows.
    #[test]
    fn each_holder_draws_its_own_row() {
        let seed = Hash32([3; 32]);
        let mut rows: Vec<u64> = (0..10)
            .map(|i| slot_row(&seed, 7, 1, &node_id(&[1; 20], i, &seed), 1 << 40))
            .collect();
        rows.sort_unstable();
        rows.dedup();
        assert_eq!(rows.len(), 10);
        // Under 1 row every holder draws row 0 (the stated residual).
        assert_eq!(slot_row(&seed, 7, 1, &node_id(&[1; 20], 0, &seed), 1), 0);
    }
}
