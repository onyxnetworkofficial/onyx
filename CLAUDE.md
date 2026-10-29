# Working in the Onyx repo

Onyx is a contributor-run data network: miners supply metrics and entity labels, validators
rerun open WASM recipes, finalized Merkle roots go to Base. Read README.md, PLAN.md and STATUS.md first.

## Commands
- All checks (mirror of CI): `scripts/check-all.sh`
- Rust: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
- Contracts: `cd contracts && forge fmt && forge test` (Foundry in `~/.foundry/bin`)
- Gas snapshot, after an intended gas change: `cd contracts && FOUNDRY_PROFILE=gas forge snapshot --no-match-test 'testFuzz_|invariant_' --no-match-contract Fork`, commit `.gas-snapshot` (check-all runs it with `--check`).
- Regenerate shared vectors (only with an ADR change): `ONYX_BLESS=1 cargo test -p onyx-merkle -p onyx-core`
- Boundaries: `scripts/check-boundaries.sh`. Doc structure: `scripts/check-docs.sh`.
- Benchmarks (release): `cargo run --release -p onyx-source-btc --example bench-golden`,
  `cargo run --release -p onyx-recipe-host --example bench-compile` (and `bench-frames`); results in docs/ops/benchmarks.md.
  Attestations: `cargo run --release -p onyx-source-btc --example bench-attest -- 20000`.
- BIP-322 registry vectors: `cargo run -p onyx-source-btc --example gen-proof-vectors > adapters/source-btc/src/proof_vectors.rs` (only with a new `proof_id`).
- Recipes: `scripts/build-recipes.sh` (reproducible; commit `recipes/out/`). Golden test:
  `cargo test -p onyx-source-btc --test golden` (bless with `ONYX_BLESS=1` only with a recipe change).
- Fuzzing (opt-in, nightly + cargo-fuzz; not in check-all): `scripts/fuzz-smoke.sh` (30 s per
  target in `fuzz/`; `FUZZ_SECS`, `FUZZ_TARGETS`). Report: docs/audit/2026-10-13-fuzzing.md.
- Mutation testing (opt-in, `cargo install --locked cargo-mutants`; not in check-all):
  `scripts/mutants-smoke.sh` (merkle, claim quorum, revenue split, x402 precheck; about 30 min).
  Report: docs/audit/2026-10-13-mutation.md.
- Heavy builds (wasmtime, release): run under `systemd-run --user --scope -p MemoryMax=16G` on the dev box.

## Conventions
- Core crates (`crates/*`) are sector-agnostic and never depend on `adapters/*` or `recipes/*`.
- No floats anywhere in protocol data (ADR-0009). Amounts in smallest units; ppm for probabilities.
- Every hashed structure is Borsh (ADR-0002), except hashes the EVM recomputes, which use the fixed packing in ADR-0011. Every hash is Keccak-256 (ADR-0003).
- Sets are sorted `Vec`s with a validate check, never `HashMap`/`HashSet` in schemas.
- Tests first for protocol rules. Every threat in THREATS.md names its test or a TODO with a milestone.
- `unwrap()` only in tests. Error enums with `thiserror`.
- No wall-clock asserts in tests (ROADMAP E12): never "took < N s" or a fixed sleep before a check. Use
  controlled time (tokio `start_paused`, the injected clock in `scripts/health-watch-tests.py`), wait for
  the condition the check reads (`apps/web/tests/wait.mjs`, a signal from the code under test), or a bound
  tied to the configured deadline with a documented margin. A real-time wait is a condition with a cap far
  above any run, never a guess at the machine's speed.

## Loop (per milestone, max 6 loops per session)
1. Research prior art; add to RESEARCH.md with links.
2. Design: update docs/spec; ADR for any real decision; assumptions in the spec; questions in PLAN.md.
3. Build in small tested steps.
4. Test: `scripts/check-all.sh` must pass. Never end a loop red.
5. Adversarial review by a fresh subagent (cheating miner, lazy validator, griefing challenger,
   data thief). Every valid finding goes in THREATS.md with a test or a TODO.
6. Update PLAN.md checklist; overwrite the top of STATUS.md (phone format, max 15 lines); commit.
Stop after each milestone passes and wait for review. Same problem fails 3 loops in a row: stop and ask.

## Hard limits
- Testnets only. Only the testnet key in `.env`. Never print it, never commit `.env`.
- Ask before installing anything paid, signing up for services, or spending money.
- Labels name entities only (exchanges, ETFs, funds, protocols). Never individuals.
- Never ingest data whose license forbids redistribution.
- No invented benchmarks or claims about other projects; link sources.
- Commit after each passing loop. Never rewrite history.
