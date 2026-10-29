# BIP-322 test vectors

Copied byte for byte from the BIPs repository, BIP-322 v2.0.0, license CC0-1.0:

- https://github.com/bitcoin/bips/blob/master/bip-0322/basic-test-vectors.json
- https://github.com/bitcoin/bips/blob/master/bip-0322/generated-test-vectors.json

Latest upstream commit touching `bip-0322/` when copied (2026-10-08): `d77863fb9e9be7829ad8bb51694b9ba80a786766`.
The generated vectors were made with btcd (https://github.com/btcsuite/btcd/pull/2521), so they
check the Rust verifier against an independent implementation.

The files contain the vectors' private keys. They are public test keys from the BIP. They are not
Onyx keys and must never hold funds. Checksums are in `../SHA256SUMS`.

Used by `adapters/source-btc/tests/bip322.rs`.
