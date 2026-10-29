# ADR-0011: Commitment binding hash and one commit formula

## Status
Accepted, 2026-10-06 (M0 loop 2). Implemented in `crates/onyx-core/src/commitment.rs` and
`contracts/src/OnyxCommit.sol`, cross-checked by `fixtures/commit/vectors.json`.

## Context
Loop 1 review found three different commit formulas (spec, Solidity, Rust) and that neither the
commit nor the reveal bound the label set, snapshot, schema, leaf count or runtime (THREATS
R-BIND-1, R-COMMIT-2). A miner could pick whichever label set suited it during a dispute, and a
commit could be replayed across rounds or shards.

## Decision
1. `Commitment` gains `runtime: Option<RuntimeProfile>` (ABI version, host build id, fuel, memory,
   output and call-depth limits; present exactly when `recipe` is) and `rights_hash`.
2. Binding hash, fixed packing so Solidity can recompute it with `abi.encodePacked`:
   `keccak256("onyx-commitment-v1" || root || leaf_count:u64 BE || recipe_id || runtime_hash ||
   label_set_hash || source_hash || schema_id || rights_hash)`. Absent optional fields are 32 zero bytes.
   Nested structures (`RuntimeProfile`, `LabelSetRef`, `SourceRef`, `DataRights`) are hashed as
   Keccak of their Borsh encoding.
3. Commit hash: `keccak256("onyx-commit-v1" || round:u64 BE || shard || binding_hash || miner:20 bytes || salt)`.
4. `reveal` submits all commitment fields; the contract recomputes both hashes.
5. Shards (amended loop 3, R-SHARD-1): the coordinator fixes a `ShardSpec` (recipe, runtime profile,
   label set, source snapshot, schema, rights hash, params) before the round.
   `shard_fields_hash = keccak256("onyx-shard-fields-v1" || recipe_id || runtime_hash || label_set_hash ||
   source_hash || schema_id || rights_hash)`; `shard_id = keccak256("onyx-shard-v1" || shard_fields_hash ||
   keccak256(params))`. A reveal is accepted only if its fields hash to the registered `shard_fields_hash`,
   so the miner chooses nothing except the rows. `label_set_hash` always means `LabelSetRef::hash`
   (Keccak of the Borsh `LabelSetRef`, binding name, version and content hash), not the bare content hash.
6. Challenges are committed first: `keccak256("onyx-challenge-v1" || round || shard || claimed_root ||
   challenger || salt)`, then revealed (R-CHAL-2).
7. One commit per (miner, round, shard). A second commit by the same miner replaces nothing and is rejected.

## Alternatives
- Hash the Borsh encoding of the whole `Commitment`: simpler in Rust, but Solidity would need a
  Borsh encoder for nested optional structs. Rejected for onchain cost and mismatch risk.
- EIP-712 typed data: good wallet UX, but heavier and not needed for machine-generated commits.

## Consequences
- Any change to the packing is a breaking change: new tag (`-v2`), new vectors, new contract.
- A dispute recomputes against exactly the bound runtime profile, so "my host differs" is checkable.
