//! Borsh schema for Bitcoin recipe source data, parameters and output rows.
//!
//! `no_std` so recipes can use it inside WASM. The adapter (`onyx-source-btc`)
//! builds [`BtcSource`] from a pinned snapshot and does nothing else: turning
//! label addresses into scripts happens inside the recipe, so all logic runs in
//! the checked sandbox.
//! All amounts are integer satoshis (ADR-0009). Every list with set semantics
//! is strictly sorted, and [`BtcRangeInput::is_canonical`] checks that.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use borsh::{BorshDeserialize, BorshSerialize};

/// Schema version carried in every input, so a recipe can refuse inputs it does not understand.
pub const SCHEMA_VERSION: u32 = 1;

/// One transaction output: locking script and value.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct TxOut {
    /// scriptPubKey bytes.
    pub script: Vec<u8>,
    /// Value in satoshis.
    pub value: u64,
}

/// A transaction reduced to what balance recipes need, in block order.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct TxDelta {
    /// Outputs this transaction spends (its inputs' prevouts). Empty for coinbase.
    pub spent: Vec<TxOut>,
    /// Outputs this transaction creates.
    pub created: Vec<TxOut>,
}

/// One block.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct BlockDelta {
    /// Height.
    pub height: u64,
    /// Block hash in internal byte order reversed for display (as shown by explorers).
    pub hash: [u8; 32],
    /// Transactions in block order.
    pub txs: Vec<TxDelta>,
}

/// Starting balance for an entity, from a prior finalized claim. Sorted by entity.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct EntityBase {
    /// Onyx entity id.
    pub entity: String,
    /// Balance in satoshis at `start_height - 1`.
    pub balance_sats: u64,
}

/// Source part of the recipe input: a contiguous block range.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct BtcSource {
    /// Must equal [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Chain id, e.g. `btc.mainnet`.
    pub chain: String,
    /// Blocks in ascending, contiguous height order.
    pub blocks: Vec<BlockDelta>,
}

impl BtcSource {
    /// True if the version and chain match and heights are contiguous and
    /// ascending (no wrap-around: `checked_add`).
    pub fn is_canonical(&self) -> bool {
        self.schema_version == SCHEMA_VERSION
            && self.chain == "btc.mainnet"
            && !self.blocks.is_empty()
            && self
                .blocks
                .windows(2)
                .all(|w| w[0].height.checked_add(1) == Some(w[1].height))
    }
}

/// Shard parameters for `btc-entity-balance`.
#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct BalanceParams {
    /// Starting balances, strictly ascending by entity. Empty if none were supplied.
    pub base: Vec<EntityBase>,
}

impl BalanceParams {
    /// True if `base` is strictly ascending by entity.
    pub fn is_canonical(&self) -> bool {
        self.base.windows(2).all(|w| w[0].entity < w[1].entity)
    }
}

/// One output row of `btc-entity-balance`. The row key is the raw UTF-8 bytes
/// of the entity id, so rows are strictly ascending by entity id (R-KEY-ORD-1).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct EntityFlowRow {
    /// Onyx entity id.
    pub entity: String,
    /// First height of the range.
    pub from_height: u64,
    /// Last height of the range (inclusive).
    pub to_height: u64,
    /// Satoshis received by labelled scripts in the range.
    pub received_sats: u128,
    /// Satoshis spent from labelled scripts in the range.
    pub sent_sats: u128,
    /// `received - sent`.
    pub net_sats: i128,
    /// Transactions that touched a labelled script of this entity.
    pub tx_count: u64,
    /// `base + net` when a base was supplied, else `None`.
    pub end_balance_sats: Option<u128>,
}

/// Failure code `btc-entity-flow-series` returns when no labelled entity was
/// touched anywhere in the range (SER-R1-F1). A shard needs at least one leaf,
/// so a quiet range commits the canonical failure leaf of
/// `RecipeError(SERIES_NO_ACTIVITY)`: every honest run reaches it, and readers
/// show it as an all-zero series, not as an error (R-SERIES-1).
pub const SERIES_NO_ACTIVITY: u32 = 8;

/// One output row of `btc-entity-flow-series`: one entity's flows in one block.
/// The row key is the entity id's UTF-8 bytes, a `0x00` byte, then the height
/// as 8 big-endian bytes, so rows are strictly ascending by (entity, height)
/// (R-KEY-ORD-1). Only blocks where the entity was touched get a row: a
/// missing (entity, height) means zero flow (R-SERIES-1). A range with no row
/// at all ends with [`SERIES_NO_ACTIVITY`].
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct EntityBlockFlowRow {
    /// Onyx entity id.
    pub entity: String,
    /// Block height.
    pub height: u64,
    /// Satoshis received by labelled scripts in this block.
    pub received_sats: u128,
    /// Satoshis spent from labelled scripts in this block.
    pub sent_sats: u128,
    /// `received - sent`.
    pub net_sats: i128,
    /// Transactions in this block that touched a labelled script of this entity.
    pub tx_count: u64,
}

impl EntityBlockFlowRow {
    /// The row key described on [`EntityBlockFlowRow`].
    pub fn key(entity: &str, height: u64) -> Vec<u8> {
        let mut k = Vec::with_capacity(entity.len() + 9);
        k.extend_from_slice(entity.as_bytes());
        k.push(0);
        k.extend_from_slice(&height.to_be_bytes());
        k
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn block(height: u64) -> BlockDelta {
        BlockDelta {
            height,
            hash: [0; 32],
            txs: vec![],
        }
    }

    /// R-CLAIM-1 / R-LBLVIEW-1: no wrap-around, chain checked.
    #[test]
    fn canonical_rejects_wrap_and_wrong_chain() {
        let mut s = BtcSource {
            schema_version: SCHEMA_VERSION,
            chain: "btc.mainnet".into(),
            blocks: vec![block(5), block(6)],
        };
        assert!(s.is_canonical());
        s.blocks = vec![block(u64::MAX), block(0)];
        assert!(!s.is_canonical());
        s.blocks = vec![block(5), block(6)];
        s.chain = "btc.testnet".into();
        assert!(!s.is_canonical());
    }
}
