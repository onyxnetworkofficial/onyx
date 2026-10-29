//! Borsh schema of `btc-entity-reserve` (ADR-0034): the base a reserve shard
//! builds on (params) and the output row.
//!
//! `no_std` so the recipe can use it inside WASM. A separate crate from
//! `onyx-btc-schema`: touching that crate changes the bytes, and so the ids,
//! of the existing Bitcoin recipes. The source is `onyx_btc_schema::BtcSource`
//! and base rows are `onyx_btc_schema::EntityBase`, unchanged.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use borsh::{BorshDeserialize, BorshSerialize};
pub use onyx_btc_schema::EntityBase;

/// Schema tag of [`EntityReserveRow`]: `ShardSpec.schema` is its Keccak.
pub const SCHEMA_ENTITY_RESERVE: &[u8] = b"onyx.btc.entity-reserve-row.v1";

/// Shard parameters for `btc-entity-reserve` (ADR-0034): the state a reserve
/// shard builds on. A reserve history is a chain of shards; each shard's base
/// is the previous shard's end balances, and the first shard's base comes from
/// a committed snapshot (a UTXO scan rolled back to `base_height`).
#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ReserveParams {
    /// Height the base balances hold at (after that block). The source must
    /// start at `base_height + 1` (R-RES-BASE-1: a base for another height is refused).
    pub base_height: u64,
    /// What the base comes from: Keccak of the base snapshot document for the
    /// first shard of a chain, else the Merkle root of the previous shard.
    /// The recipe cannot check it; it carries it into every row, and the
    /// chain checker (`onyx_source_btc::reserve`) checks the links.
    pub base_ref: [u8; 32],
    /// Balance at `base_height` of every labelled entity, strictly ascending
    /// by entity; exactly the label set's entities (a partial base would
    /// fabricate zero balances, R-RCP-BASE-1).
    pub base: Vec<EntityBase>,
}

impl ReserveParams {
    /// True if `base` is strictly ascending by entity.
    pub fn is_canonical(&self) -> bool {
        self.base.windows(2).all(|w| w[0].entity < w[1].entity)
    }
}

/// One output row of `btc-entity-reserve`: one entity's balance at the end of
/// the range. The row key is the raw UTF-8 bytes of the entity id, so rows are
/// strictly ascending by entity id (R-KEY-ORD-1). Every labelled entity gets a
/// row (also when untouched), so the rows are the next shard's base.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct EntityReserveRow {
    /// Onyx entity id.
    pub entity: String,
    /// Height of the base (`ReserveParams::base_height`).
    pub base_height: u64,
    /// Last height of the range: the balance holds after this block.
    pub height: u64,
    /// `ReserveParams::base_ref`, carried so a row names the state it built on.
    pub base_ref: [u8; 32],
    /// Balance at `base_height`.
    pub base_sats: u64,
    /// Satoshis received by labelled scripts in the range.
    pub received_sats: u128,
    /// Satoshis spent from labelled scripts in the range.
    pub sent_sats: u128,
    /// Transactions that touched a labelled script of this entity.
    pub tx_count: u64,
    /// `base + received - sent`, never negative at any transaction (R-RES-NEG-1).
    pub balance_sats: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn base(entities: &[&str]) -> ReserveParams {
        ReserveParams {
            base: entities
                .iter()
                .map(|e| EntityBase {
                    entity: (*e).into(),
                    balance_sats: 1,
                })
                .collect(),
            ..ReserveParams::default()
        }
    }

    /// The base is strictly ascending by entity: no duplicate, no reorder
    /// (mutation L4).
    #[test]
    fn base_must_ascend_strictly() {
        assert!(base(&[]).is_canonical());
        assert!(base(&["entity:a"]).is_canonical());
        assert!(base(&["entity:a", "entity:b", "entity:c"]).is_canonical());
        assert!(!base(&["entity:b", "entity:a"]).is_canonical());
        assert!(!base(&["entity:a", "entity:a"]).is_canonical());
        assert_eq!(
            base(&["entity:a"]).base,
            vec![EntityBase {
                entity: "entity:a".into(),
                balance_sats: 1
            }]
        );
    }
}
