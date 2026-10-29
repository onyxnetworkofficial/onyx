# ADR-0001: Rust workspace for all core components

## Status
Accepted, 2026-10-06 (M0).

## Context
Onyx needs a deterministic WASM host, Merkle code, miners, validators, a coordinator
and an API that must agree byte for byte. The owner already runs Rust indexers
(the entity indexer) whose logic seeds the first recipe.

## Decision
- One Cargo workspace, edition 2021, toolchain pinned to 1.98.0 in `rust-toolchain.toml`
  with `wasm32-unknown-unknown`, rustfmt and clippy.
- `unsafe_code = "forbid"` and `clippy::float_arithmetic = "deny"` workspace-wide.
- CI gates: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`,
  `scripts/check-boundaries.sh`, `scripts/check-docs.sh`.
- Contracts are Solidity (ADR-0005). The only non-Rust runtime allowed is a thin
  x402 sidecar if ADR-0006 ever falls back to it.

## Alternatives
- Go: good networking, but weaker WASM-host ecosystem than wasmtime's native Rust API.
- TypeScript: fastest for the API, but a second language for determinism-critical code.

## Consequences
- One language for recipe SDK and host, so ABI types are shared, not duplicated.
- Recipe authors must write Rust (or anything that targets WASM without floats) at first.
- Toolchain bumps are explicit PRs; a bump can change codegen, so golden tests (M1) must pass first.
