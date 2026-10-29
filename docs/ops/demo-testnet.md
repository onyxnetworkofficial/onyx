# M8 on Base Sepolia (and its fork rehearsal)

One command deploys the contracts, funds per-actor keys, runs a real round on Base Sepolia, serves
the shard with onyx-api, and has a customer pay with x402 and check the row; then it sells a
reserve row (`btc.entity-reserve`) from a second round behind its serving check (steps 5b and 8). Payments settle
straight into `OnyxRevenue`, which credits the shard in the same transaction (ADR-0026). The same
driver as [demo-local.md](demo-local.md), with `--mode fork|testnet`. Testnet only: the driver
refuses anything but Base Sepolia (84532) or a local anvil fork of it.

## 1. Rehearse on a fork (no funds, nothing broadcast; about 1 minute after the build)
```sh
scripts/demo-testnet.sh fork --dispute
```
anvil forks Base Sepolia in memory (`ONYX_FORK_RPC`, default https://sepolia.base.org, read only),
so the run uses the real USDC bytecode and real EIP-3009 signatures. The deployer is anvil's first
dev key; before the round the driver sets its balance to exactly the planned post-deploy budget,
so a passing run shows the plan suffices. Customer USDC is minted inside the fork by pranking
Circle's master minter. Gated test: `ONYX_DEMO_FORK=1 cargo test --release -p onyx-source-btc
--test demo_local` (after `cargo build --release -p onyx-source-btc --example demo-local`).

Receipt path (step 6b, REV-11 and REV-4): after the self-settled sale the customer buys the row
again from a second API instance that uses a standard x402 facilitator (it submits the
authorization to the token, as x402.org does), with its receipt log in
`<state>/onyxrevenue-<address>/standard-facilitator/receipts.jsonl` (one directory per deployed
`OnyxRevenue`, REV-L3-F4). The client pays only after `client::PayToPin` accepted `payTo` from
`<state>/onyxrevenue-<address>/deployment.json` (written right after the deploy, with the runtime
code hash and chain id its own node must show). The reporter commits only once the `credit` is in
the `finalized` block: on the fork the driver mines blocks until it is; on Base Sepolia this adds
about a quarter hour (REV-L3-F8). The driver stops that API right after it answered (a crash),
reopens the log from disk, finds the receipt, posts it with `reporter::Reporter` (`credit`, from
the operator key), checks the shard's book (two sales, per-miner share equal to Rust's
`revenue::split`), and has miner 0 `claim` and `withdraw`: the USDC it receives must equal its
share of both sales. `--state-dir` (default `<work>/api-state`; testnet:
`.onyx-testnet/api-state`, gitignored) holds the journals and the manifest.

Reserve round (steps 5b and 8, ADR-0034 Amendment 1, ROADMAP A2): needs the reserve tail block
files 971,192-971,199 (`ONYX_RESERVE_DIR`, default `fixtures-live/tail`, gitignored, 16 files,
about 45 MB; the script checks them before the plan and stops before sending anything without
them). Fetch them from a Bitcoin mainnet node with
`scripts/fetch-btc-node.sh <ssh-host> 971192 971199 fixtures-live/tail`. Right after round 1 is released and registered in `OnyxRevenue`,
round 3 settles one `btc-entity-reserve` shard on those blocks the same way (the owner registers
`btc.entity-reserve` with floor 4 and label set v2; the coordinator attests and opens; miners
commit and reveal; validators rerun; finalize; the challenge window; release) and registers it in
`OnyxRevenue` (5b). The driver then waits once for the `finalized` block to hold both roots (the
reserve root first; on Base Sepolia one wait of about a quarter hour instead of two). After the
dispute round, step 8: a second chain indexer (`<state>/onyxrevenue-<address>/indexer-reserve.json`)
admits the shard at level 4, the reserve serving check clears it (the chain from block 971,191
lands on the committed scan), and an API on port API + 2 (8406) sells OKX's row: unpaid 402; the
client pins `payTo` from the deployment manifest, pays with x402, the API self-settles into
`OnyxRevenue` with round 1's facilitator (one nonce sequence for the operator key) and the driver
checks the 2,500 micro-USDC was booked to the reserve shard; the client checks the row on its own
node at `finalized`, `verifyRow` and `verifyRowLeveled` (level 4); Binance gets 403
`not_cross_checked` before any 402; a spec on a base with OKX 1,000 BTC higher reruns to one root
for both miners and the serving check refuses it. Reads go through the same pinned, retried node
client as round 1; the indexer and the serving check are asked again on a failed read.

Result on Base Sepolia on 2026-10-27 (NS17, `testnet --yes --export-node`, run-20261027T024839Z):
REHEARSAL PASSED; 145 transactions, 35.2 million L2 gas, 0.000211 test ETH, 2,707 s. The first try
that night stopped at step 3: the 16 validators send their verdict commits one after another,
about 9 blocks apart through the public node, and the 80-block verdict window closed after 6
(`WrongPhase`). The round's verdict commits now close 260 blocks after the beacon and reveals 230 blocks after
that; dispute panels get 60 blocks per phase and the challenge window 200 (`round_cfg` in
`examples/demo-local.rs`); fork runs passed with them first.
Its export is what the hosted API serves ([site-hosting.md](site-hosting.md) "API").

Result with the reserve round on 2026-10-13 (fork, live blocks, `--dispute --sweep`, branch
reserve-live, three runs): REHEARSAL PASSED each time; 171-180 transactions (176-185 sent with
the deploy), 39.7-40.7 million L2 gas, driver 59-86 s; at most 1,301,541 gas per miner, 922,744
per validator, 11,641,518 for the coordinator after deploy (`fixtures/ops/rehearsal-gas.json`).

Result with step 6b on 2026-10-10 (fork, live blocks, `--dispute --sweep`): every step passed;
the credit applied 1 of 1; the shard earned 5,000 micro-USDC from 2 sales and miner 0 withdrew
1,250 (625 per sale: 50% to 2 miners); 157 transactions, 34.0 million L2 gas, driver 49 s; the
sweep returned 0.041 ETH from 19 actor keys. (anvil prices the fork at about 1 gwei, so the
fork's own plan reads 0.129 ETH; Base Sepolia itself is at 0.005 gwei, see below.)

Result on 2026-10-10 (fork of block ~47,944,000, live blocks, `--dispute`): every step passed;
the root equals the website's live root and OKX equals the Python recount; the paid request
settled into `OnyxRevenue` and booked 2,500 micro-USDC to the shard in the settle transaction;
the dispute panel decided the true root. 137 transactions, 32.2 million L2 gas; the driver part
took 33 s and the whole script 48 s (driver prebuilt).

`ONYX_DEMO_ANVIL_CONFIG=<file>` (fork only) uses an anvil fork already listening on
`ONYX_DEMO_RPC_PORT` (its `--config-out` file) instead of starting one, and leaves it running,
so the fork and what the run settled outlive the script; `scripts/docker-smoke.sh` runs the fork
in a container this way and serves the `--export-node` output from both Docker images.

### Level 2 on the fork (`--level2`, ROADMAP B1)
```sh
scripts/demo-testnet.sh fork --dispute --level2
```
Also runs the recorded Treasury level-2 round (ADR-0027) on the fork: the registry owner registers
the feed template, the coordinator draws, posts the transcript log and records the fact. Before
the API serves level 2, the driver opens the fact's revenue book in `OnyxRevenue`
(`registerObservation`, from the operator key, which is the reporter) and runs the opening
again to show it sends nothing (THREATS L2API-4). Without the open book a paid level-2 request
answers 503 (`settle` reverts `NotRegistered`), so the run stops before serving if the book
cannot be opened (a key that is not the reporter, a revert, an unmined send, an unreadable node;
running again is safe: the book is read first). The customer then buys the value and the driver
checks that 2,500 micro-USDC were booked to the fact's observation book in the settle
transaction. The plan adds the level-2 gas (`GAS_LEVEL2_*` in `adapters/source-btc/src/budget.rs`)
and still funds the deployer with exactly the planned budget. The level-2 receipts are signed with the
deployment's own receipt key (ROADMAP B3): `receipt-signer-<registry address>.key` in the actor
keys' directory (0600, made on the first `--level2` run for that registry), refused if it is the
deployer, the reporter, the registry owner or any other actor key; the coordinator sets it with
`setReceiptSigner` before the draw. The 0.01 test USDC covers the
fourth payment exactly. `--level2` is refused in testnet mode (by the script, before the plan or
the deploy): level 2 stays off Base Sepolia until ROADMAP B2-B6. Gated test:
`demo_local::base_sepolia_fork_rehearsal_opens_the_level2_book_before_serving`
(`ONYX_DEMO_FORK=1`).

Result on 2026-10-16 (fork, live blocks, `--dispute --level2`, branch revenue-books): REHEARSAL
PASSED; the book opened in one transaction and the second opening sent nothing; the level-2 sale
was booked to the book (two runs); 183 and 189 transactions; coordinator 13,342,878 and 13,595,005
gas after the deploy (11,881,398 in the same day's run without `--level2`), 82 transactions (68);
registry owner 7 registrations
(`fixtures/ops/rehearsal-gas.json`, `level2`).

Running onyx-node with a results store (`ONYX_OBSERVATIONS`) in self-settle mode outside this
driver: the node does not open the books itself yet (THREATS L2API-4, residual for B6); open each
served fact's book with the reporter key before serving.

## 2. What the real run needs (the project owner)
1. `.env` with `BASE_SEPOLIA_PRIVATE_KEY` (a testnet-only wallet) and optionally
   `BASE_SEPOLIA_RPC_URL` (default https://sepolia.base.org; a dedicated endpoint avoids public
   rate limits).
2. Base Sepolia ETH on that wallet from a faucet you choose. Check the exact amount first:
   ```sh
   scripts/demo-testnet.sh testnet --plan
   ```
   It prints the budget at the node's current fees and the deployer's ETH and test USDC, sends
   nothing, and fails if either is short (M8-L58-F6). `--dry-run` is the same flag. Any other
   unknown argument is refused, and a real run needs `--yes` (M8-L58-F1). At 2026-10-10 fees (base 0.005 gwei, tip 0.001 gwei) the plan
   was **0.00135 ETH** in all (deploy 0.00056 ETH, then 0.00079 ETH for the coordinator and the
   18 actors), already three times the gas measured on the fork. With the receipt path (miners
   claim and withdraw, the operator posts a transfer and a `credit`) the plan at the same fees is
   **0.0014025 ETH** (read-only `--plan` against https://sepolia.base.org, 2026-10-10). With the
   reserve round (2026-10-13, fee cap 0.011 gwei) it is **0.00179916 ETH** (deploy 0.0006435,
   then 0.00115566; 6 owner registrations). 0.01 ETH covers it with room.
3. **0.01 test USDC** on the same wallet (Circle's faucet, Base Sepolia): the customer's x402
   payments (2,500 micro-USDC each; a run makes 3: round 1's row, the receipt path, the reserve
   row). Bonds and stakes use the deployed free-mint MockUSDC, not
   USDC.

## 3. The real run
```sh
scripts/demo-testnet.sh testnet --plan             # budget, balances; sends nothing
scripts/demo-testnet.sh testnet --dispute --yes    # the real run
scripts/demo-testnet.sh testnet --dispute --yes --sweep   # and return leftover actor ETH
```
Order: build the driver; `--plan` (stops if short); forge deploys `Deploy.s.sol` and
`DeployRevenue.s.sol` (`--slow`, from a copy of `contracts/` in a temp dir, so no `.env` or
broadcast files land in the repo); the driver funds 2 miners and 16 validators with their planned
ETH and the customer with test USDC, runs round 1 (waiting for real blocks: windows are 40-300
blocks of 2 s), releases, registers the shard in `OnyxRevenue`, runs the reserve round the same
way, waits until both roots are in the `finalized` block (about 15-20 minutes on Base Sepolia,
once), serves and pays, the receipt path (its `credit` waits for finality again), the dispute
round, then sells the reserve row. Expect about 60-80 minutes in all. Keep the console output
for the Run page: `... 2>&1 | tee .onyx-testnet/runs/console-<UTC stamp>.log`, then
`python3 -I scripts/build-testnet-run.py <run log> <console log> <out.json>` (it reads steps 5b
and 8 when present, both or neither, and refuses any other step line).

Keys: the deployer key goes from `.env` into the environment of forge and the driver only; the
driver takes it out of its own environment at start, so no `cast` child inherits it, and the node
URL reaches cast, forge and the driver in the environment, never on a command line (errors show
`scheme://host` only; M8-L58-F7). The API's hot key is a separate `operator` key in
`.onyx-testnet/keys`: the deployer (OnyxRevenue's owner) makes it the reporter with `setReporter`
and funds it with gas only, so it can post settles and receipts and move nothing (M8-L58-F5).
`--sweep` sends what each actor key holds in ETH back to the deployer after the run (the
customer's leftover test USDC stays; it has no ETH for gas; M8-L58-F8).
Actor keys are created in `.onyx-testnet/keys` (gitignored; dir 0700, files 0600; never printed)
and reused by later runs, so leftover test ETH stays usable. The run log, with BaseScan links for
every transaction and the spend per address, is written to `.onyx-testnet/runs/` (gitignored);
copy the public parts into `docs/ops/demo-testnet-summary.template.md` to publish a summary.

## Level 2 on Base Sepolia (ROADMAP B6)
```sh
scripts/level2-sepolia.sh usgs <export dir>   # one live earthquake round, about 30 minutes
scripts/level2-sepolia.sh ncei <export dir>   # one live temperature round
```
Runs one level-2 round of a dated source on the registry of a testnet run (the export's
`deployment.json`): the coordinator (`examples/level2-coordinator.rs` of the adapter, the shared
`onyx_level2::live`) and 7 observer processes, 5 drawn. Signers: the coordinator is the deployer
(`OnyxSettlement.coordinator()`; its key goes from `.env` into the coordinator's environment
only), the registry owner registers the metric once (`registry-owner.key`), each observer posts
its own commit with its own key (the run's standby validators `validator-9..15.key`), and the
receipt key is the deployment's own (`l2-receipt.key`, made once). Every key signs in its own
process (`KeySigner`, the same EIP-1559 signing as the driver); none is printed or put on a command
line. The day is the newest the source's rule allows (`--record-date latest`). The coordinator
reads the commits once the commit phase is final (about 20 minutes on Base Sepolia), receipts each
reveal on chain, records the value (`recordObservation`) and appends the round to
`<export dir>/observations/<source>.json`, with the template pin in `observations/pins`
(`scripts/deploy-api.sh` ships both). A run that stops is finished by running it again: the book,
spool and window stay in `~/.cache/onyx-b6/<source>`.

More series of the same adapters (north-star loop NL17, ROADMAP S13): `ONYX_L2_SERIES` picks one,
and its store is `observations/<source>-<series>.json` with its state in
`~/.cache/onyx-b6/<source>-<series>`:
```sh
ONYX_L2_SERIES=usw00003017.prcp scripts/level2-sepolia.sh ncei <export dir>   # Denver precipitation
ONYX_L2_SERIES=usw00094728.tmax scripts/level2-sepolia.sh ncei <export dir>   # Central Park high
ONYX_L2_SERIES=usw00094728.prcp scripts/level2-sepolia.sh ncei <export dir>   # Central Park precipitation
ONYX_L2_SERIES=la300.m2_5 scripts/level2-sepolia.sh usgs <export dir>         # M2.5+ near Los Angeles
```
The same variable picks the series for `examples/level2-check.rs`. Rounds share the observer keys,
so they run one after another.

Result, 2026-10-27/28, the same registry (14 transactions each: coordinator 8, owner 1, observers 5;
about 0.0000118 test ETH each; the deployer kept 0.0012 test ETH):

| Series | Day | Value | Round | Agreed |
|---|---|---|---|---|
| Denver precipitation (`usw00003017.prcp`) | 2026-09-27 | 0.0 mm | `0x61994924…6098` | 5 of 5 |
| Central Park daily high (`usw00094728.tmax`) | 2026-09-27 | 17.2 °C | `0xaddc3ee6…8b1d` | 5 of 5 |
| Central Park precipitation (`usw00094728.prcp`) | 2026-09-27 | 40.1 mm | `0xf5fb60e4…ba06` | 5 of 5 |
| M2.5+ within 300 km of Los Angeles (`la300.m2_5`) | 2026-07-20 | 1 | `0x8efb57d8…cd8a` | 4 of 5 |

In the Los Angeles round one drawn observer's own fetch failed (its observation was "source
unavailable"); the other four agreed and the value settled, as the round rule allows. All four
stores are in the export's `observations/` with their pins; the hosted node serves them after the
final deploy.

Base Sepolia makes a block every 0.2 s, so off anvil a draw's beacon goes 50 blocks ahead
(`LIVE_DRAW_LEAD`, about 10 s) and the coordinator waits for that height instead of mining it.

Result, 2026-10-27, registry `0x497389e04f7d58e07a79ac8d395c3927aecbdf2b`:

| Source | Day | Value | Round | Transactions | Test ETH |
|---|---|---|---|---|---|
| USGS M5+ earthquakes, worldwide | 2026-07-19 | 5 | `0x1dba7daf…071c` (attempt 0) | 15 (coordinator 9, owner 1, observers 5) | 0.000012250 |
| NCEI daily high, Denver airport | 2026-09-27 | 28.3 °C | `0xbdb43692…2228` (attempt 0) | 14 (coordinator 8, owner 1, observers 5) | 0.000011811 |

Each: 5 commits read from the chain, 5 reveals, all 5 agreed, every drawn observer saw its receipt
on chain, 2 not drawn. Served by a local node with the export's settings, both values passed a
client's check off chain and on chain (`examples/level2-check.rs`), and a changed value was
refused. Limits: one host and one operator for all 7 observers (THREATS B6-5, L2-SRC-1); reveals
through a local spool, not sealed on chain (REVLOG-1 applies and the API's `limits` says so).

## Markets on Base Sepolia (NORTH-STAR NS11)
```sh
scripts/markets-sepolia.sh deploy <export dir>                  # once: tONYX, stakes, panel, futures
scripts/markets-sepolia.sh market <export dir> open --record-date 2026-10-27 \
  --edges 4045000000000000,4050000000000000 --close 1793188800 --observable 1793232000 \
  --resolve-by 1793836800                                       # Treasury debt, in cents
scripts/markets-sepolia.sh market <export dir> bet --market 1 \
  --key-file .onyx-testnet/keys/miner-0.key --bucket 0 --amount 30
scripts/markets-sepolia.sh market <export dir> status [--out FILE]
```
A futures market on a value no round can observe yet, settled from the level-2 value observers
later record in the deployment's registry (ADR-0042 amendment 2). `open` refuses a date a round
may observe before trading closes, lists the registry fact in `OnyxObservedFacts` with the first
block a recording may come from, and opens the market. `bet` gives a new bettor 0.00005 test ETH
for gas and mints the tONYX it bets (the deployer is the minter); each bettor signs with its own
key file. Level-2 observers (`validator-9..15`) do not bet (THREATS NL6-4). `status` reads every
market from the chain. The Markets page reads the same markets in the reader's browser
(`apps/web/static/markets/base-sepolia.json` names the addresses and each market's title; add a
market there after `open`).

Live, 2026-10-27 (addresses in [deployment.md](deployment.md)): market 1 on the U.S. Treasury's
total public debt outstanding for record date 2026-10-27, published on 2026-10-28. Buckets in
cents: below $40.45 trillion, $40.45-40.50 trillion, above $40.50 trillion. Trading closes
2026-10-28 12:00 UTC (before the Treasury publishes); a round may observe the date from
2026-10-29 00:00 UTC; void with refunds if no value is proved by 2026-11-05. Four test bets: 30,
50, 20 and 25 tONYX (miner-0, miner-1, validator-0, customer); 125 tONYX in the pools.
Settled 2026-10-29 (NL15): the round observed $40,582,274,254,843.57 (3 of 5 observers agreed, 2
got no answer from the source and sent Missed), so bucket 2 won; validator-0 was paid 122.9 tONYX
and the 2.1 tONYX fee is owed to the treasury.

Market 2 (2026-10-29, NL23) is the first market on the guarded futures (insiders may not bet,
trading closes before the source publishes): the debt for record date 2026-11-02, buckets under
$40.60 trillion, $40.60-40.70 trillion and above; trading closes 2026-11-01 23:00 UTC; a round may
observe it from 2026-11-04 00:00 UTC. It settles the same way with
`ONYX_MARKETS_FILE=<export dir>/insurance.json` and `--market 1`. The page lists it under the
insurance block's `markets` with `"number": 2`.

To settle market 1 (after 2026-10-29 00:00 UTC):
```sh
ONYX_L2_RECORD_DATE=2026-10-27 scripts/level2-sepolia.sh fiscaldata <export dir>   # about 30 minutes
scripts/markets-sepolia.sh market <export dir> settle --market 1 \
  --store <export dir>/observations/fiscaldata.json \
  --key-file .onyx-testnet/keys/miner-0.key --key-file .onyx-testnet/keys/miner-1.key \
  --key-file .onyx-testnet/keys/validator-0.key --key-file .onyx-testnet/keys/customer.key
```
`settle` checks the store as the server does, proves the value (`OnyxObservedFacts.prove`: its
result hash with the recorded spec and the round id must equal the registry's), resolves the
market and pays each winner. Limit: on chain the market trusts the coordinator's recorded result,
as every level-2 fact does; clients check it off chain (THREATS NL6-2).

## Insurance on Base Sepolia, and the meeting demo (NL14, NL18)

The second markets deployment (ADR-0042 amendment 3; addresses in docs/ops/deployment.md) adds
guarded listings and futures and `OnyxParametric`, a test insurance contract: a policy pays a fixed
amount when a recorded level-2 value crosses a line. Commands (`scripts/insurance-sepolia.sh`):

```sh
scripts/insurance-sepolia.sh deploy <export>      # once; writes <export>/insurance.json
scripts/insurance-sepolia.sh offer <export> --metric M --template 0x.. --subject S \
  --cmp atleast|atmost --threshold N --payout A --premium A --buy-by T --close-by T \
  --observable T --resolve-by T --max N
scripts/insurance-sepolia.sh status <export>
scripts/insurance-sepolia.sh demo-fork <export>   # the meeting demo, fork only
```

Live products (2026-10-27; buying closes 2026-10-31 00:00 UTC): product 1, "Denver high on
2026-10-31 at most 0.0 °C" (100 tONYX per policy, premium 10; settles from 2026-12-01, when the
NCEI rule lets a round observe that day), and product 2, "at least 6 M5+ earthquakes worldwide on
2026-10-31" (50 tONYX, premium 15; from 2027-02-09, the USGS rule's 100 days). The deployer is an
insider (owner and the registry's coordinator), so it can neither underwrite nor buy them; a
non-insider key does both. The pool is funded with
`ONYX_MARKETS_FILE=<export>/insurance.json scripts/markets-sepolia.sh market <export> underwrite
--key-file .onyx-testnet/keys/customer.key --amount 1000` (the tool refuses an insider or observer
before any gas grant or mint; 2026-10-28: 1,000 tONYX, block 51534119, so both products have cover
for sale).

**The live summary** (`scripts/meeting-demo.sh`, north-star loop NL26): one read-only command
prints what the partners can click through, from the live API and Base Sepolia: the API's chain
and payment state, each checked series with its value and proof level (it exits 1 when one is not
served), market 1's result and winning bucket, market 2's open pools and close time, and the
insurance pool with each product's terms and whether cover is for sale. Addresses come from the
site's own markets file; no key, no transaction. Env: `ONYX_API`, `ONYX_MARKETS_RPC`.

**The meeting demo** (`demo-fork`) shows a payout today without waiting for those dates and
without touching Base Sepolia. It starts a throwaway anvil fork of Base Sepolia (port
`ONYX_DEMO_PORT`, default 8549), uses no real key (anvil's own unlocked test accounts, and the
registry's coordinator impersonated on the fork), and stops the fork when it ends:

1. It deploys fork copies of `OnyxObservedFacts` and `OnyxParametric` (from `contracts/out`) and
   offers the live products again on them: same facts, same terms. Only the earliest recording
   block is near, because a fork cannot skip the millions of blocks to the real dates.
2. An anvil test account underwrites 500 tONYX; another buys one policy of each product.
3. For each product it moves the fork's clock past `observableAt`, and the coordinator records a
   demo value through the real registry code: announce a draw, fix its beacon, record the result.
   Anyone then proves the value and settles the product.
4. Product 1 gets a value that triggers it, product 2 one that does not.

Output of the 2026-10-27 run (values in the feed's units: tenths of °C, a count):

```
underwriter put 500 tONYX in the pool; buyer bought 1 policy of each product (balance now 75.0 tONYX)
product 1: the coordinator recorded the demo value -25 (threshold 0, at most); PAID: the buyer claims the payout
product 2: the coordinator recorded the demo value 4 (threshold 6, at least); EXPIRED: the premium stays with the pool
buyer: 75.0 -> 175.0 tONYX; pool now 425.0 tONYX
DEMO PASSED (fork only: demo values, test token, nothing sent to Base Sepolia)
```

The demo values are made up for the fork; real values come only from observer rounds. The pool
ends at 500 + 25 premiums - 100 payout = 425 tONYX.

## How the driver signs (fork, testnet)
Each transaction is built by the driver: calldata from `cast calldata` (offline), gas from
`eth_estimateGas` plus 25%, nonce from `eth_getTransactionCount pending`, fees `2 * base + tip`,
signed in process (`onyx_api::evm_tx::Tx1559`, byte-identical to `cast mktx`), sent with
`eth_sendRawTransaction`, and waited for. Spend per sender (L2 gas, wei, the L1 data fee from the
receipt or the OP Stack oracle) is summed for the budget report. The API's self-settle path uses
the same signer (`evm_tx::Sender`).

## Gaps and limits
- The shard a payment is credited to is still the operator's choice (THREATS REV-1, narrowed).
- A settle stuck behind a dropped transaction is not fee-bumped (M8-L57-N3).
- The anvil fork answers `eth_maxPriorityFeePerGas` with 1 gwei, so fork runs pay far more per
  gas than Base Sepolia does; the plan always reads the target node's fees.
- Validator stakes stay staked after the run (the unbond delay is 1,800 blocks); they hold free
  MockUSDC only.
