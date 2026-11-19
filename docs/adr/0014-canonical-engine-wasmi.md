# ADR-0014: wasmi is the canonical engine; wasmtime is an advisory fast path

## Status
Accepted, 2026-10-08 (loop 8). Supersedes the engine choice in ADR-0004 (its determinism
profile still applies). Implemented in `crates/onyx-recipe-host` (`Host` = wasmi, `fast::FastHost` = wasmtime).

## Context
Two reviews (R-HOST-STACK-1, R-HOST-STACK-2) showed that on wasmtime/Cranelift the native stack
frame of a function is not bounded by its wasm frame: with optimizations on, Cranelift hoists loop
invariants and one 7-slot function got a ~250 KiB native frame, so recursion below the pinned depth
overflowed the native stack. Frame size depends on ISA, Cranelift version and CPU features, so honest
validators could disagree. Research (RESEARCH.md, "Deterministic stack"): NEAR, Polkadot and Arbitrum
Stylus all count a logical stack and then rely on native headroom; Polkadot's 8-bytes-per-value
assumption is the one our reproducer breaks, and Stylus needed a native-overflow fallback.

## Decision
1. Verdicts (committed results and dispute recomputes) come only from **wasmi 2.0.0**
   (`=2.0.0`, features `std, validate, deterministic, portable-dispatch`):
   - call stack and value stack are heap-allocated with explicit limits:
     `set_max_recursion_depth(profile.max_call_depth)`, `set_max_stack_height(WASMI_MAX_STACK_BYTES)`
     (1024 x 2048 slots x 8 bytes = 16 MiB). Exceeding either is a deterministic trap;
   - fuel counted on wasm bytecode; `CompilationMode::Eager` (lazy modes charge fuel on first use);
   - `floats(false)` plus our validator allowlist; `portable-dispatch` so the interpreter loop does
     not rely on compiler tail calls (native stack).
2. `HOST_ID = "onyx-host.0.5.0.wasmi.2.0.0"` names this engine; `RuntimeProfile.host` pins it.
3. wasmtime 49.0.6 (was 49.0.2; bumped 2026-10-14 to 49.0.3 for RUSTSEC-2026-0353..0357, 2026-10-20 to 49.0.4 for RUSTSEC-2026-0378/0379, 2026-10-30 to 49.0.5 for RUSTSEC-2026-0422/0423, 2026-11-19 to 49.0.6 for RUSTSEC-2026-0571..0573) stays as `fast::FastHost` with **Cranelift `OptLevel::None`**, the call-depth
   instrumentation and the frame-slot bound. Its results are advisory: they are never committed, and
   its fuel numbers differ from wasmi's. Tests require both engines to agree on outcome and leaves.
4. Load caps from measurement (docs/ops/benchmarks.md): module <= 1 MiB, function <= 256 KiB,
   plus br_table, arity, nesting, block, typed-block and function-count caps (loops 9-10).
5. Amendment (loop 10): the fast path compiles only **allowlisted** recipe ids (reviewed, registered
   recipes). Reviews found Cranelift compile shapes with super-linear memory/time that pass every
   load cap (L10-F1: single-result blocks, 7.3 GB at 184 KB; L10-F2: locals x loop back-edges, 1.8 GB
   and 20 s at 52 KB). wasmi loads all of them in about 0.02 s. Unreviewed recipes run only on wasmi.
   A sandboxed child-process compile with memory and time limits is the follow-up if the fast path is
   ever needed for unreviewed code.

## Alternatives
- (c) wasmtime with optimizer off and tight caps: measured 8x margin on x86_64 only; still not
  deterministic by construction (wasmtime issue #14075: frames grow with locals and differ by target).
- NEAR-style logical stack + Winch: Winch is incompatible with fuel in wasmtime 49 (RESEARCH.md).
- A zkVM: future option for onchain verification, not needed for M1.

## Consequences
- Execution is slower: 0.125 s vs 0.016 s for the reference recipe on 3 blocks (about 8x).
- Single-maintainer dependency; the wasmi 2.0 executor is not yet audited (0.31 and 0.36-0.38 were).
  Pinned exactly; upgrades are a new `HOST_ID` and need the golden test to pass unchanged.
- Root and rows of the golden run were identical on wasmtime and wasmi; only the shard id changed
  (it hashes the runtime profile, which names the engine).

## Amendment (planned, 2026-11-19): the move off wasmi 2.0.0
Not decided yet; decide again by 2026-12-03 (READINESS A35).
- Why: 2.0.0 was yanked when 2.0.1 came out (2026-11-17). The pin stays as a dated exception (THREATS DEP-6).
- Measured (trial, this repository at `cd37ca61` with `wasmi` and `wasmi_core` at `=2.0.1`, every workspace test,
  `HOST_ID` left as it is): everything passes but three golden tests in `adapters/source-btc`
  (`golden_chain`, `golden_reserve`, `golden_series`). In each one only the `fuel` number differs
  (123974 to 124782, 755434559 to 758375753, 738953128 to 741868661: 0.4 % to 0.7 % more); every root and
  every row is the same. The cause is the 2.0.1 rule that charges 1 fuel per 8 zero-initialised locals.
- So the move is a protocol change, but a small one: a new `HOST_ID` (which changes every shard id, as the
  first move to wasmi did), new fuel numbers in the golden files and vectors, and the recorded runs made again.
  No result changes.
- Upstream lists three translation fixes as not released yet (multi-value `br_table`, `if` blocks with
  parameters, `loop` blocks with parameters). Plan: move once, to the release that holds them, so that the
  host id changes one time and not two.
- Steps then: (1) pin the new version in `crates/onyx-recipe-host` and both lock files; (2) new `HOST_ID`;
  (3) bless the golden files and the fuel vectors, with the diff showing fuel and shard ids only;
  (4) differential run against wasmtime; (5) record the published windows again under the new host id and
  keep the old ones readable; (6) remove the two `deny.toml` exceptions and close DEP-6.
