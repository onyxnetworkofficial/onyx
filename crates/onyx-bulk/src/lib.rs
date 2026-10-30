//! Large datasets from a publisher (contributor loop CL5, ADR-0059,
//! docs/spec/bulk-datasets.md). Test network only.
//!
//! A dataset is a [`Schema`] (named columns of integer, decimal-as-string or
//! text kind; no floats, ADR-0009) and a list of shards. A shard is a bounded
//! batch of rows ([`shard::MAX_SHARD_BYTES`], [`shard::MAX_SHARD_ROWS`])
//! sorted by the schema's key column, committed by the ADR-0003 Merkle root
//! over its rows, so every row is provable on its own. A [`Manifest`] lists
//! the shards (index, root, row count, byte count, key range) and has its own
//! Keccak root ([`ManifestHeader::root`]); the publisher signs that root with
//! the level-3 scheme of ADR-0028 ([`anchor`]) and the signed report goes on
//! chain in `OnyxSignedBook` with one `postReport` per manifest version.
//! Versions are append-only: a new version keeps every earlier shard and
//! adds at least one.
//!
//! [`wire`] holds the JSON form a node serves and the client check of it:
//! row to shard root, shard entry to manifest root, manifest root to the
//! publisher's signature. The chain read (the book recorded and accepted
//! that signed report) is the caller's (`onyx-check --bulk`).

pub mod anchor;
pub mod input;
pub mod manifest;
pub mod row;
pub mod schema;
pub mod shard;
pub mod wire;

pub use anchor::{AnchorError, ANCHOR_METRIC};
pub use manifest::{Manifest, ManifestError, ManifestHeader, ShardEntry, RIGHTS_STATEMENT};
pub use row::{Cell, RowError};
pub use schema::{Column, ColumnKind, Schema, SchemaError};
pub use shard::{ShardError, ShardSummary};
