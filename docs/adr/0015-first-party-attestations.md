# ADR-0015: First-party attestations need an address proof and an entity-key signature

## Status
Accepted, 2026-10-08 (M2 loop 12). Amended loops 13-15 after reviews (items 3, 4, 8, 9, 10, 11).
Implemented in `crates/onyx-labels/src/attest.rs`,
`crates/onyx-core` (`AddressProof`, registry entity keys) and `adapters/source-btc/src/address_proof.rs`.

## Context
The north star treats a first-party attestation as top-confidence label evidence. A BIP-322 or
EIP-191 signature by an address proves only that the signer controlled the key when signing
(BIP-322 says so itself: "no message signing protocol can actually prove control of funds").
It says nothing about who the signer is. Anyone can sign "my address belongs to entity:okx" for an
address they control and so inflate OKX's reserves or flows. R-ATTEST-1 also asked for a domain,
an expiry and newline-safe fields.

BIP-322 v2.0.0 (CC0, 2026) defines `smp` (simple), `ful` (full) and `pof` (proof of funds)
variants, a legacy fallback, and three verifier states: valid at time T and age S, inconclusive,
invalid. The `bip322` crate 0.0.12 (rust-bitcoin, CC0) implements them. RESEARCH.md, "M2: first-party
attestations", has the review of the crate.

## Decision
1. An attestation is one statement text plus two signatures over the same bytes:
   - **address signature** by the labelled address, checked by the namespace's `AddressProof`
     (registered like an `AddressCanon`, with vectors whose results are in the registry root);
   - **entity signature**: EIP-191 `personal_sign` by a 20-byte key that the registry lists for
     the entity (`add_entity_key`). Governance takes keys only from the entity's own published
     channels (governance.md). Entity keys are in the registry root.
2. Statement text (exact bytes, `\n` line ends, no trailing newline):
   ```
   Onyx address attestation v1
   domain: <dotted id, this network>
   entity: <entity id>
   address: <ns>:<canonical body>
   valid_from: <height in the address's chain>
   expires: <height, > valid_from>
   nonce: <32 lowercase hex>
   ```
   Parse is strict and must re-render to the same bytes. Every field follows an id grammar that
   cannot hold a newline. The window is at most the proof rule's `max_window` (BTC: 52,560 blocks).
3. Validity is judged over the shard's own block range: `assemble_input(spec, ...)` maps the
   snapshot's chain to an address namespace through the registry's proof rule (`AddressProof::chain`)
   and requires `valid_from <= from_height` and `to_height < expires`, so the statement holds for
   every block the data covers. The caller cannot choose the range (loop 14, L13-F3). The network
   domain is stored in the registry (`set_domain`), so it is covered by the pinned root.
4. BTC proof rule `onyx.proof.btc-mainnet.bip322.v1`, on top of the crate:
   `smp` or `ful` prefix required (no unprefixed, legacy or `pof`); `smp` only for P2WPKH, P2WSH,
   P2TR; canonical base64 and byte-exact witness/transaction re-serialization; no time locks
   (T = 0 and S = 0); mainnet addresses with a valid checksum; a verifier panic counts as invalid;
   inconclusive never counts as proof. Consensus limits the crate skips are checked too: a P2SH
   input with a witness needs its scriptSig, a 65-byte taproot signature may not use hash type 0x00
   (BIP-341), pushes are at most 520 bytes and a witness script at most 10,000 bytes.
   The rule's 27 registry vectors cover every accepted type and variant, every refusal and one
   inconclusive case. The registry root hashes each vector in full (body, message, signature) with
   the rule's result (loop 14, L13-F2), so a verifier that differs on any of them, or a rule with
   other vectors, has a different root.
5. EIP-191: `v` in {27, 28} and low `s` (EIP-2) only, so a signature has one accepted spelling.
6. Labels cite an attestation by `EvidenceRef { kind: "attestation", content_hash }`. A label at
   full confidence (1,000,000 ppm) must cite a valid attestation for exactly its address and entity.
7. Crypto dependencies are pinned exactly (`secp256k1 =0.29.1`, `bip322 =0.0.12`,
   `bitcoin =0.32.102`, `base64 =0.23.1`), and CI builds with `--locked`, so transitive crates
   (`secp256k1-sys`, `bech32`) come from the committed Cargo.lock.
8. Revocation: an entity revokes "address A is ours from height h" with an EIP-191 statement
   (`Onyx address revocation v1`) signed by any of its registered keys; no address key is needed.
   Revocations are recorded in the registry (`apply_revocation`) with their signing key and hash,
   and the registry root every shard pins covers them, so all validators apply the same set. An
   address counts as revoked for a shard if any recorded revocation is effective at or before the
   range's last block. Removing an entity key (`remove_entity_key`, governance) keeps the
   revocations it signed, except those governance lists by hash: a thief's revocations after a
   compromise can be undone (L13-F4), and retiring an honest key changes nothing (L14-F5).
9. The validator path checks attestations: `assemble_input` keeps only the labels that pass
   (`LabelSet::attested_subset`). A label whose cited attestation is missing, invalid, expired,
   revoked or for another address or entity is dropped, as is a full-confidence label without one.
   The rest of the set is used, so one expiry or revocation cannot void a whole set (loop 14,
   replacing the loop-13 "whole set fails" rule). Publishers can use the strict
   `check_attestations` before publishing. The set's content hash pins the hash of every cited
   attestation, so any party holding those bytes gets the same subset. A party that lacks a cited
   attestation abstains (`HostError::Abstain`); it never drops the label (loop 15, L14-F1).
   Making cited attestations available before a round opens (content-addressed storage) is part of
   M3. Uncited attestations a caller supplies are ignored. For block-range snapshots the source
   bytes must hash to the snapshot's `content_hash`.
10. Scope: attestations guard *full confidence* only. Labels below full confidence are the
   subjective lane: anyone may publish a set with any address at 0.99. That is kept in check by
   calibration scoring (ADR-0016), spread across label sets, and customers choosing sets, not by
   attestations.

11. Caps (L13-F5, L14-F8): at most 8 evidence entries and 2 attestation citations per label, 65,536
   cited attestations and 16 MiB of cited attestation bytes per check; the label-set pin is compared
   before any attestation work. The byte cap binds first. Worst case measured: 13,500 attestations
   of a 1-of-16 multisig signed by the last key (every key tried) took 8.8 s; up to about 14,650 fit,
   about 9.6 s (estimate), paid per shard (L15-F4) (docs/ops/benchmarks.md;
   the 64 MiB cap of loop 14 allowed 58,000 and 37.6 s). It needs a registered entity key. Loop 16: `AttestCache` keeps the signature verdict per
   (attestation, registry root), so this cost is paid once per registry, not per shard; only the
   cheap range check repeats (L15-F4).

## Consequences
- Without the entity key, an attacker cannot attest anything, even for addresses they control.
  With a stolen entity key, an attacker can attest any address they control until governance
  removes the key (or until expiry). The cost: entities must publish an Onyx signing key once,
  through governance.
- Attestations prove key control at signing time only. Expiry bounds how long that is trusted.
  Revocation (item 8) ends it earlier.
- Proof-of-funds, time-locked proofs and scripts the crate cannot run (P2TR script path, non-multisig
  P2WSH) are not accepted. Exchanges with such scripts must use a supported address type for now.
- EIP-712 is deferred: EIP-191 over the plain statement is enough until contracts must check
  attestations onchain (M6).
- The registry root changed shape (proofs and entity keys), so the golden shard id was re-blessed;
  the golden output root is unchanged.
