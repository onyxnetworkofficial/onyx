# Onyx website

SvelteKit pages over a Rust verifier (`verify/`, compiled to WebAssembly). Every check runs the
network's own code in the browser; reference values are pinned in `verify/src/lib.rs` until M6
(ADR-0020).

1. Export the demo data: `cargo run --release -p onyx-source-btc --example export-demo`
   (with `ONYX_LIVE=dir:from:to` for the live range).
   The one-week history (`static/history/week/`: `index.json` plus one leaf file per UTC day,
   committed) comes from `scripts/fetch-btc-node.sh indexer <from> <to> fixtures-live/week`, then
   `ONYX_HISTORY=fixtures-live/week:<from>:<to> cargo run --release -p onyx-source-btc --example export-history -- apps/web/static/history/week`;
   it prints the commitment to pin in `verify/src/lib.rs` (`pinned::HISTORY_*`) and in
   `adapters/source-btc/tests/golden_history.rs`. The page loads the index first and each day's
   leaves after it, and draws a day only once its leaves check out.
2. Build and test: `scripts/build-web.sh` (needs clang, `wasm-bindgen-cli` 0.2.129, pnpm, Chrome). Or `ONYX_WEB=1 scripts/check-all.sh` to run it after the repo gate (`ONYX_WEB_PUBLIC=1`: the public build with `ONYX_RIGHTS=cleared`, then `fallback`, each with its guards and e2e, then the preview). CI does not run it: the e2e checks need the live blocks from our node (AUD-1-F6). The browser tests never sleep a fixed time before a check: they wait for the state the check reads, with the caps in `tests/wait.mjs` (`settled`, `routeDrawn`, `WAIT_CAP`; every page gets them through `capWaits`, ROADMAP E12).
3. Develop: `cd apps/web && pnpm dev`. Output: `apps/web/build` (static files, hash routes).
   Loading (W4): the verifier WASM, the recipe input and the history files load only on pages
   that show their results; Home starts the verifier when a checked figure is on screen or the
   checks run, API when its button is pressed. The build writes `run/heads.json` (the run files'
   fields the home page reads) and strips indentation from the shipped JSON (values unchanged).
   `tests/w4.mjs` checks all of it.
4. Reserves (`static/history/reserves-week.json`, ADR-0034): `cargo run --release -p onyx-source-btc
   --example export-reserves -- export fixtures-live/week fixtures-live/tail
   apps/web/static/history/week/index.json fixtures/reserves/scan-971199.json
   fixtures/reserves/base-969838.json apps/web/static/history/reserves-week.json` (the tail is
   blocks 970847-971199 from `scripts/fetch-btc-node.sh`); pin the printed `RESERVES_*` in
   `verify/src/lib.rs` and `adapters/source-btc/tests/golden_reserve.rs`.
5. Add data (`#/submit`, ROADMAP S1, ADR-0038): the onboarding questions and checks run in the
   browser (`src/lib/intake.ts`, pinned to `fixtures/intake/vectors.json` with the Rust crate
   `onyx-intake`). Every build from `scripts/build-web.sh` is offline for submissions: steps 3 and 4
   say the API is not connected and offer the JSON file. To try the API path locally:
   `cargo run -p onyx-api --features http-client --example submit-dev` (loopback, port 8788), then
   `cd apps/web && VITE_ONYX_SUBMIT_API=1 ONYX_SUBMIT_PROXY=http://127.0.0.1:8788 pnpm dev`.
   A typed API key (the keyed-source step) never leaves the page. `tests/s1.mjs` checks it all.
