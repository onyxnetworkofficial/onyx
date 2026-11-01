//! Storage proofs (R-STOR-7).
//!
//! In epoch `e` with seed `s`, slot `i` of deal `d` picks shard
//! [`slot_shard`]`(s, d, i, shardCount)` and, in it, row
//! [`slot_row`]`(s, d, i, rows)`. A challenged holder answers with the
//! shard's 84-byte entry (Borsh [`ShardEntry`]) and its Merkle path to the
//! manifest's `shards_root`, and the row's bytes and their path to the
//! entry's root (ADR-0003 layout, `OnyxMerkle.sol`). The contract recomputes
//! both paths with Keccak; the index and leaf count of each path come from
//! the challenge and the entry, never from the answer.

use crate::ids::{slot_row, slot_shard};
use onyx_bulk::manifest::{shards_tree, Manifest, ShardEntry};
use onyx_bulk::shard::split_rows;
use onyx_core::Hash32;
use onyx_merkle::{MerkleTree, Proof};

/// Bytes of a Borsh [`ShardEntry`]: u32, 32 bytes, u64, u64, i128, i128.
pub const ENTRY_BYTES: usize = 4 + 32 + 8 + 8 + 16 + 16;

/// A storage proof: what an answer carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageProof {
    /// The shard's entry in the manifest.
    pub entry: ShardEntry,
    /// Siblings of the entry's path to `shards_root`.
    pub entry_siblings: Vec<Hash32>,
    /// The row's bytes.
    pub row: Vec<u8>,
    /// Siblings of the row's path to the entry's root.
    pub row_siblings: Vec<Hash32>,
}

/// Why a proof cannot be built or does not hold.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProofError {
    /// The shard index is not in the manifest.
    #[error("shard {0} is not in the manifest")]
    NoShard(u32),
    /// The held file is not the shard the manifest lists.
    #[error("the held shard file does not match its manifest entry")]
    WrongFile,
    /// The entry is not the challenged shard.
    #[error("the entry is shard {got}, the challenge asks for {want}")]
    WrongShard {
        /// Asked.
        want: u32,
        /// Given.
        got: u32,
    },
    /// The entry does not prove to `shards_root`.
    #[error("the entry does not prove to the manifest's shards root")]
    EntryPath,
    /// The row does not prove to the entry's root at the challenged index.
    #[error("the row does not prove to the shard root at the challenged index")]
    RowPath,
    /// An entry with no rows.
    #[error("an entry with no rows")]
    Empty,
}

/// The 84 leaf bytes of an entry (`borsh(entry)`).
pub fn entry_bytes(e: &ShardEntry) -> Vec<u8> {
    e.leaf()
}

/// An entry from its 84 leaf bytes (`OnyxStorage._entry`).
pub fn parse_entry(b: &[u8]) -> Option<ShardEntry> {
    if b.len() != ENTRY_BYTES {
        return None;
    }
    borsh::from_slice(b).ok()
}

/// Build the proof for `(shard_index, row_index)` from the manifest and the
/// held shard file. Refuses a file that is not that shard.
pub fn prove(
    manifest: &Manifest,
    shard_file: &[u8],
    shard_index: u32,
    row_index: u64,
) -> Result<StorageProof, ProofError> {
    let entry = *manifest
        .shards
        .get(shard_index as usize)
        .ok_or(ProofError::NoShard(shard_index))?;
    let rows = split_rows(shard_file).map_err(|_| ProofError::WrongFile)?;
    let tree = MerkleTree::from_rows(&rows);
    if tree.root() != entry.root || tree.leaf_count() != entry.rows {
        return Err(ProofError::WrongFile);
    }
    let row_proof = tree.proof(row_index).ok_or(ProofError::RowPath)?;
    let entry_proof = shards_tree(&manifest.shards)
        .proof(u64::from(shard_index))
        .ok_or(ProofError::NoShard(shard_index))?;
    Ok(StorageProof {
        entry,
        entry_siblings: entry_proof.siblings,
        row: rows[row_index as usize].to_vec(),
        row_siblings: row_proof.siblings,
    })
}

/// The answer to slot `slot` of deal `deal` in the epoch with `seed`.
pub fn answer(
    manifest: &Manifest,
    shard_file: &[u8],
    seed: &Hash32,
    deal: u64,
    slot: u32,
) -> Result<StorageProof, ProofError> {
    let index = slot_shard(seed, deal, slot, manifest.header.shard_count);
    let entry = manifest
        .shards
        .get(index as usize)
        .ok_or(ProofError::NoShard(index))?;
    let row = slot_row(seed, deal, slot, entry.rows);
    prove(manifest, shard_file, index, row)
}

/// Check a proof exactly as `OnyxStorage.answer` does.
pub fn verify(
    shards_root: &Hash32,
    shard_count: u32,
    seed: &Hash32,
    deal: u64,
    slot: u32,
    p: &StorageProof,
) -> Result<(), ProofError> {
    let want = slot_shard(seed, deal, slot, shard_count);
    if p.entry.index != want {
        return Err(ProofError::WrongShard {
            want,
            got: p.entry.index,
        });
    }
    let ep = Proof {
        index: u64::from(want),
        leaf_count: u64::from(shard_count),
        siblings: p.entry_siblings.clone(),
    };
    if !onyx_merkle::verify(shards_root, &entry_bytes(&p.entry), &ep) {
        return Err(ProofError::EntryPath);
    }
    if p.entry.rows == 0 {
        return Err(ProofError::Empty);
    }
    let rp = Proof {
        index: slot_row(seed, deal, slot, p.entry.rows),
        leaf_count: p.entry.rows,
        siblings: p.row_siblings.clone(),
    };
    if !onyx_merkle::verify(&p.entry.root, &p.row, &rp) {
        return Err(ProofError::RowPath);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use onyx_bulk::manifest::NewVersion;
    use onyx_bulk::row::{encode_row, Cell};
    use onyx_bulk::shard::{check_shard, encode_shard};
    use onyx_bulk::Schema;
    use onyx_core::{DataRights, Redistribution, SourceId};

    /// A real manifest over `n` shards of `rows` rows each, and their files.
    pub(crate) fn dataset(n: u32, rows: i128) -> (Manifest, Vec<Vec<u8>>) {
        let schema = Schema::from_json(
            &serde_json::json!({"columns":[{"name":"t","kind":"int"},{"name":"v","kind":"text"}],"key":"t"}),
        )
        .expect("schema");
        let mut files = Vec::new();
        let mut entries = Vec::new();
        for s in 0..n {
            let rs: Vec<Vec<u8>> = (0..rows)
                .map(|i| {
                    let k = i128::from(s) * rows + i;
                    encode_row(&schema, &[Cell::Int(k), Cell::Text(format!("v{k}"))]).expect("row")
                })
                .collect();
            let f = encode_shard(&rs);
            let c = check_shard(&schema, &f).expect("shard");
            entries.push(ShardEntry::new(s, &c.summary));
            files.push(f);
        }
        let m = Manifest::build(
            NewVersion {
                dataset: "synthetic.storage-test".into(),
                publisher: SourceId("synthetic.publisher".into()),
                created_unix: 1_700_000_000,
                schema,
                rights: DataRights {
                    license: "CC0-1.0".into(),
                    redistribution: Redistribution::Allowed,
                    source: SourceId("synthetic.publisher".into()),
                    attribution: None,
                    terms: String::new(),
                },
            },
            None,
            entries,
        )
        .expect("manifest");
        (m, files)
    }

    #[test]
    fn honest_answers_verify_for_every_slot() {
        let (m, files) = dataset(5, 13);
        for e in 0..20u8 {
            let seed = Hash32([e; 32]);
            for slot in 0..3 {
                let idx = slot_shard(&seed, 4, slot, 5);
                let p = answer(&m, &files[idx as usize], &seed, 4, slot).expect("proves");
                verify(&m.header.shards_root, 5, &seed, 4, slot, &p).expect("verifies");
                assert_eq!(parse_entry(&entry_bytes(&p.entry)), Some(p.entry));
            }
        }
    }

    #[test]
    fn forgeries_fail() {
        let (m, files) = dataset(4, 9);
        let seed = Hash32([1; 32]);
        let idx = slot_shard(&seed, 1, 0, 4);
        let p = answer(&m, &files[idx as usize], &seed, 1, 0).expect("proves");
        let root = m.header.shards_root;
        // A changed byte in the row.
        let mut f = p.clone();
        let last = f.row.len() - 1;
        f.row[last] ^= 1;
        assert_eq!(verify(&root, 4, &seed, 1, 0, &f), Err(ProofError::RowPath));
        // Another row of the same shard (the wrong index).
        let other = slot_row(&seed, 1, 0, 9).wrapping_add(1) % 9;
        let q = prove(&m, &files[idx as usize], idx, other).expect("proves");
        assert_eq!(verify(&root, 4, &seed, 1, 0, &q), Err(ProofError::RowPath));
        // Another shard's proof.
        let o = (idx + 1) % 4;
        let r = prove(&m, &files[o as usize], o, 0).expect("proves");
        assert!(matches!(
            verify(&root, 4, &seed, 1, 0, &r),
            Err(ProofError::WrongShard { .. })
        ));
        // An entry with a changed row count (to move the asked index).
        let mut c = p.clone();
        c.entry.rows += 1;
        assert_eq!(
            verify(&root, 4, &seed, 1, 0, &c),
            Err(ProofError::EntryPath)
        );
        // The wrong file for an entry cannot even be proven.
        assert_eq!(
            prove(&m, &files[o as usize], idx, 0),
            Err(ProofError::WrongFile)
        );
        // The wrong shard count.
        assert!(verify(&root, 5, &seed, 1, 0, &p).is_err());
    }
}
