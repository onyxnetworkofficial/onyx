//! Ethereum block format for Onyx (ADR-0044): RLP, the ordered
//! Merkle-Patricia trie root, the header, transaction and receipt fields the
//! statistics read, and the Borsh schema of `eth-chain-stats` (its source
//! and its per-block row).
//!
//! `no_std` so the recipe uses the same code inside WASM as the adapter and
//! the browser verifier outside it. All amounts are integer wei (ADR-0009),
//! as `u128` (Ethereum's whole supply is below 2^87 wei); ratios are parts
//! per million.

#![no_std]

extern crate alloc;

pub mod decode;
pub mod rlp;
#[cfg(any(test, feature = "synthetic"))]
pub mod synthetic;
pub mod trie;

use alloc::string::String;
use alloc::vec::Vec;
use borsh::{BorshDeserialize, BorshSerialize};
use sha3::{Digest, Keccak256};

/// Keccak-256 (ADR-0003; Ethereum's block, transaction and trie hash).
pub fn keccak256(b: &[u8]) -> [u8; 32] {
    Keccak256::digest(b).into()
}

/// Schema version of [`EthChainSource`].
pub const ETH_SCHEMA_VERSION: u32 = 1;

/// Chain id of Ethereum mainnet in Onyx's registry.
pub const ETH_MAINNET: &str = "eth.mainnet";

/// One block as the chain hashes it: the header's RLP, every transaction
/// as the transactions trie stores it (EIP-2718 envelope) and every receipt
/// as the receipts trie stores it, in block order.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct EthBlock {
    /// Block number.
    pub number: u64,
    /// Block hash: Keccak-256 of `header`.
    pub hash: [u8; 32],
    /// The header's RLP.
    pub header: Vec<u8>,
    /// Transactions, in block order.
    pub txs: Vec<Vec<u8>>,
    /// Receipts, one per transaction, in block order.
    pub receipts: Vec<Vec<u8>>,
}

/// Canonical source of `eth-chain-stats`: every block of a contiguous range
/// and the header of the block before it (for the first block's parent
/// link, base fee and interval).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct EthChainSource {
    /// Must equal [`ETH_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Chain id, [`ETH_MAINNET`].
    pub chain: String,
    /// RLP of the header of block `blocks[0].number - 1`.
    pub parent_header: Vec<u8>,
    /// Blocks in strictly ascending, contiguous number order.
    pub blocks: Vec<EthBlock>,
}

impl EthChainSource {
    /// Version and chain match, at least one block, the first above zero
    /// (it has a parent), numbers contiguous, one receipt per transaction.
    pub fn is_canonical(&self) -> bool {
        self.schema_version == ETH_SCHEMA_VERSION
            && self.chain == ETH_MAINNET
            && self.blocks.first().is_some_and(|b| b.number > 0)
            && self
                .blocks
                .windows(2)
                .all(|w| w[0].number.checked_add(1) == Some(w[1].number))
            && self.blocks.iter().all(|b| b.txs.len() == b.receipts.len())
    }
}

/// The shard parameters of `eth-chain-stats`: the range and tip the shard's snapshot pins.
/// The recipe refuses a source whose first and last block numbers or last block hash differ,
/// so the rows are bound to the pinned blocks, not only to some self-consistent chain (P4A-R1-1).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct EthStatsParams {
    /// First block number.
    pub from: u64,
    /// Last block number.
    pub to: u64,
    /// Hash of the last block.
    pub tip: [u8; 32],
}

/// Schema tag of [`EthChainStatsRow`]: `ShardSpec.schema` is its Keccak.
pub const SCHEMA_ETH_CHAIN_STATS: &[u8] = b"onyx.eth.chain-stats-row.v1";

/// One output row of `eth-chain-stats`: one block's statistics. Key: the
/// block number as 8 big-endian bytes, so rows ascend by number.
/// Definitions: docs/spec/recipes.md R-ETH-2.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct EthChainStatsRow {
    /// Block number.
    pub number: u64,
    /// Block hash.
    pub hash: [u8; 32],
    /// Header timestamp, unix seconds.
    pub time: u64,
    /// Timestamp minus the parent's, seconds (12 per slot; more after a missed slot).
    pub interval_secs: u64,
    /// Transactions.
    pub txs: u64,
    /// Transactions whose receipt has status 0 (reverted; their gas is still paid).
    pub failed_txs: u64,
    /// Successful transactions (receipt status 1) with an empty `to`: top-level contract
    /// creations (a reverted creation makes no contract; a contract created by another
    /// contract's code is not counted).
    pub contract_creations: u64,
    /// Gas used by the block.
    pub gas_used: u64,
    /// Gas limit of the block.
    pub gas_limit: u64,
    /// `gas_used x 1,000,000 / gas_limit`, rounded down.
    pub fullness_ppm: u32,
    /// Base fee per gas, wei.
    pub base_fee_wei: u128,
    /// Base fee x gas used: the execution fee burned (EIP-1559), wei.
    pub burned_wei: u128,
    /// Sum over transactions of (effective gas price - base fee) x gas used: the
    /// priority fees paid to the block's fee recipient (often a builder), wei.
    pub priority_fees_wei: u128,
    /// Blob-carrying transactions (type 3).
    pub blob_txs: u64,
    /// Blob gas used (131,072 per blob), from the header.
    pub blob_gas_used: u64,
    /// Excess blob gas, from the header.
    pub excess_blob_gas: u64,
    /// Blob base fee per blob gas, wei (EIP-4844 `fake_exponential` with the
    /// update fraction the recipe pins).
    pub blob_base_fee_wei: u128,
    /// Blob gas used x blob base fee: the blob fees burned, wei.
    pub blob_fee_wei: u128,
    /// Sum of the `value` fields of successful transactions (receipt status 1): ether
    /// sent by top-level calls. A reverted transaction sends nothing; internal transfers
    /// by contract code are not counted. Wei.
    pub value_wei: u128,
}

impl EthChainStatsRow {
    /// The row key described on [`EthChainStatsRow`].
    pub fn key(number: u64) -> Vec<u8> {
        number.to_be_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn block(number: u64) -> EthBlock {
        EthBlock {
            number,
            hash: [0; 32],
            header: vec![],
            txs: vec![],
            receipts: vec![],
        }
    }

    #[test]
    fn canonical_needs_contiguous_blocks_above_genesis_on_mainnet() {
        let mut s = EthChainSource {
            schema_version: ETH_SCHEMA_VERSION,
            chain: ETH_MAINNET.into(),
            parent_header: vec![],
            blocks: vec![block(5), block(6)],
        };
        assert!(s.is_canonical());
        s.blocks = vec![block(u64::MAX), block(0)];
        assert!(!s.is_canonical());
        s.blocks = vec![block(0)];
        assert!(!s.is_canonical(), "genesis has no parent");
        s.blocks = vec![];
        assert!(!s.is_canonical());
        s.blocks = vec![block(5), block(7)];
        assert!(!s.is_canonical(), "a gap");
        s.blocks = vec![block(5)];
        s.blocks[0].txs.push(vec![1]);
        assert!(!s.is_canonical(), "a transaction without its receipt");
        s.blocks[0].receipts.push(vec![1]);
        assert!(s.is_canonical());
        s.chain = "eth.sepolia".into();
        assert!(!s.is_canonical());
        s.chain = ETH_MAINNET.into();
        s.schema_version = 2;
        assert!(!s.is_canonical());
    }

    #[test]
    fn keys_ascend_with_number_and_keccak_is_ethereums() {
        assert!(EthChainStatsRow::key(255) < EthChainStatsRow::key(256));
        // Keccak-256 of the empty string (not SHA3-256's a7ffc6f8...).
        assert_eq!(
            keccak256(b"")[..4],
            [0xc5, 0xd2, 0x46, 0x01],
            "Keccak-256, not FIPS SHA3"
        );
    }
}
