//! Borsh schema of `btc-chain-stats` (ADR-0043): the block source, and the
//! per-block statistics row.
//!
//! `no_std` so the recipe can use it inside WASM. A separate crate from
//! `onyx-btc-schema`: touching that crate changes the bytes, and so the ids,
//! of the existing Bitcoin recipes.
//!
//! All amounts are integer satoshis and every rate is an integer (ADR-0009):
//! fee rates in milli-satoshis per virtual byte (`sat/vB x 1000`), shares as
//! counts the reader divides.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use borsh::{BorshDeserialize, BorshSerialize};
pub use onyx_btc_schema::TxOut;

/// Schema version of [`ChainSource`].
pub const CHAIN_SCHEMA_VERSION: u32 = 1;

/// One spent output, as the chain statistics read it: its value and script
/// (the prevout) and the height of the block that created it.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ChainInput {
    /// Value of the spent output, satoshis.
    pub value: u64,
    /// scriptPubKey of the spent output.
    pub script: Vec<u8>,
    /// Height of the block that created the spent output.
    pub created_height: u64,
}

/// One transaction: its serialized sizes (from the raw bytes), what it spends
/// and what it creates, in block order.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ChainTx {
    /// Size without witness data, bytes (BIP-141 base size).
    pub base_size: u32,
    /// Size with witness data, bytes (BIP-141 total size).
    pub total_size: u32,
    /// Spent outputs, in input order. Empty for the coinbase.
    pub inputs: Vec<ChainInput>,
    /// Created outputs, in output order.
    pub outputs: Vec<TxOut>,
}

/// One block: height, hash (display order), the raw 80-byte header and its
/// transactions.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ChainBlock {
    /// Height.
    pub height: u64,
    /// Block hash, display byte order (as explorers show it).
    pub hash: [u8; 32],
    /// The 80-byte header.
    pub header: [u8; 80],
    /// Transactions in block order; the first is the coinbase.
    pub txs: Vec<ChainTx>,
}

/// Canonical source of `btc-chain-stats`: every block of a contiguous range,
/// plus the header of the block before it (for the first block's interval).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ChainSource {
    /// Must equal [`CHAIN_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Chain id, `btc.mainnet`.
    pub chain: String,
    /// Header of the block at `blocks[0].height - 1`.
    pub parent_header: [u8; 80],
    /// Blocks in strictly ascending, contiguous height order.
    pub blocks: Vec<ChainBlock>,
}

impl ChainSource {
    /// Version and chain match, at least one block, the first above height
    /// zero (it has a parent), heights contiguous.
    pub fn is_canonical(&self) -> bool {
        self.schema_version == CHAIN_SCHEMA_VERSION
            && self.chain == "btc.mainnet"
            && self.blocks.first().is_some_and(|b| b.height > 0)
            && self
                .blocks
                .windows(2)
                .all(|w| w[0].height.checked_add(1) == Some(w[1].height))
    }
}

/// Schema tag of [`ChainStatsRow`]: `ShardSpec.schema` is its Keccak.
pub const SCHEMA_CHAIN_STATS: &[u8] = b"onyx.btc.chain-stats-row.v1";

/// One output row of `btc-chain-stats`: one block's statistics. Key: the
/// height as 8 big-endian bytes, so rows ascend by height. Definitions are
/// docs/spec/recipes.md R-CHAIN-2.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ChainStatsRow {
    /// Block height.
    pub height: u64,
    /// Block hash, display byte order.
    pub hash: [u8; 32],
    /// Header timestamp (unix seconds).
    pub time: u32,
    /// Header timestamp minus the parent's (seconds; can be negative).
    pub interval_secs: i64,
    /// Compact difficulty target (`nBits`) from the header.
    pub bits: u32,
    /// Transactions, coinbase included.
    pub txs: u64,
    /// Inputs of non-coinbase transactions.
    pub inputs: u64,
    /// Outputs of every transaction, coinbase included.
    pub outputs: u64,
    /// Serialized block size, bytes.
    pub size_bytes: u64,
    /// Block weight, weight units (BIP-141).
    pub weight_wu: u64,
    /// Subsidy the schedule allows at this height, satoshis.
    pub subsidy_sats: u64,
    /// Sum of non-coinbase fees (inputs minus outputs), satoshis.
    pub fees_sats: u64,
    /// Sum of the coinbase outputs, satoshis.
    pub coinbase_sats: u64,
    /// New supply: coinbase outputs minus fees, satoshis (below the subsidy
    /// when a miner claims less than it may; negative if it claims less than
    /// the fees).
    pub issued_sats: i64,
    /// Sum of non-coinbase outputs (value moved, change included), satoshis.
    pub transfer_sats: u128,
    /// Lower median of the non-coinbase fee rates, milli-sat per vB.
    pub fee_rate_median_msat_vb: u64,
    /// Fees over the non-coinbase virtual size, milli-sat per vB.
    pub fee_rate_avg_msat_vb: u64,
    /// Non-coinbase transactions that carry witness data.
    pub segwit_txs: u64,
    /// Inputs spending a pay-to-taproot output.
    pub taproot_inputs: u64,
    /// Outputs creating a pay-to-taproot output.
    pub taproot_outputs: u64,
    /// Distinct spendable output scripts spent from or paid to in the block.
    pub active_scripts: u64,
    /// Sum over inputs of value x (height - creation height), sat-blocks.
    pub coin_blocks_destroyed: u128,
}

impl ChainStatsRow {
    /// The row key described on [`ChainStatsRow`].
    pub fn key(height: u64) -> Vec<u8> {
        height.to_be_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn block(height: u64) -> ChainBlock {
        ChainBlock {
            height,
            hash: [0; 32],
            header: [0; 80],
            txs: vec![],
        }
    }

    #[test]
    fn canonical_needs_contiguous_blocks_above_genesis_on_mainnet() {
        let mut s = ChainSource {
            schema_version: CHAIN_SCHEMA_VERSION,
            chain: "btc.mainnet".into(),
            parent_header: [0; 80],
            blocks: vec![block(5), block(6)],
        };
        assert!(s.is_canonical());
        s.blocks = vec![block(u64::MAX), block(0)];
        assert!(!s.is_canonical());
        s.blocks = vec![block(0)];
        assert!(!s.is_canonical(), "genesis has no parent");
        s.blocks = vec![];
        assert!(!s.is_canonical());
        s.blocks = vec![block(5)];
        s.chain = "btc.testnet".into();
        assert!(!s.is_canonical());
        s.chain = "btc.mainnet".into();
        s.schema_version = 2;
        assert!(!s.is_canonical());
    }

    #[test]
    fn keys_ascend_with_height() {
        assert!(ChainStatsRow::key(255) < ChainStatsRow::key(256));
        assert_eq!(ChainStatsRow::key(1), vec![0, 0, 0, 0, 0, 0, 0, 1]);
    }
}
