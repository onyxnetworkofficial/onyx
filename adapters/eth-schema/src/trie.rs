//! The root of an ordered Merkle-Patricia trie (Yellow Paper, appendix D),
//! as a block header commits its transactions and receipts: item `i` is
//! stored under the key `RLP(i)` with the item's bytes as the value.
//!
//! Built bottom-up from the sorted keys: a single entry is a leaf, a shared
//! nibble prefix an extension, otherwise a 17-slot branch. A node whose
//! encoding is shorter than 32 bytes is embedded in its parent; any other is
//! referenced by its Keccak-256. The root is always hashed.

use crate::keccak256;
use crate::rlp::{encode_bytes, encode_list, encode_uint};
use alloc::vec::Vec;

/// Root of the empty trie: Keccak-256 of `RLP("")`.
pub const EMPTY_ROOT: [u8; 32] = [
    0x56, 0xe8, 0x1f, 0x17, 0x1b, 0xcc, 0x55, 0xa6, 0xff, 0x83, 0x45, 0xe6, 0x92, 0xc0, 0xf8, 0x6e,
    0x5b, 0x48, 0xe0, 0x1b, 0x99, 0x6c, 0xad, 0xc0, 0x01, 0x62, 0x2f, 0xb5, 0xe3, 0x63, 0xb4, 0x21,
];

fn nibbles(key: &[u8]) -> Vec<u8> {
    key.iter().flat_map(|b| [b >> 4, b & 0x0f]).collect()
}

/// Hex-prefix encoding of a nibble path (appendix C), with the leaf flag.
fn hex_prefix(path: &[u8], leaf: bool) -> Vec<u8> {
    let flag = if leaf { 2u8 } else { 0 };
    let mut out = Vec::with_capacity(path.len() / 2 + 1);
    let rest = if path.len() % 2 == 1 {
        out.push(((flag + 1) << 4) | path[0]);
        &path[1..]
    } else {
        out.push(flag << 4);
        path
    };
    for pair in rest.chunks(2) {
        out.push((pair[0] << 4) | pair[1]);
    }
    out
}

/// How a parent refers to a child node: embedded when shorter than 32 bytes,
/// else the RLP of its hash.
fn reference(out: &mut Vec<u8>, node: &[u8]) {
    if node.len() < 32 {
        out.extend_from_slice(node);
    } else {
        encode_bytes(out, &keccak256(node));
    }
}

/// The RLP of the node holding `entries` (sorted, distinct nibble keys) below `depth`.
fn node(entries: &[(Vec<u8>, &[u8])], depth: usize) -> Vec<u8> {
    let mut payload = Vec::new();
    if let [(key, value)] = entries {
        encode_bytes(&mut payload, &hex_prefix(&key[depth..], true));
        encode_bytes(&mut payload, value);
    } else {
        // Longest prefix shared by every key below depth: sorted, so first vs last.
        let (first, last) = (&entries[0].0, &entries[entries.len() - 1].0);
        let shared = first[depth..]
            .iter()
            .zip(&last[depth..])
            .take_while(|(a, b)| a == b)
            .count();
        if shared > 0 {
            encode_bytes(
                &mut payload,
                &hex_prefix(&first[depth..depth + shared], false),
            );
            reference(&mut payload, &branch(entries, depth + shared));
        } else {
            return branch(entries, depth);
        }
    }
    let mut out = Vec::with_capacity(payload.len() + 4);
    encode_list(&mut out, &payload);
    out
}

fn branch(entries: &[(Vec<u8>, &[u8])], depth: usize) -> Vec<u8> {
    let mut payload = Vec::new();
    let mut value: Option<&[u8]> = None;
    let mut rest = entries;
    if let Some((k, v)) = rest.first() {
        if k.len() == depth {
            value = Some(v);
            rest = &rest[1..];
        }
    }
    for nib in 0u8..16 {
        let n = rest.iter().take_while(|(k, _)| k[depth] == nib).count();
        if n == 0 {
            payload.push(0x80);
        } else {
            reference(&mut payload, &node(&rest[..n], depth + 1));
            rest = &rest[n..];
        }
    }
    match value {
        Some(v) => encode_bytes(&mut payload, v),
        None => payload.push(0x80),
    }
    let mut out = Vec::with_capacity(payload.len() + 4);
    encode_list(&mut out, &payload);
    out
}

/// Root of the trie holding `values[i]` under `RLP(i)`: a header's
/// `transactionsRoot` over the transactions' envelopes, or its
/// `receiptsRoot` over the receipts' envelopes.
pub fn ordered_root<V: AsRef<[u8]>>(values: &[V]) -> [u8; 32] {
    let keys: Vec<Vec<u8>> = (0..values.len())
        .map(|i| {
            let mut k = Vec::new();
            encode_uint(&mut k, i as u128);
            k
        })
        .collect();
    let pairs: Vec<(&[u8], &[u8])> = keys
        .iter()
        .zip(values)
        .map(|(k, v)| (k.as_slice(), v.as_ref()))
        .collect();
    root(&pairs)
}

/// Root of the trie holding each `(key, value)` (keys distinct, values not empty; any order).
pub fn root(pairs: &[(&[u8], &[u8])]) -> [u8; 32] {
    if pairs.is_empty() {
        return EMPTY_ROOT;
    }
    let mut entries: Vec<(Vec<u8>, &[u8])> = pairs.iter().map(|(k, v)| (nibbles(k), *v)).collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    keccak256(&node(&entries, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn the_empty_trie_is_the_hash_of_the_empty_string() {
        assert_eq!(keccak256(&[0x80]), EMPTY_ROOT);
        assert_eq!(ordered_root::<&[u8]>(&[]), EMPTY_ROOT);
    }

    #[test]
    fn hex_prefix_matches_the_yellow_paper() {
        assert_eq!(hex_prefix(&[1, 2, 3, 4, 5], false), vec![0x11, 0x23, 0x45]);
        assert_eq!(
            hex_prefix(&[0, 1, 2, 3, 4, 5], false),
            vec![0x00, 0x01, 0x23, 0x45]
        );
        assert_eq!(
            hex_prefix(&[0x0f, 1, 0x0c, 0x0b, 8], true),
            vec![0x3f, 0x1c, 0xb8]
        );
        assert_eq!(
            hex_prefix(&[0, 0x0f, 1, 0x0c, 0x0b, 8], true),
            vec![0x20, 0x0f, 0x1c, 0xb8]
        );
    }

    #[test]
    fn one_item_is_a_hashed_leaf() {
        // Key RLP(0) = 0x80, nibbles [8, 0]: leaf [0x20 0x80, value].
        let v = [0xaau8; 40];
        let mut payload = Vec::new();
        encode_bytes(&mut payload, &[0x20, 0x80]);
        encode_bytes(&mut payload, &v);
        let mut leaf = Vec::new();
        encode_list(&mut leaf, &payload);
        assert_eq!(ordered_root(&[v]), keccak256(&leaf));
    }

    /// Known answers from ethereum/tests `TrieTests/trieanyorder.json` ("dogs", "puppy"): short
    /// values, so nodes under 32 bytes are embedded, and a key ("do") that ends inside a branch.
    #[test]
    fn a_node_of_exactly_32_bytes_is_hashed_not_embedded() {
        // P4A-R2-4 (Yellow Paper appendix D: only a node under 32 bytes is embedded). Keys RLP(0)
        // = 0x80 and RLP(1) = 0x01 split at the root on nibbles 8 and 0. Item 1's leaf is
        // [hp([1], leaf) = 0x31, 29-byte value]: 1 + 1 + 30 = 32 bytes, so the branch holds its
        // hash; item 0's leaf [0x30, 0x05] is 3 bytes, embedded. The root, built here by hand:
        let v1 = [0xaa_u8; 29];
        let mut leaf1 = Vec::new();
        let mut p = vec![0x31];
        encode_bytes(&mut p, &v1);
        encode_list(&mut leaf1, &p);
        assert_eq!(leaf1.len(), 32);
        let leaf0 = vec![0xc2, 0x30, 0x05];
        let mut branch = Vec::new();
        encode_bytes(&mut branch, &keccak256(&leaf1));
        branch.extend([0x80; 7]);
        branch.extend(&leaf0);
        branch.extend([0x80; 8]);
        let mut node = Vec::new();
        encode_list(&mut node, &branch);
        let values: [&[u8]; 2] = [&[0x05], &v1];
        assert_eq!(ordered_root(&values), keccak256(&node));
    }

    #[test]
    fn known_answers_from_ethereum_tests() {
        let dogs: [(&[u8], &[u8]); 3] = [
            (b"doe", b"reindeer"),
            (b"dog", b"puppy"),
            (b"dogglesworth", b"cat"),
        ];
        assert_eq!(
            root(&dogs),
            [
                0x8a, 0xad, 0x78, 0x9d, 0xff, 0x2f, 0x53, 0x8b, 0xca, 0x5d, 0x8e, 0xa5, 0x6e, 0x8a,
                0xbe, 0x10, 0xf4, 0xc7, 0xba, 0x3a, 0x5d, 0xea, 0x95, 0xfe, 0xa4, 0xcd, 0x6e, 0x7c,
                0x3a, 0x11, 0x68, 0xd3
            ]
        );
        let puppy: [(&[u8], &[u8]); 4] = [
            (b"do", b"verb"),
            (b"horse", b"stallion"),
            (b"doge", b"coin"),
            (b"dog", b"puppy"),
        ];
        assert_eq!(
            root(&puppy),
            [
                0x59, 0x91, 0xbb, 0x8c, 0x65, 0x14, 0x14, 0x8a, 0x29, 0xdb, 0x67, 0x6a, 0x14, 0xac,
                0x50, 0x6c, 0xd2, 0xcd, 0x57, 0x75, 0xac, 0xe6, 0x3c, 0x30, 0xa4, 0xfe, 0x45, 0x77,
                0x15, 0xe9, 0xac, 0x84
            ]
        );
    }

    #[test]
    fn order_and_content_move_the_root() {
        let vals: Vec<Vec<u8>> = (0u8..200)
            .map(|i| vec![i; 1 + usize::from(i % 50)])
            .collect();
        let r = ordered_root(&vals);
        let mut swapped = vals.clone();
        swapped.swap(3, 4);
        assert_ne!(r, ordered_root(&swapped));
        let mut edited = vals.clone();
        edited[150][0] ^= 1;
        assert_ne!(r, ordered_root(&edited));
        assert_ne!(r, ordered_root(&vals[..199]));
        assert_eq!(r, ordered_root(&vals));
    }
}
