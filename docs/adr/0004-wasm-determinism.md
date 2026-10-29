# ADR-0004: wasmtime with a strict determinism profile

## Status
Accepted, 2026-10-06 (M0). Implemented in M1. Engine choice superseded by ADR-0014
(wasmi is canonical; wasmtime is an advisory fast path). The determinism profile below still applies.

## Context
Recipes must give byte-identical output on every validator, across thread counts, machines and
rebuilds. Sources in RESEARCH.md §3.A.

## Decision
- Target `wasm32-unknown-unknown`. Recipe id = Keccak-256 of the exact `.wasm` bytes.
- Host: `wasmtime`, configured with:
  - `consume_fuel(true)`; fuel exhaustion is a deterministic trap. Epoch interruption is not used
    for verdicts (wasmtime docs say it is not deterministic).
  - `cranelift_nan_canonicalization(true)` as defence in depth.
  - Threads, relaxed SIMD and other nondeterministic proposals disabled.
  - Memory limit via a store limiter.
- Load-time validation with `wasmparser` before instantiation:
  - Reject any `f32`/`f64` value type or float instruction anywhere (types, locals, globals, code).
  - Reject any import at all. ABI v1 gives recipes no clock, randomness, network or filesystem.
  - Require the exports in `onyx-recipe-sdk` (`onyx_alloc`, `onyx_run`, `onyx_abi_version`).
- Golden tests: fixed recipe + fixture + label set -> checked-in output hash, rerun under several
  thread counts and after a clean rebuild.

## Amendment, M0 loop 2 (THREATS R-DET-4)
- The full runtime profile (ABI version, host build id, fuel, memory, output and call-depth limits)
  is pinned in every commitment (ADR-0011). Recompute in a dispute uses exactly that profile.
- Call depth is limited by deterministic instrumentation (a stack-limiter transform that counts
  frames), never by native stack size, which differs between machines and codegen.
- Failure is a result: a trap, out-of-fuel or depth overflow produces a canonical failure output
  (fixed failure code row), so every honest host commits the same root for a failing run.
- The float scan covers scalar and SIMD float ops (`f32x4.*`, `f64x2.*`), conversions,
  `trunc_sat`, reinterpret, and float value types anywhere in the module.
- Golden tests reject recipes whose fuel use on the reference fixture exceeds half the budget, so
  fuel-count drift between wasmtime versions cannot flip a pass into a trap.
- Output batches must be strictly ascending by primary key with no duplicates; the host checks
  this before hashing (R-DUPKEY-1).

## Alternatives
- Wasmer: similar features; wasmtime has the clearer fuel and NaN-canonicalization documentation.
- RISC-V zkVM (e.g. SP1, RISC Zero): would add proofs later, but heavier for M1. Recipes written
  against our SDK can be recompiled for a zkVM later.

## Consequences
- Recipe authors cannot use floats even internally; the SDK supplies fixed-point helpers.
- Rejecting all imports means recipes cannot log; debug output is returned in-band in dev builds only.
- A wasmtime upgrade could change fuel counts; fuel is a limit, not part of the output,
  so outputs still compare equal. Fuel used is reported but never committed.
