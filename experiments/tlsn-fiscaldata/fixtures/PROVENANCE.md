# Provenance of the TLSNotary fixtures

- `presentation.bin`: made by `cargo run --locked --release --bin prove` on 2026-10-21 07:26:35 UTC
  (unix 1792567595) on the maintainer's dev box, against `api.fiscaldata.treasury.gov` over TLS
  1.2, User-Agent `onyx-dev`, tlsn `v0.1.0-alpha.15`. The notary ran in the prover's process with
  a random secp256k1 key that was dropped after the run; its public key is in `summary.json`.
- `summary.json`: what `prove` printed for that run (sizes, timings, hashes, the value).
- The response is U.S. Treasury Fiscal Data, public domain; rights reviewed in
  `adapters/source-fiscaldata/fixtures/PROVENANCE.md`. The body equals that adapter's recorded
  fetch of the same URL byte for byte (`recorded_presentation_verifies`).
- The hidden `X-Demo-Key` value was 32 random bytes generated for the run, not a real key, and
  never stored.
