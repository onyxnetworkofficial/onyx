# ADR-0003: Keccak-256 and a positional, count-bound Merkle tree

## Status
Accepted, 2026-10-06 (M0). Implemented in `crates/onyx-merkle` and `contracts/src/OnyxMerkle.sol`,
cross-checked by `fixtures/merkle/vectors.json`.

## Context
Every row must be provable against a root stored on Base. Recipe ids must be content addresses.

## Decision
Hash: Keccak-256 everywhere (recipe ids, label set hashes, evidence, Merkle).
- On the EVM, `KECCAK256` costs 30 + 6 gas per word; the SHA-256 precompile costs 60 + 12 per word
  plus call overhead (RESEARCH.md §3.D). One hash for the whole system avoids mixups.

Tree layout:
- `leaf = keccak256(0x00 || row_bytes)`, `node = keccak256(0x01 || left || right)`.
- Leaves stay in committed order (rows sorted by their primary key, defined per schema).
- An odd last node is promoted unchanged, never duplicated.
- `root = keccak256(0x02 || leaf_count:u64 big-endian || top)`; empty tree uses `top = 0^32`.
- Proofs carry `index`, `leaf_count` and siblings; verification uses index bits, so proofs are positional.

## Why
- Domain prefixes stop an inner node being presented as a leaf (second-preimage shape).
- No duplication avoids the CVE-2012-2459 ambiguity where `[a,b,c]` and `[a,b,c,c]` share a root (tested).
- Binding `leaf_count` and positions means a miner cannot silently drop, add or reorder rows,
  and a sorted-key layout later supports non-membership proofs from two adjacent leaves.

## Alternatives
- OpenZeppelin `MerkleProof` (sorted pairs, double-hashed leaves): audited, but sorted pairs lose
  position, so no completeness or ordering guarantees. We reuse its double-hash idea via prefixes.
- Merkle Mountain Range or sparse Merkle tree: useful for append-only logs or key-value state; can
  be added later as a different `SchemaId` without changing the row lane.

## Consequences
- We own a ~40 line Solidity verifier; it is fuzzed in Foundry and must match Rust vectors in CI.
- Changing the layout is a breaking protocol change: new ADR, new vectors, new contract.
