# ADR-0002: Borsh as the canonical serialization

## Status
Accepted, 2026-10-06 (M0).

## Context
Rows are hashed into Merkle trees, recipes emit bytes that validators compare, and
commitments go onchain. Every value needs exactly one encoding. Candidates: Borsh and
deterministic CBOR (RFC 8949 §4.2.1). Sources in RESEARCH.md §3.B.

## Decision
Borsh (`borsh` 1.x) for every hashed or compared structure: rows, claims, label sets,
commitments, recipe input and output.
Extra rules on top of Borsh, enforced by validation code and tests:
1. No floats in any schema (ADR-0009). Borsh can encode floats; we never do.
2. No `HashMap`/`HashSet` in schemas. Collections with set semantics are `Vec` kept
   strictly sorted and checked (`LabelSet::validate`, `Claim::validate` for quorum sources).
3. Fixed-point values are normalized (no trailing zero in the mantissa when scale > 0).
4. Decoding is strict: `from_canonical_bytes` rejects trailing bytes (tested). Ingest uses
   `Claim::decode`, which decodes and validates in one step.
5. Fixed-point scale is 1..=38; scale 0 must be `Int` (one number, one encoding).
6. Evidence lists are strictly ascending by `(kind, content_hash)`.
7. Identifiers follow the ASCII grammars in `onyx_core::ids` (no case variants, homoglyphs or whitespace).

## Why Borsh over deterministic CBOR
- Borsh's stated design goal is a canonical, deterministic encoding ([README](https://github.com/near/borsh)).
  Struct layout is positional, so there are no map-key ordering or integer-width choices to get wrong.
- Mature Rust crate used in production by NEAR and Solana programs. The deterministic CBOR
  crates we found (`cbor-core`, `cbor2`, `dcbor`) are all 0.x (RESEARCH.md §3.B).
- Smaller and simpler for fixed row schemas; decoding in Solidity is plain fixed offsets.

## Alternatives
- Deterministic CBOR: self-describing and multi-language, but canonical mode depends on the
  library and the Rust options are young. Revisit if non-Rust recipe languages arrive.
- SSZ: canonical and Merkle-friendly, but tied to the Ethereum consensus ecosystem and
  a less convenient schema story for arbitrary sectors.

## Consequences
- Not self-describing: every row type has a `SchemaId` (Keccak of its schema descriptor) in the
  commitment so a reader knows how to decode.
- Little-endian integers: Solidity decoders must byte-swap when reading row fields onchain
  (only the settlement endpoint needs this; Merkle verification treats rows as opaque bytes).
- Schema evolution = a new schema id. Old rows stay valid under their old id.
