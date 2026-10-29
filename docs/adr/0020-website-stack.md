# ADR-0020: Website stack - Rust for everything that is checked, Svelte for the screen

## Status
Accepted, 2026-10-09. Implemented in `crates/onyx-wasm` and `apps/web`.

## Context
Onyx needs one public site: what the network is, a live explorer of rounds, labels and data, and
later (M6, M7) a wallet on Base Sepolia and the paid query API. The site's key promise is "do not
trust us, check it": the browser must recompute roots, proofs, signatures and recipe runs. The
question asked: should the site be pure Rust? Research and versions: RESEARCH.md "Website stack".

## Decision
1. Trust-critical code is Rust, compiled to WebAssembly with wasm-bindgen (`crates/onyx-wasm`). It
   calls the same crates the network runs: `onyx-recipe-host` (canonical wasmi engine, same fuel),
   `onyx-merkle`, `onyx-labels` (EIP-191 recovery, scoring). The browser never reimplements a check
   in JavaScript. `onyx-recipe-host` gets a default `fast` feature so the wasm build leaves out
   wasmtime (it cannot target wasm32).
2. The screen is SvelteKit 3 (Svelte 5, TypeScript) with `adapter-static`: every page is
   prerendered to plain files, so any static host or a claude.ai artifact can serve it.
3. The server, when M7 needs one, is Rust (axum) in `onyx-api`, serving the same JSON the static
   export writes today.
4. Wallet and contract calls (M6) use viem, the library Base documents. Not added until M6.
5. Not pure Rust: Leptos and Dioxus are pre-1.0, their component and wallet ecosystems are small,
   and the UI layer holds no trust. Rust where it is checked; the most-used tool where it is shown.

6. Until M6 anchors roots on Base Sepolia, the reference values the page checks against (recipe id,
   input hash, roots, live tip) are pinned in `onyx_web_verify::pinned`, tested against the golden
   fixtures, never read from the data files next to the page (website review W1, W2).

## Consequences
- One verifier codebase: a bug fix in a crate fixes the site at the next build.
- Two toolchains (cargo and pnpm). `scripts/build-web.sh` runs both.
- The wasm bundle carries the wasmi interpreter; size is reported in STATUS.md, not guessed here.
- The UI can be swapped later (for example to Leptos 1.0) without touching the checks.
