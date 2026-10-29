# ADR-0013: Recipe ABI v1, output rows and reproducible builds

## Status
Accepted, 2026-10-06 (M1 loop 6). Implemented in `crates/onyx-recipe-sdk`, `crates/onyx-recipe-host`,
`scripts/build-recipes.sh`.

## Context
M1 needs a fixed contract between recipes and the host, a rule for what gets committed, and a way
to show that "same source, same recipe id" holds on a clean machine.

## Decision
1. Exports: `onyx_abi_version() -> u32` (= 1), `onyx_alloc(len) -> ptr`, `onyx_run(ptr, len) -> u64`
   (packed out ptr/len), `memory`. No imports.
2. Input: Borsh `RecipeInput { abi_version, source, labels, params }`, built only by
   `assemble_input`, which checks the label set against the shard pin and the registry.
3. Output: Borsh `RunReturn::Ok(RecipeOutput { rows: Vec<OutputRow { key, value }> })` or
   `RunReturn::Err(code)`. Rows must be non-empty and strictly ascending by `key` (unique keys).
4. Merkle leaves: Borsh of each `OutputRow`. Any failure (trap, out of fuel, recipe error, bad or
   oversized output) commits exactly one leaf, `borsh(("onyx-failure-v1", Outcome))`.
5. Builds: `scripts/build-recipes.sh` (pinned toolchain, LTO, one codegen unit, `opt-level=s`,
   stripped symbols, `--remap-path-prefix` for repo, cargo home and sysroot). Artifacts and their
   Keccak ids are committed in `recipes/out/`; CI rebuilds them on a clean runner and fails on any
   byte difference.
6. Recipes run on a host whose build id is pinned in the `RuntimeProfile`; golden tests require
   fuel use at most half the pinned budget.

## Alternatives
- Output as one opaque blob hashed once: simpler, but no per-row proofs.
- WASI with a denied capability set: larger surface; the empty import set is easier to audit.

## Consequences
- A change in rustc, LTO or the SDK changes recipe ids. That is intended: a recipe id names exact bytes.
- Recipes cannot log. Debugging happens by running the same source natively (the golden test does).
