# Provenance: the synthetic keyed provider (CL4 demo FIXTURE)

| Field | Value |
|---|---|
| Source | None: a loopback fake provider started by `level2-keyed provider` (crates/onyx-level2/examples/level2-keyed.rs) for the local keyed demo (`scripts/level2-keyed-anvil.sh`) and the tests |
| Terms | Synthetic, CC0; stands in for a provider whose terms allow passing on data obtained with a personal key (ADR-0054 item 9) |
| Access | A FAKE key per holder in the `api_key` query parameter; the fake answers by the key's Keccak-256 and keeps no key |
| Recording | `recording/2026-10-27.json`, synthetic (ADR-0054 item 2: rehearsal recordings of keyed sources are synthetic) |
