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
3. wasmtime 49.0.5 (was 49.0.2; bumped 2026-10-14 to 49.0.3 for RUSTSEC-2026-0353..0357, 2026-10-20 to 49.0.4 for RUSTSEC-2026-0378/0379, 2026-10-30 to 49.0.5 for RUSTSEC-2026-0422/0423) stays as `fast::FastHost` with **Cranelift `OptLevel::None`**, the call-depth
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
