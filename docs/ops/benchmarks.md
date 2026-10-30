# Benchmarks

Measured 2026-10-08 on an AMD Ryzen 5 5600X (6 cores / 12 threads, 60 GiB RAM), Linux, rustc 1.98.0,
release builds, wasmi 2.0.0, wasmtime 49.0.2 (numbers measured on 49.0.2; the pin is 49.0.5 since 2026-10-30). Numbers are wall-clock and vary by machine; rerun with
the commands below before relying on them. Nothing here is a claim about other projects.

## Reference recipe on real blocks

`cargo run --release -p onyx-source-btc --example bench-golden`

Input: blocks 921080-921082 (11,615 txs, 1,805 KiB recipe input), label set v1 (120 labels).

| Engine | First run s (incl. load) | Warm run s (median of 5) | Fuel used | Root |
|---|---|---|---|---|
| wasmi (canonical) | 0.137 | 0.125 | 136,914,259 | 0xf912872a...86b2 |
| wasmtime opt none (advisory) | 0.038 | 0.016 | 126,559,637 | 0xf912872a...86b2 |

Fuel models differ between engines; only wasmi fuel is canonical. Adapter load and verification: 0.11 s.
Fuel used is 3.4% of the 4,000,000,000 budget used in the golden test (limit: 50%).

## Series recipe `btc-entity-flow-series` (2026-10-10)

`ONYX_LIVE_DIR=fixtures-live/node cargo test --release -p onyx-source-btc --test golden_series -- --nocapture --test-threads 1`
(same machine as above). Label set v2 (9,850 labels). Fuel is wasmi fuel (canonical); wall time is
the wasmi run alone, 3 runs.

| Input | Series rows | Series fuel | Range recipe fuel (same input) | wasmi s (3 runs) |
|---|---|---|---|---|
| Fixture blocks 921080-921082 | 6 | 738,953,128 | 749,838,200 | not timed |
| Live blocks 970543-970554 (12 blocks, one round shard) | n/a | 1,393,066,006 | not run | not timed |
| Live blocks 970543-970566 (24 blocks, display range) | 42 | 2,387,852,735 | 2,390,051,557 | 2.57 (1 run) |

Recipe `0xca406e0b…` (SER-R1-F1, `NO_ACTIVITY`); the earlier id `0x64ffb478…` measured 738,953,120
and 2,387,852,727 (2.37 / 2.47 / 2.36 s) on the same inputs. The series costs no more than the
range recipe: decoding 9,850 label addresses dominates both. On the 24 live blocks both use about
60% of the 4,000,000,000 budget, so the round preflight refuses that range as one shard; a
12-block shard passes at 35% (THREATS SER-1, SER-R1-F2).

Preflight probe (`tests/series_props.rs`): 24 fixture-density blocks with every input and output
doubled cost 3,382,451,237 fuel (85%) and are refused. Cost model from these runs: about 0.56 B
fuel for label decoding plus about 3.8 k fuel per input or output (docs/spec/recipes.md).

## History export: one day of shards (2026-10-10)
`ONYX_HISTORY=fixtures-live/day:970703:970846 cargo run --release -p onyx-source-btc --example export-history -- <out dir>`
(the output directory is required here: without it the exporter writes the committed week files
in `apps/web/static/history/week`; AUD-2-F12) (dev box, wasmi, label set v2). Blocks 970,703-970,846 (144 blocks, 10 Oct 01:20-23:43 UTC) fetched
from the node with `scripts/fetch-btc-node.sh`: 884 MB on disk, 2 min 41 s. Shards planned by
inputs plus outputs (`history::plan_shards`, target 1.8 B) and each confirmed by the preflight.

| Shard | Blocks | Inputs + outputs | Estimated fuel | Series fuel | Range fuel | Series wasmi |
|---|---|---|---|---|---|---|
| 970,703-970,718 | 16 | 295,723 | 1,728,105,850 | 1,716,312,466 | 1,720,422,523 | 1.75 s |
| 970,719-970,733 | 15 | 294,257 | 1,722,315,150 | 1,730,678,074 | 1,735,582,315 | 1.79 s |
| 970,734-970,750 | 17 | 311,059 | 1,788,683,050 | 1,756,774,484 | 1,760,621,723 | 1.86 s |
| 970,751-970,767 | 17 | 301,324 | 1,750,229,800 | 1,729,075,542 | 1,734,123,237 | 1.79 s |
| 970,768-970,783 | 16 | 306,180 | 1,769,411,000 | 1,751,001,506 | 1,755,067,939 | 2.50 s |
| 970,784-970,799 | 16 | 307,666 | 1,775,280,700 | 1,751,989,913 | 1,756,627,281 | 2.57 s |
| 970,800-970,814 | 15 | 302,721 | 1,755,747,950 | 1,744,929,397 | 1,750,397,936 | 2.60 s |
| 970,815-970,829 | 15 | 296,541 | 1,731,336,950 | 1,733,995,774 | 1,739,548,494 | 2.31 s |
| 970,830-970,846 | 17 | 286,710 | 1,692,504,500 | 1,678,160,793 | 1,683,905,213 | 1.95 s |

Planned with `FUEL_PER_IO` = 3,950 (HIST-R1-F6; 3,800 read up to 3.2% low, so 4 of 9 shards went
over the 1.8 B target). Every shard is at or under 44.0% of the 4 B limit (largest: range fuel
1,760,621,723; the preflight allows 50%); the estimate is now within 1.8% of the measured fuel.
Whole export (headers, 9 shards x 2 recipes with
preflight, Python recount of every shard): 2 min 2 s. Output then (the exporter wrote one `day.json`; it now writes `index.json` plus one leaf file per
day into the output directory):
269 series rows, 130,087 bytes (28,641 with gzip -9). Rerun check
`ONYX_HISTORY_DIR=fixtures-live/day cargo test --release -p onyx-source-btc --test golden_history`:
67 s.

## History export: one week of shards (2026-10-11)

Blocks 969,839-970,846 (1,008 blocks, ending at the one-day history's tip). The 864 blocks before
the day were fetched read-only from our node with `scripts/fetch-btc-node.sh indexer 969839 970702
fixtures-live/week` in 14 min 32 s (sequential, 5.0 GB; the day's 144 files are linked, 5.9 GB in
all). Export: `ONYX_HISTORY=fixtures-live/week:969839:970846 cargo run --release -p onyx-source-btc
--example export-history -- apps/web/static/history/week`, 12 min 53 s wall clock on this machine
(headers, 66 shards x 2 recipes with preflight, Python recount of every shard; the series runs
alone: 112.0 s).

Shards never cross a UTC day (`history::plan_days`), then pack by inputs + outputs as before.

| Day (UTC, by header time) | Heights | Blocks | Shards | Series rows | Six-block buckets |
|---|---|---|---|---|---|
| 2026-10-04 (from 11:59) | 969,839-969,923 | 85 | 6 | 157 | 15 |
| 2026-10-05 | 969,924-970,099 | 176 | 12 | 311 | 30 |
| 2026-10-06 | 970,100-970,255 | 156 | 10 | 289 | 26 |
| 2026-10-07 | 970,256-970,409 | 154 | 10 | 293 | 26 |
| 2026-10-08 | 970,410-970,548 | 139 | 9 | 278 | 24 |
| 2026-10-09 | 970,549-970,694 | 146 | 9 | 277 | 25 |
| 2026-10-10 | 970,695-970,846 | 152 | 10 | 283 | 26 |

Shards: 1-18 blocks each; series fuel 650,286,814-1,804,795,767; largest fuel (range recipe)
1,809,603,141 = 45.2% of the 4 B limit (the preflight allows 50%). The estimate (`FUEL_PER_IO` =
3,950) was at most 3.3% off the measured fuel; 3 of 66 shards ran slightly over the 1.8 B planning
target. Series run per shard on wasmi: 0.81-2.09 s.

Committed site files (`apps/web/static/history/week/`): `index.json` 237,160 bytes (1,008 headers,
66 shard ranges and roots, 7 days with their buckets) plus one leaf file per day, 40,666-80,637
bytes; 719,914 bytes in all, 157,645 with gzip -9. The page loads the index, then each day's
leaves after the page is interactive. Lighthouse on `#/data` (mobile): perf 94, CLS 0, LCP 3.1 s,
2,138 KiB in all.

Rerun check `ONYX_HISTORY_DIR=fixtures-live/week cargo test --release -p onyx-source-btc --test
golden_history`: all 66 shards of both recipes rerun to the exported leaves, with the day and
shard plans and every header checked, in 385 s (while the website built in parallel).

## History export: one week with a candidate label set v3 (2026-10-11, ADR-0031; not shipped)

Measured on a candidate v3 that added the wBTC and cbBTC reserve lists; both were then dropped on
rights (IDX-R1-F1/F2), so the site keeps v2 and these numbers record only how fuel grows with a
larger label set. Same 1,008 blocks (969,839-970,846) and the same blocks on disk; candidate set
10,302 labels, 6 companies (v2: 9,850). Export: `ONYX_LABEL_SET=v3 ONYX_HISTORY=<fixtures-live/week>:969839:970846
cargo run --release -p onyx-source-btc --example export-history -- apps/web/static/history/week`,
14 min 52 s wall clock (810.0 s in the exporter; the series runs alone: 134.6 s). Every one of
the 66 shards passed the preflight and the Python recount of every row (1,936 rows).

| Day (UTC, by header time) | Heights | Blocks | Shards | Series rows | Six-block buckets |
|---|---|---|---|---|---|
| 2026-10-04 (from 11:59) | 969,839-969,923 | 85 | 6 | 158 | 15 |
| 2026-10-05 | 969,924-970,099 | 176 | 12 | 322 | 30 |
| 2026-10-06 | 970,100-970,255 | 156 | 10 | 297 | 26 |
| 2026-10-07 | 970,256-970,409 | 154 | 10 | 302 | 26 |
| 2026-10-08 | 970,410-970,548 | 139 | 9 | 291 | 24 |
| 2026-10-09 | 970,549-970,694 | 146 | 9 | 279 | 25 |
| 2026-10-10 | 970,695-970,846 | 152 | 10 | 287 | 26 |

Series fuel per shard 679,391,088-1,840,139,167; largest fuel (range recipe) 1,848,217,014 =
46.2% of the 4 B limit (the preflight allows 50%). The planner's estimate (`FUEL_PER_IO` = 3,950,
calibrated on v2) read up to 6.1% low with v3's larger label set; the preflight, not the
estimate, is the authority (IDX-7). Series run per shard on wasmi: 0.70-2.94 s.

Committed site files: 769,654 bytes in all (index 241,696), 161,329 with gzip -9. Live 24-block
display range with v3: series fuel 2,433,928,779, range fuel 2,439,166,329 (61%, a display rerun,
not a round shard, as with v2). Lighthouse (mobile) after the v3 switch: home 90, data 94, run
95, explorer 96, labels 96, api 96; CLS 0 on every page.

## Pool blocks `btc-pool-blocks` (2026-10-11, ADR-0032)

Wasmi, fuel limit 4 B. The cost is the table decode (127 reviewed pools since review POOL-R2; 128 after POOL-R1; 135 before) plus a scan of each coinbase for the earliest tag;
it does not depend on a label set.

| Run | Blocks | Fuel | % of limit |
|---|---|---|---|
| Golden, committed proofs 970,019-970,029 (before POOL-R2) | 11 | 2,872,968 | 0.07% |
| Week day 2026-10-04 (smallest; POOL-R2 earliest-tag rule) | 85 | 24,012,640 | 0.60% |
| Week day 2026-10-05 (largest; POOL-R2 earliest-tag rule) | 176 | 50,074,084 | 1.25% |

One shard per UTC day is far under the preflight's 50%. Week rerun from raw blocks
(`ONYX_WEEK_DIR=<fetched week> cargo test --release -p onyx-source-btc --test golden_pools
the_week`: load and check 1,008 blocks, 7 shards, Python recount of each day, commitment):
18.1 s wall clock on this machine (test binary already built).

## Hostile shapes (load + run)

`cargo run --release -p onyx-recipe-host --example bench-compile`

| Shape | wasm KiB | wasmi (canonical) s | wasmtime opt none s | wasmtime opt speed s |
|---|---|---|---|---|
| licm 8k values, recursion to depth 1000 | 54 | 0.08 Ok | 0.04 Ok | 1.41 Ok |
| licm 16k values | 109 | 0.17 Ok | 0.08 Ok | 6.24 Ok |
| licm 36.9k values | 252 | 0.38 Ok | 0.18 Ok | 29.70 **Trap (native stack overflow)** |
| straight-line 4k ops | 39 | 0.00 Ok | 0.00 Ok | 0.01 Ok |
| straight-line 16k ops | 156 | 0.01 Ok | 0.02 Ok | 0.02 Ok |
| br_table worst allowed by caps (4,094 targets x 16 results per function, many functions) | 992 | 0.23 Ok | 0.54 Ok | 0.38 Ok |
| typed (64-param) blocks at the per-function cap of 64, many functions | 911 | 0.12 Ok | 0.34 Ok | 0.35 Ok |

wasmi load time grows about linearly with size on the LICM shapes (0.08 / 0.17 / 0.38 s for 54 / 109 /
252 KiB; a later rerun gave 0.50 s for 252 KiB, so expect +-30% run to run). Before the br_table and
arity caps (loop 9), a 966 KiB module of br_tables with 65,520 targets x 1,000 results took 11.2 s to
load (fifth review, L8-COMPILE-3); it is now rejected at load. Worst case seen within all caps: under
1 s. This is not a proof of a bound for every shape; see THREATS R-HOST-COMPILE-2.

Typed blocks (blocks with parameters) before the loop-10 cap: one function with 10,240 of them made
the fast path's compiler use 3.3 GB (2,560: 223 MB; growth about quadratic), and the sixth review saw
more than 6 GiB with 80,000; wasmi used 27 MB for 64,000. Plain blocks are cheap on both (24 MB for
10,240). Measured with `cargo run --release -p onyx-recipe-host --example bench-nested -- fast 40 256 64`.
Whole `bench-compile` run peak RSS: 562 MB.

Loop-10 review (fast path only; canonical loads each in about 0.02 s): single-result blocks nested
255 deep and repeated 240 times in one function (184 KB) used 7.3 GB to compile; 2,000 locals with
8,000 `br_if` back-edges to one loop (52 KB) used 1.8 GB and 20 s; `br_table` back-edges grew faster.
Response: the fast path compiles only allowlisted (reviewed) recipe ids, and total blocks are capped
at 4,096 per function on both engines.

## Level-2 transcript log gas (2026-10-12, ADR-0027 amendment 8)

Measured in forge (`OnyxRegistryTranscriptLog.t.sol::test_GasOfTheLog`, `gasleft()` around one
call, warm registry, Foundry default EVM). Not a Base Sepolia measurement.

| Call | Gas |
|---|---|
| `postCommit` (rebuilds the signed text, ecrecover, 4 slots incl. the signature) | 182,480 |
| `postReceipt` (rebuilds the signed text, ecrecover, 1 slot) | 115,571 |
| `transcriptComplete`, 3 observers (view) | 25,316 |

`postCommit` was 139,255 before review REVLOG-R2-H1 stored the observer's signature in state (two
more slots), so the coordinator reads it with one `eth_call` instead of `eth_getLogs`.
`OnyxRegistry` runtime 13,604 B after REVLOG-R2 (13,072 B after REVLOG-R1-L2, 12,866 B before;
limit 24,576 B; `forge build --sizes`). `OnyxSettlement` 23,179 B (+42 B for the constant
`coordinatorEpoch`, REVLOG-R2-L3).

## Attestation check (M2 loop 13)

`cargo run --release -p onyx-source-btc --example bench-attest -- <N>`, AMD Ryzen 5 5600X,
2026-10-08. N labels at full confidence, each citing a real attestation (BIP-322 P2WPKH full proof +
EIP-191 entity signature). Time is `LabelSet::check_attestations` only.

| Labels | Address type | check_attestations s | per label |
|---|---|---|---|
| 2,000 | P2WPKH (loop 13) | 0.206 | 103 us |
| 20,000 | P2WPKH (loop 13) | 1.833 / 2.389 / 2.025 (3 runs) | 92-119 us |
| 2,000 | P2WPKH (loop 14) | 0.159 | 79 us |
| 20,000 | P2WPKH (loop 14) | 1.627 | 81 us |
| 1,000 | P2WSH 16-of-16, hostile (`-- 1000 ms16`) | 0.640 / 0.601 | 601-640 us |
| 1,000 | P2WSH 1-of-16 signed by key 16 (`-- 1000 ms1of16`, loop 15) | 0.636 / 0.626 | 626-636 us |
| 58,000 | same, under the old 64 MiB cap | 37.564 | 648 us |
| 13,000 / 13,500 | same, under the 16 MiB cap (14,700 is refused) | 7.444 / 8.839 | 573-655 us |
| 20,000 | P2WPKH, 16 MiB cap (loop 15) | 2.410 | 121 us |

The byte cap binds before the count cap. Loop-14 text said "about 40 s at the count cap"; that was
wrong (L14-F8): the 64 MiB byte cap was hit first, and the reviewer measured 33.5 s at 58,000; this
machine measured 37.6 s. With the 16 MiB cap, 13,500 took 8.8 s here; the loop-15 reviewer found
14,650 also fits (8.45 s on its run), so the true worst case is about 9.6 s at 655 us per label
(estimate). It is paid per shard (L15-F4). It needs a
registered entity key.

Before the loop-13 fix the reviewer measured 0.18 s at 250 labels, 3.0 s at 1,000 and 12.2 s at
2,000 (quadratic; L12-F5).

## Validator machine requirements

wasmi grows linear memory with `Vec` reservations, which can double; a run may hold up to about
2 x `MAX_MEMORY_BYTES` (2 GiB) plus the value stack (16 MiB). If the machine cannot allocate, the run
abstains (`HostError::Abstain`) instead of producing a verdict (L8-OOM-1). Recommended: at least
4 GiB free per concurrent run.

## Native frame size on the fast path

`cargo run --release -p onyx-recipe-host --example bench-frames` (8 MiB native stack for wasm)

| Shape | wasm slots | opt none: bytes/frame | opt speed: bytes/frame |
|---|---|---|---|
| empty frame | 1 | < 8,208 | < 8,208 |
| 2000 live operands | 2001 | < 8,208 | < 8,208 |
| 2000 locals | 2001 | ~16,070 (~8 B/slot) | < 8,208 |
| licm 30k (7 slots) | 7 | < 8,208 | ~246,723 (~35 KB/slot) |

The fast path budgets 64 B/slot. Measured worst with the optimizer off is ~8 B/slot (x86_64 only).
With the optimizer on, the LICM shape breaks any per-slot budget: hence ADR-0014.

## Ethereum chain statistics window (2026-10-19, ADR-0044)

Measured on the dev box under load average 20-60 (other agents' jobs). Fetch:
`scripts/refresh-eth-onchain.py --rpc https://ethereum-rpc.publicnode.com --check-rpc
https://rpc.flashbots.net --count 300 --fetch-only` (4 requests a second). Export:
`export-eth-chain-stats` (release, wasmi, recipe `0x84773a98…` after review P4A-R2), one shard per block, each
preflighted and run, then `scripts/recount-eth-chain-stats.py`. Wall times and peak memory from
`/usr/bin/time -v`.

| Measure | Value |
|---|---|
| Window | blocks 26,225,261-26,225,560 (300, 2026-10-19 07:54-08:54 UTC, finalized at fetch) |
| Fetch, 300 blocks with receipts (613 requests) | 190 s wall, 46 MB peak (Python) |
| Answers on disk | 405 MB raw JSON, 53 MB gzip (about 1.35 MB raw a block) |
| Export, 300 blocks (load and check, preflight, run, recount) | 101-137 s wall over three runs (100.6 s before the recount on the last), 32 MB peak |
| Fuel per block shard | mean 48,647,979, max 96,632,769 (2.42% of 4 B; P4A-R2 re-export) |
| Recipe `eth_chain_stats.wasm` | 57,737 bytes |
| Python recount | all 19 fields of all 300 blocks match |
| `eth-chain-stats.json` (one leaf per block) | 144,608 bytes |
| Rerun input shipped (block 26,225,368, 102 txs) | 72,446 source bytes, 96,681 bytes base64, 10,864,779 fuel; 0.6 s in headless Chrome on this machine |

Not measured: a full Ethereum node (none runs here), a day-sized window, the browser rerun on a
phone (the page prints its time), and a worst-case block (THREATS ETH-11).

## Test robustness under load (E9/E10, 2026-10-16)

The browser suite and the onyx-api timing tests, rerun on the shared dev box after the waits were
changed to wait on page state (`apps/web/tests/wait.mjs`) and the two timing tests moved to a
paused tokio clock. Load is the 1-minute load average at the start and end of each run.
Logs: the session scratchpad `e9e10/` (not committed).

| Check | Runs passed | Load (1 min) |
|---|---|---|
| e2e, private preview (602 checks; before the L14 merge) | 6 of 6 (build run + 5 repeats) | 17-35 |
| e2e, public (594 checks; built before the L14 merge, so as today's `ONYX_RIGHTS=cleared`) | 8 of 9 (build run + 8 repeats): 1 run stopped on a fresh page whose route had not drawn after 120 s | 14-43 |
| e2e, public `ONYX_RIGHTS=fallback` (501 checks, after the L14 merge) | 6 of 6 (build run + 5 repeats) | 8-22 |
| `the_permit_lasts_until_the_body_is_written_or_the_send_timeout` alone | 50 of 50 (20 + 30) | 25-37 |
| `settles_never_queue_for_a_facilitator_slot` alone | 30 of 30 | 25 |
| whole onyx-api lib suite (321 tests, in parallel) | 10 of 10 with the paused clocks; 8 of 10 before (both failures: `settles_never_queue_for_a_facilitator_slot`, no payment settled) | 21-37 |

Before the change the same machine failed about 1 check-all run in 3 on a different load flake each
time (ROADMAP E9/E10). The fallback runs had the quietest machine of the set. The unexplained
public stop is a real stop now (it used to pass on as a later FAIL or a 30 s click timeout): the
wait names the hash, current nav link, h1 and any pending placeholder when it gives up.


## Chain statistics window (2026-10-18, ADR-0043)

`cargo run --release -p onyx-source-btc --example export-chain-stats -- apps/web/static/onchain
fixtures-live/week:fixtures-live/tail` on the dev box under load average 40-60 (other agents'
jobs), wasmi, recipe `0xc14f0ea9…`. Blocks 969,840-971,199 (1,360 blocks, 10 UTC days), one
shard per block, each preflighted and run, then `scripts/recount-chain-stats.py -j 4`.

| Measure | Value |
|---|---|
| Fuel per block shard | mean 65,446,333, max 81,900,478 (2.05% of 4 B) |
| Densest block (inputs + outputs) | 26,452 (970,266) |
| Export, 1,360 blocks (load, preflight, run) | 1,220 s (0.90 s a block) |
| Python recount, every field of every block | all 22 fields of all 1,360 blocks match |
| `btc-chain-stats.json` (one leaf per block) | 617,819 bytes |
| Rerun input shipped (block 970,835, 1,144 txs) | 190,062 source bytes, 253,441 bytes base64, 16,681,489 fuel |
| `golden_chain` with `ONYX_CHAIN_DIRS` (30 sample blocks rebuilt and rerun, release) | 22.4 s |

Not measured: the browser rerun's wall time on a phone (the page prints it).

## API load test (2026-10-28, north-star loop NL20, LC-22)

`node scripts/load-test.mjs --url http://127.0.0.1:18480 --obs obs.json --levels 1,8,32,64
--seconds 10 --clients 1` (then `--clients 256`). The script refuses any host but this machine.
The live server was **not** load-tested. Instead, the same release binary (`onyx-node` built from
`1bf96cf`, the one deployed with release `2f01280`) ran on the dev box with a copy of the deployed
export: the six level-2 stores and their pins, payments off, intake on, `ONYX_RIGHTS=fallback`,
over plain HTTP on 127.0.0.1 with no Caddy in front. The two label-derived catalog shards and
the chain indexer were off, because the indexer would have read about 455,000 blocks of the
public RPC from the deploy block; they answer 403 on the server anyway. Machine: the Ryzen 5
5600X above, with other jobs running (a Rust build in another worktree), so the numbers are
noisy. Mix per worker, in turn: the six served values, `/v1/health`, a day with no round (404),
an unknown path (404), a malformed subject (308 to its canonical form), a 4 KiB subject (400) and
a broken JSON body to `/v1/submit/check` (400, then 429). Latency is client-side, in ms.

| Clients | Concurrency | Requests in 10 s | Requests/s | p50 | p95 | p99 | max | 5xx or errors |
|---|---|---|---|---|---|---|---|---|
| 1 | 1 | 29,220 | 2,922 | 0.23 | 0.78 | 2.16 | 64.7 | 0 |
| 1 | 8 | 36,447 | 3,645 | 1.62 | 4.42 | 10.24 | 298.2 | 0 |
| 1 | 32 | 60,022 | 6,000 | 3.92 | 13.01 | 29.19 | 136.5 | 0 |
| 1 | 64 | 70,912 | 7,090 | 6.88 | 21.21 | 40.76 | 140.6 | 0 |
| 256 | 1 | 23,031 | 2,303 | 0.24 | 1.22 | 3.76 | 59.6 | 0 |
| 256 | 8 | 40,011 | 4,001 | 1.49 | 4.84 | 12.77 | 96.0 | 0 |
| 256 | 32 | 35,062 | 3,505 | 6.41 | 27.46 | 63.42 | 227.9 | 0 |
| 256 | 64 | 40,374 | 4,033 | 12.47 | 41.12 | 102.05 | 350.5 | 0 |

- Every served value answered 200 at every level, and no request failed or answered 5xx.
  The node stayed under 23 MiB of memory (systemd `MemoryPeak`).
- `/v1/submit/check` is the only route that answered 429: after 3 requests from one client, and
  from many clients after a shared cap. The value, health and 404 routes have no per-client
  request limit: one address sent about 3,500 value reads a second (THREATS NL20-1).
- "256 clients" spreads requests over 256 addresses with `X-Forwarded-For` (the node trusts
  127.0.0.1, as behind Caddy). Its lower rate at 32 and 64 is the per-client limiter's work on
  the intake route plus machine noise; it is not a limit on values.
- Not measured: the server itself (its CPU, Caddy, TLS, the network), and the 443 connection cap
  under load (it caps open connections per address, not requests on a kept-alive connection).
