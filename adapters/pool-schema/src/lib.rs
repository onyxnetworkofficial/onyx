//! Borsh schema of `btc-pool-blocks` (ADR-0032): the coinbase source, the
//! pinned pool table (params) and the output row.
//!
//! `no_std` so the recipe can use it inside WASM. A separate crate from
//! `onyx-btc-schema`: touching that crate changes the bytes, and so the ids,
//! of the existing Bitcoin recipes.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use borsh::{BorshDeserialize, BorshSerialize};
pub use onyx_btc_schema::TxOut;

/// Schema version of [`CoinbaseSource`].
pub const COINBASE_SCHEMA_VERSION: u32 = 1;

/// One block's coinbase transaction as the pool metric reads it: the coinbase
/// input's `scriptSig` (where pools write their tag) and the coinbase outputs
/// (where pools are paid). Built from raw blocks whose header, height and
/// Merkle inclusion of the coinbase were checked by the adapter (ADR-0032).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CoinbaseBlock {
    /// Block height (BIP-34 height of the coinbase).
    pub height: u64,
    /// Block hash, display byte order.
    pub hash: [u8; 32],
    /// The coinbase input's `scriptSig` bytes.
    pub script_sig: Vec<u8>,
    /// The coinbase transaction's outputs (value is unused by the pool recipe).
    pub outputs: Vec<TxOut>,
}

/// Canonical source of `btc-pool-blocks`: the coinbase of every block of a
/// contiguous range (ADR-0032).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CoinbaseSource {
    /// Must equal [`COINBASE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Chain id, `btc.mainnet`.
    pub chain: String,
    /// Blocks in strictly ascending, contiguous height order.
    pub blocks: Vec<CoinbaseBlock>,
}

impl CoinbaseSource {
    /// Version and chain match, at least one block, heights contiguous.
    pub fn is_canonical(&self) -> bool {
        self.schema_version == COINBASE_SCHEMA_VERSION
            && self.chain == "btc.mainnet"
            && !self.blocks.is_empty()
            && self
                .blocks
                .windows(2)
                .all(|w| w[0].height.checked_add(1) == Some(w[1].height))
    }
}

/// One mining pool of the pinned pool table (`btc-pool-blocks` params). The
/// name is a reviewed pool company name; `tags` are byte strings searched in
/// the coinbase `scriptSig`; `scripts` are output scripts of the pool's own
/// published coinbase payout addresses (ADR-0032).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PoolRule {
    /// Pool company name as the output names it.
    pub name: String,
    /// Coinbase tags (non-empty byte strings).
    pub tags: Vec<Vec<u8>>,
    /// Payout output scripts (scriptPubKey bytes).
    pub scripts: Vec<Vec<u8>>,
}

/// Params of `btc-pool-blocks`: the pinned pool table, in priority order.
/// Its bytes are the shard's `params`, so its hash is part of the shard id.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PoolTable {
    /// Keccak-256 of the source list file the table was derived from (audit).
    pub list_hash: [u8; 32],
    /// Pools in matching priority order.
    pub pools: Vec<PoolRule>,
}

/// The pool name a block gets when no pinned pool matches it. Never a coinbase
/// string: free text is never output (ADR-0032).
pub const UNKNOWN_POOL: &str = "unknown";

/// One output row of `btc-pool-blocks`: how many blocks of the range one pool
/// mined. Key: the pool name's UTF-8 bytes, so rows ascend by name.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PoolBlocksRow {
    /// Pool name from the table, or [`UNKNOWN_POOL`].
    pub pool: String,
    /// First height of the range.
    pub from_height: u64,
    /// Last height of the range (inclusive).
    pub to_height: u64,
    /// Blocks of the range attributed to this pool.
    pub blocks: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn block(height: u64) -> CoinbaseBlock {
        CoinbaseBlock {
            height,
            hash: [0; 32],
            script_sig: vec![],
            outputs: vec![],
        }
    }

    #[test]
    fn canonical_needs_contiguous_blocks_on_mainnet() {
        let mut s = CoinbaseSource {
            schema_version: COINBASE_SCHEMA_VERSION,
            chain: "btc.mainnet".into(),
            blocks: vec![block(5), block(6)],
        };
        assert!(s.is_canonical());
        s.blocks = vec![block(u64::MAX), block(0)];
        assert!(!s.is_canonical());
        s.blocks = vec![];
        assert!(!s.is_canonical());
        s.blocks = vec![block(5)];
        s.chain = "btc.testnet".into();
        assert!(!s.is_canonical());
    }
}
