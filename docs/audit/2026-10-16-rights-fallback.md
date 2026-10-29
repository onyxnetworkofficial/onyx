# Rights fallback: what disappears (ROADMAP L14, 2026-10-16)

What a rights-fallback public build (`ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh`)
and a fallback node (`ONYX_RIGHTS=fallback` for onyx-node) leave out, compared with today's
default public build and node. Decision and switch: [ADR-0035](../adr/0035-rights-fallback.md).
Why: THREATS L8-5, launch gate LC-1 (path B). Default stays `cleared`; nothing changed in the
private preview.

Rule: anything computed from the entity label sets (OKX, Binance, BitMEX, Bitwise BITB) goes;
anything whose rights are clear stays (Bitcoin block data, mining-pool shares from the
MIT-licensed bitcoin-data/mining-pools list, U.S. Treasury Fiscal Data, SEC EDGAR, roots and
commitments, the label set's own hash and count).

## Website: pages and sections

| Page | Section | Default public build | Fallback build |
|---|---|---|---|
| Every page | Build notice | none | Banner: "Company totals are held back until data rights are cleared." |
| Overview | Proof card 2 (W5 home reserves card) | OKX 146,358.26346743 and BITB 37,290.75352194 BTC matched their published figures | Card says the totals are held back and why; links the Reserves page |
| Overview | Stats row, 4th figure | OKX net flow over the live range (BTC) | Number of in-browser checks |
| Overview | In-browser checks | 7: label set / run rows root / live headers / live rows root / attestation / row proof / BIP-322 | 5: label set pinned / live headers / **pool shares commitment** (new) / attestation / BIP-322 |
| Overview | "Markets that settle on data" (OKX net-flow market) | shown | gone |
| Overview | "Live from Bitcoin" | transactions per block + OKX in/out per block | transactions per block only |
| Data | "Company flows, per block" (rows, chart, add-up check, tamper demo) | shown | one held-back section |
| Data | "Flows over time" (series, check, tamper demo) | shown | gone (same held-back section) |
| Data | "One week" per-company history (7-day and 1-day charts, all-exchanges line, check, tamper demo) | shown | gone (same held-back section) |
| Data | Bitcoin block by block, pools, Treasury, SEC, data map | shown | **kept** |
| Reserves | Balance over the week, where the base comes from, API table, published-figure table | shown | one held-back section ("Balance over the week") |
| Run | Base Sepolia run summary line | "one OKX reserve row, 144931.85016221 BTC" | "one OKX reserve row, its value held back until data rights are cleared" |
| Run | Step lines with an amount in BTC | amount shown | "[company total held back]" |
| Run | Local rehearsal fact "OKX net over the range" | −107.99382189 BTC | "Held back", with the reason line |
| Explorer | Row proof (pick a company row, check, change 1 sat) | shown | gone; the input panel says the rows are held back and why |
| Explorer | Live blocks: company flow bars | shown | headers only, with the reason |
| API | Sample response (a company row and its proof) and its check button | shown | gone; a note says a fallback node answers 403 `label_rights_withheld` |
| Labels | Pinned label set (hash, count), attestation demo, BIP-322 vectors | shown | **kept** (no total) |

## Website: shipped files

| File | Default public build | Fallback build |
|---|---|---|
| `history/week/index.json`, `history/week/<day>.json` (7) | shipped | not shipped |
| `history/reserves-week.json` | shipped | not shipped |
| `data/demo.json` | run rows + proofs, live rows, per-block rows, series rows, spread, recount lines | rows and proofs empty; per-block, series, spread gone; recount line "held back"; `rights: "fallback"` |
| `run/live-run.json` | `round.okx` (net, in, out, tx count) | `round.okx: null` |
| `run/base-sepolia-*.json` | `round.okx_net_btc`, `reserve.okx_btc`, amounts in step lines | `null`, step amounts replaced |
| `run/heads.json` (W4, the home page's run card) | verdict, network, date, transaction count, reserve round | same (no figure in it); built after the run records are scrubbed |
| JS bundle (`src/lib/reserves.ts`) | OKX and BITB figures, OKX signed-subset figure | folded out (`FALLBACK ? [] : …`) |
| `history/pools-week.json`, `level2/*`, `run/indexer-*`, fonts, verifier wasm | shipped | **kept** |

Guard: `public_build::a_fallback_build_carries_no_label_derived_totals` refuses the build if any
of the above comes back, by path, by value (satoshi and BTC amounts, row leaves, in any encoding)
or by JSON field. Self-test: `public_build::the_fallback_guard_refuses_label_derived_totals`.

## Node endpoints (`ONYX_RIGHTS=fallback`)

A shard is label-derived (`ShardMeta::label_derived`) when its binding pins a label set, when
`KNOWN_GATES` claims it (the reserve gate's schema, recipe ids or metric), or when a segment of
its metric name starts with `entity`. Every such request is refused in `admit()` with **403
`label_rights_withheld`**, before any root read, row read or 402: nothing is charged.

| Endpoint | Label-derived shard (flows, balances, series, reserves) | Other shards (pools) and level 2 |
|---|---|---|
| `GET /v1/metrics/{metric}` | 403 `label_rights_withheld` (every candidate) | served as before |
| `GET /v1/proofs/{round}/{shard}/{i}` | 403 `label_rights_withheld` | served as before |
| `GET /v1/export/{round}/{shard}` | 403 `label_rights_withheld` | served as before |
| `GET /v1/settlement/{market}` | 403 `label_rights_withheld` (a market on such a shard) | served as before |
| `GET /v1/observations/{metric}` (+ `/attempts`) | not a shard: unaffected | Treasury, SEC served as before |
| `GET /v1/health` | adds `"rights": "fallback"` | same |
| OKX reserve sale (`btc.entity-reserve`, `subject=entity:okx`) | 403 `label_rights_withheld` though the serving check clears it; was 402 then 200 | n/a |

Tests: `onyx-api tests::fallback_rights_withhold_label_derived_shards_before_any_payment` (four
routes, both modes, no PAYMENT-REQUIRED header, zero row reads),
`tests::label_derived_covers_label_sets_known_gates_and_entity_metrics` (incl. a reserve schema
under an innocent metric name), `onyx-node config::tests::rights_default_to_cleared_and_take_only_cleared_or_fallback`,
`onyx-reserve-serve api_reserve::fallback_rights_sell_no_reserve_row` (gate clears 89 shards;
OKX metric, proof and export, and the forged shard all 403 in fallback; cleared still 402).

### Node run (2026-10-16, release build of this branch)

A real `onyx-node` on a local anvil chain, from `scripts/demo-local.sh --serve --export-node`
(chain 31337, indexer on, payments off, so a cleared node answers 200 rather than 402; the
402-before-403 order is covered by the tests above). The export holds one `btc.entity-flow`
shard (round 1, `0xacbc…4244`, OKX market `okx-net-flow-demo`); the same export and a fresh
state dir per run, waited until the indexer admitted the shard.

| Request | `ONYX_RIGHTS` unset | `ONYX_RIGHTS=fallback` |
|---|---|---|
| `GET /v1/health` `rights` | `"cleared"` | `"fallback"` |
| `GET /v1/metrics/btc.entity-flow?subject=entity:okx` | 200, row + proof | 403 `label_rights_withheld` |
| `GET /v1/export/1/<shard>` | 200, rows | 403 `label_rights_withheld` |
| `GET /v1/proofs/1/<shard>/0` | 200, row + proof | 403 `label_rights_withheld` |
| `GET /v1/settlement/okx-net-flow-demo` | 409 `proof_level_not_accepted` (the demo market's level rule) | 403 `label_rights_withheld` |
| Start-up log | no rights line | "ONYX_RIGHTS=fallback: label-derived metrics ... refused with 403 label_rights_withheld; 0 catalog shard(s) withheld now, indexed ones per request" |

`ONYX_RIGHTS=Fallback` refused at start: `ONYX_RIGHTS must be cleared or fallback, not "Fallback"`.
Both runs stopped cleanly (exit 0). Not run against a node: the reserve shard (onyx-node-btc
with its serving check; covered by `api_reserve::fallback_rights_sell_no_reserve_row`) and pool
shards (the demo export holds none; covered by the onyx-api test).

### Website switch off and on (same commit)

| Build | Guard | e2e | `146,358.26346743` / `37,290.75352194` / `140,974.44061385` in `build/` | `history/` | `demo.json` run rows |
|---|---|---|---|---|---|
| `ONYX_PUBLIC=1` (rights `cleared`) | public guards pass | `E2E PASSED` | 1 file each | `pools-week.json`, `reserves-week.json`, `week/` | 4 |
| `ONYX_RIGHTS=fallback ONYX_PUBLIC=1` | public guards and `a_fallback_build_carries_no_label_derived_totals` pass | `E2E PASSED` | 0 | `pools-week.json` only | 0, `rights: "fallback"` |

In the fallback build `okx_btc` and `net_sats` remain only as field names in the JS and as
`"okx_btc": null` in `run/base-sepolia-2026-10-14.json`; the run records' Base Sepolia reserve
figure (`144931.85016221`) and the local OKX net (`107.99382189`) are absent.

## Screenshots

Full pages of the fallback public build (retaken 2026-10-16 after main was merged and the two wording fixes below), taken with `SHOTS=fallback node tests/style-audit.mjs
build ../../docs/audit/rights-fallback` (reduced motion, after the checks settle), at 1280 px
(`*-desktop.jpg`) and 390 px (`*-390.jpg`), in [rights-fallback/](rights-fallback/). The same run
measured the tokens: 0 colours off the token set (`rights-fallback/styles.json`).

| Page | 1280 px | 390 px | Looked at |
|---|---|---|---|
| Overview | [home-fallback-desktop.jpg](rights-fallback/home-fallback-desktop.jpg) | [home-fallback-390.jpg](rights-fallback/home-fallback-390.jpg) | banner; card 2 "Company totals are held back until data rights are cleared." with the reason; 4th stat "5 checks run in your browser"; 5 checks; no market; live section has the transactions chart only. Card 3 said its checks cover "rows, proofs and roots", untrue with no rows: now "hashes, block headers and signatures" in this build (cropped at 390 px and read) |
| Data | [data-fallback-desktop.jpg](rights-fallback/data-fallback-desktop.jpg) | [data-fallback-390.jpg](rights-fallback/data-fallback-390.jpg) | blocks section, then one "Company flows" held-back panel, then pools, Treasury, SEC, data map |
| Reserves | [reserves-fallback-desktop.jpg](rights-fallback/reserves-fallback-desktop.jpg) | [reserves-fallback-390.jpg](rights-fallback/reserves-fallback-390.jpg) | one held-back panel under "Balance over the week"; no figure |
| Run | [run-fallback-desktop.jpg](rights-fallback/run-fallback-desktop.jpg) | [run-fallback-390.jpg](rights-fallback/run-fallback-390.jpg) | layout intact, all sections present; cropped and read at 1280 px: "one OKX reserve row, its value held back until data rights are cleared, at block 971,199", local rehearsal fact "OKX net over the range: Held back" with the reason line; step amounts checked by the e2e (`[company total held back] = the scan`) |
| Explorer | [explorer-fallback-desktop.jpg](rights-fallback/explorer-fallback-desktop.jpg) | [explorer-fallback-390.jpg](rights-fallback/explorer-fallback-390.jpg) | input panel names the reason; no row proof; live blocks headers only |
| Labels | [labels-fallback-desktop.jpg](rights-fallback/labels-fallback-desktop.jpg) | [labels-fallback-390.jpg](rights-fallback/labels-fallback-390.jpg) | banner; label set name, count (9,850) and hash, attestation and BIP-322 demos; no total. Its note said "flows over it are checked against pinned roots", untrue in this build: now "company totals computed from it are held back until data rights are cleared." |
| API | [api-fallback-desktop.jpg](rights-fallback/api-fallback-desktop.jpg) | [api-fallback-390.jpg](rights-fallback/api-fallback-390.jpg) | no sample row; the scope list ends with the fallback note (403 `label_rights_withheld`); at 1280 px the right column, where the sample was, stays empty (left as is) |

After merging `825efd7` the shots were retaken: the home page's live-block and label counts show "–" in the full-page image because W4 loads the verifier only when they scroll into view (the e2e checks they fill); the rest as described.

Seen and left as is: the data map and the proof ladder still name "Bitcoin company flows" as a
kind of data Onyx runs (a description, no total); the Reserves page is short at 390 px (one
panel). Not checked: how a search engine or a link preview shows the pages (no public URL).

## Checks run

First pass on branch `rights-fallback` with main `cfb4bbd` merged (commit `0851e96`); then main
`825efd7` (W4 site speed, L13 history scan) merged and the web checks rerun (rows marked
"after 825efd7"), 2026-10-16. Heavy jobs under
`systemd-run --user --scope -p MemoryMax=12G`.

| Check | Result |
|---|---|
| `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` (final run, after the two wording fixes) | exit 0: public guards, `a_fallback_build_carries_no_label_derived_totals`, `E2E PASSED` |
| `ONYX_PUBLIC=1 scripts/build-web.sh` (switch off) | exit 0, `E2E PASSED`; totals present (table above) |
| e2e flake | one earlier fallback build failed `labels: attestation valid` (the signature check did not answer within the wait) while another worktree's checks ran on the same machine; the same build's e2e then passed twice in a row and the final full build passed. Not reproduced; recorded here, not fixed |
| onyx-node both ways on a local chain | table under "Node run" |
| `scripts/check-all.sh` | exit 0, `ALL CHECKS PASSED` (1,104 Rust tests, 0 failed) |
| `scripts/check-secrets.sh` | exit 0, 730 tracked files scanned |
| After 825efd7: `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` | exit 0, guards and `E2E PASSED` (W4 checks included); the five figures above found in 0 files; `history/` holds `pools-week.json` only |
| After 825efd7: `ONYX_PUBLIC=1 scripts/build-web.sh` | exit 0, `E2E PASSED`; OKX, BITB and OKX-subset figures in 1 file each; `history/` has `week/` and `reserves-week.json` |
| After 825efd7: `ONYX_WEB=1 scripts/check-all.sh` (adds the preview build and its e2e) | exit 0, `ALL CHECKS PASSED` (1,105 Rust tests, 0 failed; preview `E2E PASSED`) |
| After 825efd7: `scripts/check-secrets.sh` | exit 0 during the merge (745 index entries, conflict stages included) and again after the merge commit (735 tracked files) |
| onyx-node after 825efd7 | not rerun: the merge changed no node or API code (only a 3-line allowlist in `public_build.rs`) |

Merging W4 (`825efd7`): the home page now warms the verifier only when a figure is on screen
(`warmUp`); the fallback guards moved there (no live rows, no per-block series). Its browser test
`tests/w4.mjs` assumed a cleared build; it now reads `rights: "fallback"` and expects 5 checks, no
API sample (and nothing heavy on the API page), no history files on Data, nothing heavy on
Reserves, no market or flow-chart slot on the home page, and 9 fewer data files. The run-record
scrub runs before W4's `run/heads.json` and minifying steps.

Fixed in this pass: the home page's check card and the Labels page note described row checks the
fallback build no longer runs (both now say what the build checks or holds back).

Not done in that pass: an adversarial review by a fresh subagent (done below, L14-R1); the
fallback build's e2e is not in check-all (same as the public build, ROADMAP E10).

## Review L14-R1 (fresh reviewer, 2026-10-16)

Main's W12 round 1 (`0ccc90f`) merged first (one conflict on the Run page: the held-back text
kept, main's `fine` paragraph class taken). Findings and fixes are THREATS L14-R1-1..5.

Leak hunt over the whole fallback build (every file: JSON, JS chunks, both wasm files, og image,
`history/`, `run/`, `level2/`):

| Search | Before this review | After |
|---|---|---|
| Any number with a BTC, sat or ₿ unit | `50BTC` (pool name), `10 BTC` (dead chart button), `1 sat` (text) | `50BTC` only |
| A figure within 100 bytes of OKX, Binance (not Binance Pool), BitMEX, Bitwise, BITB | `99.8%` "of OKX's labelled value" in the Reserves chunk (dead branch) | none (rule 4 now refuses it) |
| Per-entity counts and the cross-check text (`3,558`, `labelled value`, "Onyx's OKX figure") | in the Reserves chunk | 0 files |
| The committed totals as LE/BE 64-bit integers in the verifier and recipe wasm (1,715 values) | not checked | 0 |
| `og.jpg` | looked at: hero, no figure | same |

Cause of the dead text: Svelte hoists each block's HTML into module-level `$.from_html()` calls;
the bundler removed the `{:else}` code behind `FALLBACK` but kept the calls. `vite.config.ts` now
marks them pure; run on the bundle built before the fix, the guard (rule 4) refuses it with
`figure next to a company name (99.8%)`. The guard was not a 5-value denylist (it compares every
satoshi amount, 8-decimal BTC amount and row leaf of the committed outputs: over 100 amounts and
2,000 leaves), but it could only find values it already knew; rule 4 is structural.

Node: tried HEAD, a valid payment signed on a cleared server sent first, junk payment, a
contributor proof, `?root=`, an out-of-range index, unknown subjects, `as_of`/`min_as_of` and the
settlement market against a fallback server: all 403 `label_rights_withheld`, no row read, nothing
settled (`tests::the_fallback_403_cannot_be_skipped`). `/v1/health` carries no total.

Default: decided no default where a deploy can be public (ADR-0035 Amendment 1, TRACKER). The
real binary, `--check-config` with `ONYX_ENV=testnet`: unset or empty `ONYX_RIGHTS` exits 2 with
"ONYX_RIGHTS is required with ONYX_ENV=testnet"; `fallback` and `cleared` pass that check; `dev`
unset passes. `ONYX_PUBLIC=1 scripts/build-web.sh` without it exits 2.

| Check (this review) | Result |
|---|---|
| `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` | first run after the merge failed in e2e (`w12.mjs`, L14-R1-4); after the fixes, rule 4 refused `forged_extra_btc` (L14-R1-5); final run exit 0, all guards, `E2E PASSED` |
| `ONYX_RIGHTS=cleared ONYX_PUBLIC=1 scripts/build-web.sh` | exit 0, `E2E PASSED`; full Reserves page drawn (screenshot looked at) |
| `ONYX_WEB=1 scripts/check-all.sh` | exit 0, `ALL CHECKS PASSED` (1,106 Rust tests; preview build `E2E PASSED`) |
| `ONYX_PUBLIC=0 scripts/build-web.sh` (last build) | run 1 one a11y focus-ring FAIL on Labels at load average 44 (the same tree passed in check-all just before); rerun exit 0, `E2E PASSED` |
| `scripts/check-docs.sh`, `scripts/check-secrets.sh` | exit 0 each (752 tracked files scanned) |
| `scripts/docker-smoke.sh` | not run (its export now says `ONYX_RIGHTS=fallback`; the smoke switches to `cleared`) |
| Screenshots, fallback build, 1280 and 390 px (`tests/shots.mjs`: home, data, reserves, run) | looked at: banner on every page, the held-back panel says what is left out and why, Run says "its value held back until data rights are cleared"; not committed |

