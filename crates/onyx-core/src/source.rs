//! Sources, snapshots and the adapter interface every sector implements.

use crate::hash::{keccak256, Hash32};
use crate::ids::is_dotted_id;
use crate::rights::DataRights;
use borsh::{BorshDeserialize, BorshSerialize};

/// Stable source identifier, dotted namespace, e.g. `chain.btc.mainnet` or `official.fred`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, BorshSerialize, BorshDeserialize)]
pub struct SourceId(pub String);

/// Chain identifier as a namespaced string, e.g. `btc.mainnet`, `eip155:8453`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, BorshSerialize, BorshDeserialize)]
pub struct ChainId(pub String);

/// The exact input state a claim was computed against.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Snapshot {
    /// A chain at a height. The block hash pins the fork.
    Block {
        /// Chain.
        chain: ChainId,
        /// Height (inclusive).
        height: u64,
        /// Hash of the block at `height`.
        block_hash: Hash32,
    },
    /// A contiguous block range, pinned by its tip and by the content hash of
    /// the adapter's canonical bytes for the whole range (R-SRC-BIND-1). The
    /// first height is part of the pin, so a shorter range with the same tip,
    /// or edited content, is a different snapshot.
    BlockRange {
        /// Chain.
        chain: ChainId,
        /// First height (inclusive).
        from_height: u64,
        /// Last height (inclusive).
        to_height: u64,
        /// Hash of the block at `to_height`.
        tip_hash: Hash32,
        /// Keccak-256 of the adapter's canonical source bytes for this range.
        content_hash: Hash32,
    },
    /// An offchain publication (filing, statistical release, price file) pinned by content hash.
    Document {
        /// Publication time, unix seconds, as stated by the publisher.
        published_unix: u64,
        /// Keccak-256 of the exact bytes fetched.
        content_hash: Hash32,
    },
}

/// A source plus the snapshot of it that was used.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SourceRef {
    /// Which source.
    pub source: SourceId,
    /// Which state of it.
    pub snapshot: Snapshot,
}

/// Snapshot or source error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// Malformed source or chain id.
    #[error("malformed id: {0}")]
    BadId(&'static str),
    /// Zero hash where a real content or block hash is required.
    #[error("zero hash in snapshot")]
    ZeroHash,
    /// Range with `from_height > to_height`.
    #[error("block range is empty or reversed")]
    BadRange,
    /// Document without a publication time.
    #[error("document snapshot needs a publication time")]
    MissingPublishTime,
}

impl Snapshot {
    /// Structural check.
    pub fn validate(&self) -> Result<(), SourceError> {
        match self {
            Snapshot::Block {
                chain, block_hash, ..
            } => {
                if !is_dotted_id(&chain.0) {
                    return Err(SourceError::BadId("chain"));
                }
                if *block_hash == Hash32::ZERO {
                    return Err(SourceError::ZeroHash);
                }
            }
            Snapshot::BlockRange {
                chain,
                from_height,
                to_height,
                tip_hash,
                content_hash,
            } => {
                if !is_dotted_id(&chain.0) {
                    return Err(SourceError::BadId("chain"));
                }
                if from_height > to_height {
                    return Err(SourceError::BadRange);
                }
                if *tip_hash == Hash32::ZERO || *content_hash == Hash32::ZERO {
                    return Err(SourceError::ZeroHash);
                }
            }
            Snapshot::Document {
                published_unix,
                content_hash,
            } => {
                if *published_unix == 0 {
                    return Err(SourceError::MissingPublishTime);
                }
                if *content_hash == Hash32::ZERO {
                    return Err(SourceError::ZeroHash);
                }
            }
        }
        Ok(())
    }
}

impl SourceRef {
    /// Structural check.
    pub fn validate(&self) -> Result<(), SourceError> {
        if !is_dotted_id(&self.source.0) {
            return Err(SourceError::BadId("source"));
        }
        self.snapshot.validate()
    }

    /// Keccak-256 of the canonical encoding; bound into every commitment.
    pub fn hash(&self) -> Hash32 {
        keccak256(&borsh::to_vec(self).unwrap_or_default())
    }
}

/// Interface every sector adapter implements. Adapters turn a source snapshot
/// into canonical input bytes that recipes consume. Core never imports an adapter.
pub trait SourceAdapter {
    /// Adapter error type.
    type Error: std::error::Error + Send + Sync + 'static;

    /// The source this adapter reads.
    fn source_id(&self) -> SourceId;

    /// The rights that apply to everything this adapter emits.
    fn rights(&self) -> DataRights;

    /// Produce canonical recipe input for a snapshot. Must be a pure function of
    /// the snapshot: the same snapshot gives the same bytes on every machine.
    fn load(&self, snapshot: &Snapshot) -> Result<Vec<u8>, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(from: u64, to: u64, tip: Hash32, content: Hash32) -> Snapshot {
        Snapshot::BlockRange {
            chain: ChainId("btc.mainnet".into()),
            from_height: from,
            to_height: to,
            tip_hash: tip,
            content_hash: content,
        }
    }

    /// Each structural rule of a snapshot on its own (mutation L4).
    #[test]
    fn snapshot_rules() {
        let h = keccak256(b"h");
        assert_eq!(range(5, 5, h, h).validate(), Ok(()));
        assert_eq!(range(5, 9, h, h).validate(), Ok(()));
        assert_eq!(range(6, 5, h, h).validate(), Err(SourceError::BadRange));
        assert_eq!(
            range(5, 9, Hash32::ZERO, h).validate(),
            Err(SourceError::ZeroHash)
        );
        assert_eq!(
            range(5, 9, h, Hash32::ZERO).validate(),
            Err(SourceError::ZeroHash)
        );
        let bad_chain = Snapshot::BlockRange {
            chain: ChainId("Not A Chain".into()),
            from_height: 1,
            to_height: 2,
            tip_hash: h,
            content_hash: h,
        };
        assert_eq!(bad_chain.validate(), Err(SourceError::BadId("chain")));
        let doc = |t: u64, c: Hash32| Snapshot::Document {
            published_unix: t,
            content_hash: c,
        };
        assert_eq!(doc(1, h).validate(), Ok(()));
        assert_eq!(doc(0, h).validate(), Err(SourceError::MissingPublishTime));
        assert_eq!(doc(1, Hash32::ZERO).validate(), Err(SourceError::ZeroHash));
    }
}
