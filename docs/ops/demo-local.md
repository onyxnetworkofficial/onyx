# M8 rehearsal on a local chain

One command runs the whole Onyx loop on a local anvil chain (chain id 31337): deploy, a real
round on chain, finality, the query API reading roots from the chain, a paid request and a
client check. The Base Sepolia run (M8) uses the same contracts and the same API code; what
still differs is listed under "Gaps". Local only: anvil's public dev keys. Foundry loads a `.env`
from its working directory and project root, and the repo's `.env` holds the testnet key, so the
script runs every `cast` and `forge` call inside a temp dir outside the repo (the deploy uses a
copy of `contracts/`), and forge's broadcast record and script cache stay there (M8-L55-F4).

## Run it
```sh
scripts/demo-local.sh             # honest round, then serve and check (about 1 minute)
scripts/demo-local.sh --dispute   # plus a round with a cheating miner and a dispute panel
scripts/demo-local.sh --serve     # keep the API up afterwards (Ctrl-C to stop)
```
Needs Foundry (`anvil`, `forge`, `cast` in `~/.foundry/bin`, or set `FOUNDRY_BIN`). The first
run builds the release driver (wasmtime); on the dev box run it under
`systemd-run --user --scope -p MemoryMax=16G`. Ports: `ONYX_DEMO_RPC_PORT` (8545) and
`ONYX_DEMO_API_PORT` (8403). Data: the 24 live blocks 970,543-970,566 when the node files are
in `fixtures-live/node` (or `ONYX_LIVE_DIR`; fetched with `scripts/fetch-btc-node.sh`,
git-ignored), else the committed 3-block fixture. `--export-node <dir>` writes the
`onyx-node` inputs (deployment.md "Get the inputs"; the manifest pins `OnyxRegistry`, so the node
serves each shard at most at its attested level, L16); when step 8 runs, the reserve shard is in
its catalog too (held, `servingCheck` set), with its rows and every spec's params in `cas/`.

Gated test (same run, checks every step):
```sh
cargo build --release -p onyx-source-btc --example demo-local
ONYX_DEMO_LOCAL=1 cargo test --release -p onyx-source-btc --test demo_local
```

## What it proves, step by step
The driver is `adapters/source-btc/examples/demo-local.rs`; transactions go through
`cast send --unlocked` from anvil's dev accounts (1 coordinator, 2 miners, 16 validators).
1. Setup: mock USDC, coordinator bond (100), reward pool, 16 validators stake 100 each.
2. The coordinator opens round 1. The contract snapshots the validator set and computes the
   round tag; Rust computes the same tag from the same inputs (checked).
3. Two miners run the recipe (the canonical engine) on the blocks, commit bonded and signed,
   then reveal. The contract recomputes binding and commit hashes.
4. The seed is drawn from `blockhash(beaconHeight)`; Rust's `sample_seed` gives the same
   (checked). Each validator assigned by the seed (25% rate) reruns the recipe, commits and
   reveals a verdict. Validators with no task stand by, so a dispute panel could be drawn
   (L29-F4).
5. `finalizeShard`; the 100-block challenge window passes with no challenge; `release` pays the
   first committer the reward and returns both bonds; everyone withdraws, and the driver checks
   the amounts in commit order: the first committer bond + reward, the other its bond, each
   checker one fee, every standby validator 0 (M8-L55-F2, M8-L56-F1/F2).
6. The API is not handed its shard: its chain indexer (ADR-0033) reads `Finalized` events from
   the settlement's deploy block to the `finalized` block, checks `getFinalizedRoot` and the held
   rows, and only then lists the shard (cursor in `<state dir>/indexer.json`; with `--dispute`
   it resumes after round 2 and lists that shard as known but not held). onyx-api then serves
   the shard with `ChainRoots` (`eth_call` at the `finalized` block; anvil
   reports latest minus 64), pinned to the settlement's code hash. Here the driver reads that
   hash from the same node just before serving, so on anvil the pin cannot fail; on Base Sepolia
   pin it from the deploy output instead (M8-L56-F4). A request without payment gets 402; the client pays with x402 and
   gets 200; it checks the answer with `client::verify_response` against its own
   `getFinalizedRoot` call, and the contract's `verifyRow` accepts the same row and proof.
   The client signs only after `client::PayToPin` accepted the offered `payTo` (here the mock's
   address; the driver also checks a changed `payTo` is refused, REV-4), and the API keeps its
   receipts in `<work>/api-state/onyxrevenue-<mock address>/receipts.jsonl` (`--state-dir`; the
   pin runs its chain-id and code-hash checks against a mock view of the chain, since local mode
   deploys no `OnyxRevenue`): the driver reads a copy and finds
   the paid call's receipt on disk (REV-11). Posting receipts needs `OnyxRevenue`, which only the
   fork and testnet modes deploy (demo-testnet.md, step 6b).
7. `--dispute`: round 2 on the 3-block fixture with explicit zero starting balances (a new shard
   id, so it never collides with round 1, which may use the same fixture; M8-L55-F1). Miner B
   reveals a made-up root. Assigned
   validators disagree, so the shard goes Pending instead of final. `openRerun` opens a case,
   `draw` picks 3 standby validators, each reruns the recipe and votes (commit, reveal),
   `resolve` decides the true root by majority, `settleCase` finalizes it: A is credited bond and
   reward, B nothing (its bond is taken); the driver checks both amounts. If no panel can be drawn, the coordinator reruns and posts the
   root (`arbitrate`, q13 a).
8. Reserve balances (ADR-0034 Amendment 1; needs the reserve tail block files,
   `ONYX_RESERVE_DIR`, default `fixtures-live/tail`, else skipped here; fork and testnet runs
   require them): round 3 settles one `btc-entity-reserve` shard over blocks 971,192-971,199 on
   the state after block 971,191 (its `base_ref` is the committed scan's hash). It is played right
   after round 1 (printed as step 5b), so one wait for the `finalized` block covers both roots
   (on Base Sepolia that saves a second quarter-hour wait). Step 8 then: a second indexer
   (`indexer-reserve.json` in the deployment's state dir) admits it at level 4 (the floor 4 of
   `crypto.btc.entity_reserve` is registered at setup; `btc.entity-reserve`'s metric and label
   set are registered in step 5b); the serving check clears it (the chain lands on the
   committed scan at 971,199). On port API + 2: OKX unpaid 402, paid 200 with `servingCheck`
   (`published-por-file`), checked against the client's own `getFinalizedRoot`, `verifyRow` and
   `verifyRowLeveled` (level 4); Binance 403 `not_cross_checked`. A spec with OKX's base 1,000
   BTC higher reruns to the same root for both miners, and the serving check refuses it. Local
   mode pays through the mock facilitator; fork and testnet modes pin `payTo`, self-settle the
   payment into `OnyxRevenue` and check it was booked to the reserve shard (demo-testnet.md).
   Measured 2026-10-13 (`--dispute`, live blocks, reserve round right after round 1): the whole
   run 93 s and 172 transactions.

Measured on the dev box (2026-10-10, release build, `--dispute`): 64 s and 129 transactions on
the 24 live blocks, 54 s and 125 transactions on the fixture alone; the recipe takes about 1.8 s
per run on the live blocks. With the live files present the driver checks that round 1's root
equals the verified live root under the v1 test labels it uses (`0x99789e60...0727e`; the website
now shows label set v2, whose root differs) and OKX's row equals the Python recount (in
23,427,283,885 sats, out 34,226,666,074, net -10,799,382,189), and fails otherwise; without them
it prints "fixture mode" and skips those two checks (M8-L55-F3).

Re-measured 2026-10-10 with the REV-11/REV-4 checks (live blocks, `--dispute`): 59 s, 131
transactions.

`--json <path>` writes the run's report: each step's transactions, seconds and block, the round
facts, the checked payouts, the served-and-paid check and the dispute outcome. The website's
Live run section reads `apps/web/static/data/live-run.json`, made with
`ONYX_LIVE_DIR=fixtures-live/node scripts/demo-local.sh --dispute --json apps/web/static/data/live-run.json`.

## Gaps before the Base Sepolia run
- x402 runs against the local mock facilitator (payment terms say Base Sepolia, the demo payer
  is a public test key); no token moves. A real settlement needs the x402.org facilitator on
  Base Sepolia and a funded testnet payer (run-api.md "Live testnet run").
- Each actor here is an anvil account the node signs for (`--unlocked`). On Base Sepolia every
  miner and validator signs its own transactions with its own funded testnet key; the driver
  refuses any chain but 31337 by design.
- The API's JSON-RPC transport for a public node is `roots::HttpJsonRpc` over
  `x402::HyperPost::for_rpc` (HTTPS, 1 MiB answer cap, M8-L55-F5); `serve-api` does not yet
  have a chain mode with the settlement address and code hash as config.
- With 16 validators at a 25% rate, round 1 has too few standbys for a panel in about 0.3% of
  runs; the shard then goes Pending and the driver stops with that message (run it again).

## Open questions
- Should the rehearsal run in CI (needs Foundry and about a minute per run)?
