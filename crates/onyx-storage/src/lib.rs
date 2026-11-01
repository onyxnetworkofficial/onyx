//! The Onyx storage network (ADR-0060, docs/spec/storage-network.md,
//! docs/STORAGE-NETWORK.md). Test network only.
//!
//! A publisher's bulk dataset (ADR-0059: shards of at most 64 MiB under
//! Keccak Merkle roots, a manifest anchored in `OnyxSignedBook`) is bought
//! storage in `OnyxStorage` (a *deal*: a deposit per GiB per epoch for R
//! replicas). Every shard is placed on R registered storage nodes by
//! rendezvous (highest-random-weight) hashing ([`placement`]), so a node that
//! joins or leaves moves only the shards it wins or held. Each epoch a seed
//! from a past block hash picks challenge slots per deal ([`challenge`]); the
//! nodes holding the picked shard answer on chain with the picked row and its
//! two Merkle paths (row to shard root, shard entry to the manifest's
//! `shards_root`). A passed answer earns a share of the deal's epoch payment;
//! a wrong or missing one slashes part of the node's bond and frees that
//! shard's assignment, which [`repair`] plans. [`ledger`] holds the money
//! rules and a model of the contract's books; [`epoch`] the accounting of one
//! epoch from the contract's events; [`abi`] the calldata and return values.
//!
//! Every hash the contract recomputes uses the fixed packing of ADR-0011
//! (`abi.encodePacked` of fixed-width big-endian fields, [`ids`]), except
//! the two ADR-0059 hashes the contract reads as they are: the manifest root
//! over the Borsh header and the shard entry leaf (fixed-width Borsh, little
//! endian, [`deal::HeaderTail`], [`challenge::parse_entry`]).
//! `fixtures/storage/vectors.json` holds the vectors both sides check.

pub mod abi;
pub mod challenge;
pub mod deal;
pub mod epoch;
pub mod ids;
pub mod ledger;
pub mod placement;
pub mod repair;
#[cfg(test)]
mod vectors;

pub use challenge::{ProofError, StorageProof};
pub use ids::NodeId;
pub use placement::{holders, ranked};

/// Most active storage nodes (the contract's `MAX_NODES`): an answer carries
/// the whole node list of its epoch in calldata.
pub const MAX_NODES: usize = 128;
/// Largest replication factor a deal may ask for (`MAX_R`).
pub const MAX_R: u8 = 8;
/// Default replication factor.
pub const DEFAULT_R: u8 = 3;
/// Epochs after an epoch ends during which its missed answers can be settled
/// (`SETTLE_EPOCHS`).
pub const SETTLE_EPOCHS: u64 = 2;
/// Blocks after an epoch's start block during which its seed can be recorded
/// (`SEED_WINDOW`, the EVM's `blockhash` reach).
pub const SEED_WINDOW: u64 = 256;
/// Bytes in a GiB.
pub const GIB: u64 = 1 << 30;
/// Parts per million.
pub const PPM: u32 = 1_000_000;
