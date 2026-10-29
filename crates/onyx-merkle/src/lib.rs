//! Positional Keccak-256 Merkle tree over canonically encoded rows (ADR-0003).
//!
//! Layout:
//! - `leaf = keccak256(0x00 || row_bytes)`
//! - `node = keccak256(0x01 || left || right)`
//! - An odd node at the end of a level is promoted unchanged (never duplicated,
//!   which avoids the CVE-2012-2459 duplicate-leaf ambiguity).
//! - `root = keccak256(0x02 || leaf_count as u64 big-endian || tree_top)`,
//!   with `tree_top = 0x00..00` for an empty tree.
//!
//! Domain prefixes stop a leaf being passed off as an inner node. Committing the
//! leaf count makes every root bind its exact row count, and positional proofs
//! bind each row to its index, so a miner cannot drop or reorder rows unseen.
//! The Solidity verifier in `contracts/src/OnyxMerkle.sol` must match this file;
//! `fixtures/merkle/vectors.json` is the shared test vector.

use onyx_core::{keccak256, Hash32};

const LEAF_PREFIX: u8 = 0x00;
const NODE_PREFIX: u8 = 0x01;
const ROOT_PREFIX: u8 = 0x02;

/// Hash one canonically encoded row into a leaf.
pub fn leaf_hash(row: &[u8]) -> Hash32 {
    let mut buf = Vec::with_capacity(1 + row.len());
    buf.push(LEAF_PREFIX);
    buf.extend_from_slice(row);
    keccak256(&buf)
}

fn node_hash(left: &Hash32, right: &Hash32) -> Hash32 {
    let mut buf = [0u8; 65];
    buf[0] = NODE_PREFIX;
    buf[1..33].copy_from_slice(&left.0);
    buf[33..].copy_from_slice(&right.0);
    keccak256(&buf)
}

fn final_root(leaf_count: u64, top: &Hash32) -> Hash32 {
    let mut buf = [0u8; 41];
    buf[0] = ROOT_PREFIX;
    buf[1..9].copy_from_slice(&leaf_count.to_be_bytes());
    buf[9..].copy_from_slice(&top.0);
    keccak256(&buf)
}

/// Inclusion proof for one row.
#[derive(Clone, Debug, PartialEq, Eq, borsh::BorshSerialize, borsh::BorshDeserialize)]
pub struct Proof {
    /// Zero-based row index.
    pub index: u64,
    /// Total rows in the tree.
    pub leaf_count: u64,
    /// Sibling hashes from the leaf level upward. Promoted levels contribute none.
    pub siblings: Vec<Hash32>,
}

/// A built tree. Keeps every level so proofs are cheap.
#[derive(Clone, Debug)]
pub struct MerkleTree {
    levels: Vec<Vec<Hash32>>,
    leaf_count: u64,
    root: Hash32,
}

impl MerkleTree {
    /// Build from rows in their committed order.
    pub fn from_rows<R: AsRef<[u8]>>(rows: &[R]) -> Self {
        Self::from_leaves(rows.iter().map(|r| leaf_hash(r.as_ref())).collect())
    }

    /// Build from precomputed leaf hashes.
    pub fn from_leaves(leaves: Vec<Hash32>) -> Self {
        let leaf_count = leaves.len() as u64;
        let mut levels = vec![leaves];
        while levels.last().is_some_and(|l| l.len() > 1) {
            let prev = levels.last().map(Vec::as_slice).unwrap_or_default();
            let next: Vec<Hash32> = prev
                .chunks(2)
                .map(|pair| match pair {
                    [l, r] => node_hash(l, r),
                    [only] => *only,
                    _ => unreachable!("chunks(2) yields 1 or 2 items"),
                })
                .collect();
            levels.push(next);
        }
        let top = levels
            .last()
            .and_then(|l| l.first())
            .copied()
            .unwrap_or(Hash32::ZERO);
        let root = final_root(leaf_count, &top);
        MerkleTree {
            levels,
            leaf_count,
            root,
        }
    }

    /// The committed root.
    pub fn root(&self) -> Hash32 {
        self.root
    }

    /// Number of rows.
    pub fn leaf_count(&self) -> u64 {
        self.leaf_count
    }

    /// Proof for row `index`, or `None` if out of range.
    pub fn proof(&self, index: u64) -> Option<Proof> {
        if index >= self.leaf_count {
            return None;
        }
        let mut siblings = Vec::new();
        let mut idx = usize::try_from(index).ok()?;
        for level in &self.levels[..self.levels.len() - 1] {
            let sib = idx ^ 1;
            if sib < level.len() {
                siblings.push(level[sib]);
            }
            idx /= 2;
        }
        Some(Proof {
            index,
            leaf_count: self.leaf_count,
            siblings,
        })
    }
}

/// Verify that `row` sits at `proof.index` under `root`.
pub fn verify(root: &Hash32, row: &[u8], proof: &Proof) -> bool {
    verify_leaf(root, &leaf_hash(row), proof)
}

/// Verify a precomputed leaf hash.
pub fn verify_leaf(root: &Hash32, leaf: &Hash32, proof: &Proof) -> bool {
    if proof.index >= proof.leaf_count {
        return false;
    }
    let mut h = *leaf;
    let mut idx = proof.index;
    let mut n = proof.leaf_count;
    let mut sibs = proof.siblings.iter();
    while n > 1 {
        if idx % 2 == 1 {
            match sibs.next() {
                Some(s) => h = node_hash(s, &h),
                None => return false,
            }
        } else if idx + 1 < n {
            match sibs.next() {
                Some(s) => h = node_hash(&h, s),
                None => return false,
            }
        }
        idx /= 2;
        n = n.div_ceil(2);
    }
    sibs.next().is_none() && final_root(proof.leaf_count, &h) == *root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: usize) -> Vec<Vec<u8>> {
        (0..n).map(|i| format!("row-{i}").into_bytes()).collect()
    }

    #[test]
    fn every_proof_verifies_for_sizes_1_to_40() {
        for n in 1..=40 {
            let r = rows(n);
            let t = MerkleTree::from_rows(&r);
            // The count a root commits to is the row count (mutation L4).
            assert_eq!(t.leaf_count(), n as u64);
            for (i, row) in r.iter().enumerate() {
                let p = t.proof(i as u64).expect("in range");
                assert!(verify(&t.root(), row, &p), "n={n} i={i}");
            }
            assert!(t.proof(n as u64).is_none());
        }
    }

    #[test]
    fn tampering_fails() {
        let r = rows(13);
        let t = MerkleTree::from_rows(&r);
        let p = t.proof(5).expect("in range");
        assert!(!verify(&t.root(), b"row-x", &p));
        let mut wrong_index = p.clone();
        wrong_index.index = 4;
        assert!(!verify(&t.root(), &r[5], &wrong_index));
        let mut wrong_count = p.clone();
        wrong_count.leaf_count = 14;
        assert!(!verify(&t.root(), &r[5], &wrong_count));
        let mut extra = p.clone();
        extra.siblings.push(Hash32::ZERO);
        assert!(!verify(&t.root(), &r[5], &extra));
        let mut short = p;
        short.siblings.pop();
        assert!(!verify(&t.root(), &r[5], &short));
    }

    #[test]
    fn root_binds_count_and_order() {
        let a = MerkleTree::from_rows(&rows(3));
        let mut swapped = rows(3);
        swapped.swap(0, 1);
        assert_ne!(a.root(), MerkleTree::from_rows(&swapped).root());
        // Duplicating the odd last leaf must not give the same root (CVE-2012-2459 shape).
        let mut dup = rows(3);
        dup.push(dup[2].clone());
        assert_ne!(a.root(), MerkleTree::from_rows(&dup).root());
    }

    #[test]
    fn inner_node_cannot_pose_as_leaf() {
        let r = rows(4);
        let t = MerkleTree::from_rows(&r);
        // Concatenate two leaf hashes, as an attacker would, and try to prove it as a row.
        let mut fake = Vec::new();
        fake.extend_from_slice(&leaf_hash(&r[0]).0);
        fake.extend_from_slice(&leaf_hash(&r[1]).0);
        let p = Proof {
            index: 0,
            leaf_count: 2,
            siblings: vec![t.levels[1][1]],
        };
        assert!(!verify(&t.root(), &fake, &p));
    }

    #[test]
    fn empty_tree_root_is_defined() {
        let t = MerkleTree::from_rows::<Vec<u8>>(&[]);
        assert_eq!(t.leaf_count(), 0);
        assert!(t.proof(0).is_none());
    }

    /// Shared vector with the Solidity verifier. Regenerate with `ONYX_BLESS=1 cargo test -p onyx-merkle`.
    #[test]
    fn solidity_vectors_match() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/merkle/vectors.json"
        );
        let mut cases = Vec::new();
        for n in [1usize, 2, 3, 5, 8, 13, 33, 64, 65, 129] {
            let r = rows(n);
            let t = MerkleTree::from_rows(&r);
            for (i, row) in r.iter().enumerate() {
                let p = t.proof(i as u64).expect("in range");
                cases.push(serde_json::json!({
                    "root": format!("0x{}", t.root().to_hex()),
                    "row": format!("0x{}", hex::encode(row)),
                    "index": i,
                    "leafCount": n,
                    "siblings": p.siblings.iter().map(|s| format!("0x{}", s.to_hex())).collect::<Vec<_>>(),
                }));
            }
        }
        let doc = serde_json::json!({ "count": cases.len(), "cases": cases });
        let text = serde_json::to_string_pretty(&doc).expect("json") + "\n";
        if std::env::var_os("ONYX_BLESS").is_some() {
            std::fs::create_dir_all(std::path::Path::new(path).parent().expect("parent"))
                .expect("mkdir");
            std::fs::write(path, &text).expect("write vectors");
        }
        let on_disk = std::fs::read_to_string(path)
            .expect("fixtures/merkle/vectors.json missing; run with ONYX_BLESS=1");
        assert_eq!(on_disk, text, "Merkle vectors drifted from ADR-0003 layout");
    }
}
