//! Merkle proof verification (ADR-0003): never panics on any proof, every
//! honest proof verifies, and a proof that differs from the honest one for
//! its index (any index, count, sibling or row change) is refused. A proof
//! that verifies against a tree's root is exactly that tree's proof for
//! its index, over that index's row.
#![no_main]

use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use onyx_core::Hash32;
use onyx_merkle::{leaf_hash, verify, verify_leaf, MerkleTree, Proof};

#[derive(Arbitrary, Debug)]
struct Input {
    rows: Vec<Vec<u8>>,
    /// Extra rows of no content, to reach larger trees cheaply.
    pad: u8,
    pick: u16,
    /// A proof the fuzzer makes up.
    index: u64,
    leaf_count: u64,
    siblings: Vec<[u8; 32]>,
    row: Vec<u8>,
    flip: u16,
}

fuzz_target!(|i: Input| {
    let mut rows = i.rows;
    rows.truncate(256);
    for k in 0..u16::from(i.pad) {
        rows.push(k.to_be_bytes().to_vec());
    }
    let tree = MerkleTree::from_rows(&rows);
    let root = tree.root();
    let n = tree.leaf_count();
    assert_eq!(n, rows.len() as u64);
    // A made-up proof: never panics; if it verifies it is the honest one.
    let made = Proof {
        index: i.index,
        leaf_count: i.leaf_count,
        siblings: i.siblings.iter().map(|s| Hash32(*s)).collect(),
    };
    if verify(&root, &i.row, &made) {
        let honest = tree
            .proof(made.index)
            .expect("a verified index is in range");
        assert_eq!(made, honest, "a non-canonical proof verified");
        assert_eq!(rows[made.index as usize], i.row, "another row verified");
    }
    let _ = verify_leaf(&Hash32::ZERO, &leaf_hash(&i.row), &made);
    if n == 0 {
        assert!(tree.proof(0).is_none());
        return;
    }
    let idx = u64::from(i.pick) % n;
    let p = tree.proof(idx).expect("in range");
    let r = &rows[idx as usize];
    assert!(verify(&root, r, &p), "honest proof refused");
    assert!(tree.proof(n).is_none());
    // Tampering: each change is refused.
    let mut q = p.clone();
    q.index = q.index.wrapping_add(1 + u64::from(i.flip));
    if rows.get(q.index as usize) != Some(r) {
        assert!(!verify(&root, r, &q), "moved index verified");
    }
    let mut q = p.clone();
    q.leaf_count = q.leaf_count.wrapping_add(1 + u64::from(i.flip));
    assert!(!verify(&root, r, &q), "changed count verified");
    let mut q = p.clone();
    q.siblings.push(Hash32::ZERO);
    assert!(!verify(&root, r, &q), "extra sibling verified");
    if !p.siblings.is_empty() {
        let mut q = p.clone();
        let k = usize::from(i.flip) % q.siblings.len();
        q.siblings[k].0[usize::from(i.flip) % 32] ^= 1 << (i.flip % 8);
        assert!(!verify(&root, r, &q), "flipped sibling verified");
        let mut q = p.clone();
        q.siblings.pop();
        assert!(!verify(&root, r, &q), "dropped sibling verified");
    }
    let mut other = r.clone();
    other.push(0);
    assert!(!verify(&root, &other, &p), "another row verified");
    // An inner node is never accepted as a leaf.
    assert!(!verify_leaf(&root, &root, &p) || leaf_hash(r) == root);
});
