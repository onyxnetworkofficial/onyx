# EIP-191 cross-check vectors

`vectors.json` holds EIP-191 `personal_sign` signatures made by an independent implementation,
Foundry `cast` 1.8.5, so the Rust verifier in `onyx-labels::attest` is not only checked against itself.

Key: `cast keccak "onyx test entity key 1"`. It is a test key derived from a public label. It is
not a real key and must never hold funds on any network.

Regenerate (only with an ADR change):

```sh
K=$(cast keccak "onyx test entity key 1")
cast wallet address --private-key $K
cast wallet sign --private-key $K "<message>"
```
