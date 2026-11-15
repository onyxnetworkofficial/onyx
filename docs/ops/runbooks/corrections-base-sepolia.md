# Runbook: deploy OnyxCorrections on Base Sepolia (CL7, ADR-0057)

**Trigger:** the owner decides to put correction markets on the test network: one Yes
("accurate") / No market per published level-2 value, settled only by a later recheck round of
the paired `.recheck` metric (ADR-0057 and its amendment 1).

**What it is:** `OnyxCorrections` is a contract of its own. It reads the running `OnyxRegistry`
and takes bets in the markets' tONYX test token; nothing else changes (no redeploy of the
registry, the markets or `OnyxPrepaid`). The deployer owns it: the owner can only `pair` a
metric with its `.recheck` metric (append-only); it cannot bet, resolve or void outside the
rules everyone follows. The deployer is also the treasury (open fees, the 2% trading fee,
rounding dust).

**Money:** tONYX only, a test token with no value. Every economic number is a placeholder
(`DeployCorrections.defaults()`, ADR-0057 open question 1). No real funds, ever (ADR-0042 item
15: legal review before anything of value is traded).

**Who:** the owner, with the deployer key in `.env` (never printed, never on a command line).
The registry's owner is another test key (`.onyx-testnet/keys/registry-owner.key`; never the
coordinator, REG-R1-F3): it registers the recheck metric, the deployer pairs it. **Time:** about 20 minutes for
steps 1-6. **Cost:** one contract creation and three owner transactions, then two to four for the
first market (the fee minted and gas granted when the opener lacks them, its approval, the open);
a dry run prints forge's estimate first. Test ETH only.

**Rehearsed:** the recheck runner end to end on a local fork of Base Sepolia
(`scripts/recheck-fork.sh`, step 7, 2026-11-15); locally by `scripts/corrections-anvil.sh` and
`scripts/recheck-anvil.sh` (both in check-all): the same contracts, the live manifest's real template ids, a market opened as step 6
opens it (`DeployCorrections.openMarket` with blocks from `scripts/corrections-blocks.py`: the
fee minted to an opener that held none, a forged value refused before anything was sent, a
second run sending nothing), rechecks that disagree and agree, payouts to the wei. **Deployed** on Base Sepolia 2026-11-01 (step 2:
`0x3765dab41115fc2b849b13f81c75fce96a771133`, docs/ops/deployment.md), and steps 3 to 5 done the
same day for `us.treasury.tga_total_deposits`: its `.recheck` metric registered (template
`0x2d5828d8ebfcbfa6545463c11e43ab24aee03e8cf2fa7ba65f0985345d0c3016`, block 53274589) and paired
with tolerance 0; `recheck-plan` against Base Sepolia reads it (state `none`: no market yet).

## Preconditions

1. A checkout of the commit to deploy (its `scripts/check-all.sh` passed), `forge` and `cast` in
   `~/.foundry/bin`, `cargo`.
2. `.env` holds `BASE_SEPOLIA_PRIVATE_KEY` (the deployer: the coordinator and the markets'
   tONYX minter; `.onyx-testnet/keys/registry-owner.key` is the registry's owner) and, optionally, `BASE_SEPOLIA_RPC_URL`. Load it into the shell without echoing
   it: `set -a; . ./.env; set +a`. `forge script` reads the key from the environment inside the
   script; every `cast send` below uses `--interactive` (a hidden prompt), so the key is never on
   a command line or in the shell history.
3. The addresses already live (docs/ops/deployment.md, "Markets next to this registry"):

   ```sh
   export ONYX_REGISTRY=0x497389e04f7d58e07a79ac8d395c3927aecbdf2b
   export ONYX_TONYX=0x1cb5f5816812991ac3b52c63c3fcadcc9f544579
   RPC=${BASE_SEPOLIA_RPC_URL:-https://sepolia.base.org}
   ```

4. The value to correct is a numeric level-2 value of a dated or daily manifest source with a
   recorded round, for example `us.treasury.tga_total_deposits` (record date 2026-10-28, 18463,
   recorded 2026-10-30).

## Steps

**The script (2026-11-01):** `scripts/corrections-sepolia.sh` runs steps 1 to 5 with the deployer
key from `.env` passed to forge in the environment (never on a command line, never printed):
`plan` (a dry run of the deploy, simulated on the live chain's state; nothing sent), `deploy --yes`
(step 2; the address and block go to `.onyx-testnet/corrections-sepolia.json`), `pair MANIFEST
DATE` (steps 3 to 5 simulated) and `pair MANIFEST DATE --yes` (sent: the recheck metric registered
unless the registry holds it, then paired; `DeployCorrections.pair` refuses another template or
tolerance and sends nothing for what is done), `open MANIFEST DATE [--close-at T] [--yes]` and
`bet MARKET yes|no TONYX [--key NAME] [--yes]` (step 6, 2026-11-14) and `status` (the settings and
every market). The manual steps below stay as the reference.

1. **Rehearse on a fork first** (about 5 minutes, no transaction on Base Sepolia):

   ```sh
   anvil --fork-url "$RPC" --port 8546 &
   cd contracts
   forge script script/DeployCorrections.s.sol --rpc-url http://127.0.0.1:8546 --broadcast
   cd ..
   ```

   The script refuses any chain but 84532 and 31337 and an RPC whose chain id differs from the
   script's (`ChainMismatch`). Note the printed address; run steps 3-5 against the fork too, then
   stop anvil.

2. **Deploy** (one transaction):

   ```sh
   cd contracts
   forge script script/DeployCorrections.s.sol --rpc-url "$RPC" --broadcast
   cd ..
   export ONYX_CORRECTIONS=0x...   # the address forge printed
   ```

   Check it: `cast call $ONYX_CORRECTIONS 'registry()(address)' --rpc-url $RPC` is
   `$ONYX_REGISTRY`, `token()` is `$ONYX_TONYX`, `owner()` and `treasury()` are the deployer,
   `openFee()` is 5 tONYX. Record the address, block and gas in docs/ops/deployment.md.

3. **Compute the recheck template** of the source (a pure function, `onyx_core::recheck_of`;
   7 days after the original's last observable day, for 7 days, are the placeholders):

   ```sh
   cargo run --locked -q -p onyx-level2 --example onyx-source -- recheck \
     sources/us.treasury.tga_total_deposits/manifest.json --after-days 7 --span-days 7 \
     --date 2026-10-28
   ```

   It prints `original_metric_key`, `recheck_metric_key`, `recheck_template_id`, `tolerance`,
   `subject_key` and the `recheck_days` window. On 2026-10-30 (after the CL8 merge put `Bucketed` before `Recheck`) it printed recheck template
   `0x2d5828d8ebfcbfa6545463c11e43ab24aee03e8cf2fa7ba65f0985345d0c3016`, tolerance 0, recheck days
   2026-11-14 to 2026-11-20 for the record date 2026-10-28.

4. **Register the recheck metric** in the registry (owner; kind 3 = dated feed; append-only):

   ```sh
   cast send "$ONYX_REGISTRY" 'registerMetric(bytes32,uint8,bytes32,uint8)' \
     <recheck_metric_key> 3 <recheck_template_id> 0 \
     --rpc-url "$RPC" --interactive
   ```

   Check: `cast call $ONYX_REGISTRY 'metrics(bytes32)(uint8,bytes32,uint64)' <recheck_metric_key>`
   returns 3 and the template id from step 3.

5. **Pair** the original with its recheck metric (owner; append-only; the tolerance from step 3):

   ```sh
   cast send "$ONYX_CORRECTIONS" 'pair(bytes32,bytes32,uint128)' \
     <original_metric_key> <recheck_metric_key> <tolerance> \
     --rpc-url "$RPC" --interactive
   ```

   Check: `cast call $ONYX_CORRECTIONS 'pairs(bytes32)(bytes32,uint128)' <original_metric_key>`.

6. **Open the first market and bet on both sides** (anyone; here operator test keys, never the
   deployer; about 10 minutes). Each command is a dry run first (forge simulates on the live
   chain's state; nothing sent), then the same command with `--yes`. Trading closes 2026-11-17
   00:00 UTC, which leaves four recheck days (11-17 to 11-20); right after the open, one Yes and
   one No bet from two miner keys, so the market has both sides (a market whose winning side has
   no bet voids):

   ```sh
   M=sources/us.treasury.tga_total_deposits/manifest.json
   scripts/corrections-sepolia.sh open $M 2026-10-28 --close-at 2026-11-17T00:00:00Z
   scripts/corrections-sepolia.sh open $M 2026-10-28 --close-at 2026-11-17T00:00:00Z --yes
   scripts/corrections-sepolia.sh bet 1 yes 10 --key miner-0
   scripts/corrections-sepolia.sh bet 1 yes 10 --key miner-0 --yes
   scripts/corrections-sepolia.sh bet 1 no 10 --key miner-1
   scripts/corrections-sepolia.sh bet 1 no 10 --key miner-1 --yes
   scripts/corrections-sepolia.sh status
   ```

   What it does:
   - **The value:** the original round's `roundId`, `windowEnd` and value from the node's answer
     (`https://onyx.cryptagraph.com/v1/observations/us.treasury.tga_total_deposits?subject=us.treasury.tga_deposits:2026-10-28`;
     `ONYX_API_URL` names another node), its metric, subject and template checked against the
     manifest; the on-chain value is the answer's number (`answer`, not the decimal text).
   - **The blocks** (`scripts/corrections-blocks.py`), at the block time measured over the last
     432,000 blocks (0.2 s on Base Sepolia): the close the block expected at `--close-at`
     (refused before the earliest allowed close, after a day before the last recheck day starts,
     or when it leaves less than `minRecheckBlocks` before the deadline); without it the earliest
     the contract allows (`minTradingBlocks`, 18,000 blocks: an hour) plus 9,000 blocks (30
     minutes) for the build and the sends to land (`CORRECTIONS_CLOSE_MARGIN_BLOCKS`). The
     deadline is the block expected at 12:00 UTC on the day after the last recheck day
     (`recheck_days` of step 3), so a recheck round started on the last day can still be
     recorded by it.
   - **The checks** (`DeployCorrections.checkOpening`, before anything is sent): the contract's
     pair is the manifest's `.recheck` metric and the registry holds its template (as `pair`
     checks; other `CORRECTIONS_AFTER_DAYS`/`SPAN_DAYS` are refused); the close and deadline
     against the contract's own `minTradingBlocks`, `minRecheckBlocks` and `maxMarketBlocks`;
     the close before the last recheck day ends and the deadline after it; the round and value
     against the registry's recorded result (`NotTheRecordedValue`); no recheck recorded yet;
     the opener not an insider (owner, treasury, coordinator).
   - **The opener:** `.onyx-testnet/keys/miner-0.key`, a markets test bettor
     (`CORRECTIONS_OPENER_KEY_FILE` names another; level-2 observers and the registry's owner are
     refused). When it holds less than the 5 tONYX fee, or less than 0.00002 test ETH, the deployer
     (the tONYX minter) first mints the rest and grants 0.00005 test ETH, as
     `scripts/markets-sepolia.sh` does a bettor. It approves the fee unless approved, then opens.
   - **The record:** the market id forge logged (read back from the chain, a few tries while the
     RPC lags) and its blocks go to `.onyx-testnet/corrections-sepolia.json`. Run again when the
     value has a market, open, closed or settled, it sends nothing and records that market;
     `--new-market` opens another.
   - **The bets** (`DeployCorrections.placeBet`): whole tONYX from `--key NAME`
     (`.onyx-testnet/keys/NAME.key`, miner-0 by default); refused before anything is sent for an
     insider, a level-2 observer's or the registry owner's key, a market that is not open or has
     closed, a side other than yes or no, an amount under `minBet` or over the 100 tONYX per
     bettor, or a side past half the pool cap. The deployer mints the stake and grants gas when
     the bettor lacks them; the bettor approves the stake unless approved.

   **Dry runs 2026-11-14** (nothing sent). `open --close-at 2026-11-17T00:00:00Z` at head block
   58790851 (18:13 UTC): the node's round
   `0xe0001998052d14c709f552c542da326f7a3baf343b9da41627c6e91a7ad836cf` (windowEnd 1793335911),
   value 18463; close block 59758656 (~2026-11-17 00:00 UTC), deadline block 61702656
   (~2026-11-21 12:00 UTC). The opener held no tONYX and 0.000038 test ETH (no gas grant), so
   three transactions: the deployer mints 5 tONYX to it, it approves 5 tONYX and opens market 1
   (forge's estimate: 729,212 gas, 0.0000073 test ETH). With `CORRECTIONS_AFTER_DAYS=8` it was
   refused (`RecheckRegisteredOtherwise`). The two bets, dry run on a local anvil fork with that
   market opened there: miner-0 Yes 10 and miner-1 No 10, each holding no tONYX, so each is a
   mint of 10 tONYX, an approval and the bet (about 457,600 gas, 0.000005 test ETH each).
   `--yes` computes the blocks again from the head it starts at.

   By hand (the reference; approve 5 tONYX first; the same bounds):

   ```sh
   cast send "$ONYX_TONYX" 'approve(address,uint256)' "$ONYX_CORRECTIONS" 5000000000000000000 \
     --rpc-url "$RPC" --interactive
   cast send "$ONYX_CORRECTIONS" 'open(bytes32,bytes32,uint64,bytes32,int128,uint64,uint64)' \
     <original_metric_key> <subject_key> <windowEnd> <roundId> 18463 <closeBlock> <resolveBlock> \
     --rpc-url "$RPC" --interactive
   ```

   A wrong value or round is refused (`WrongValue`).

## Left before a market can settle on Base Sepolia

7. **The recheck round** (THREATS CL7-3, CL7-20, CL7-23 to CL7-27). **The script
   (2026-11-15):** `scripts/recheck-sepolia.sh` runs steps 7 and 8 for one value, from the plan
   to the treasury's payment. `E` is the node export dir of the testnet run (its
   `deployment.json` and `observations/`, as `scripts/level2-sepolia.sh` takes it):

   ```sh
   M=sources/us.treasury.tga_total_deposits/manifest.json
   scripts/recheck-sepolia.sh check $M 2026-10-28 "$E"        # the pre-flight, read-only
   scripts/recheck-sepolia.sh run $M 2026-10-28 "$E"          # dry run: nothing sent
   scripts/recheck-sepolia.sh run $M 2026-10-28 "$E" --yes    # on a recheck day after the close
   ```

   Run it on the first recheck day after the close (market 1: 2026-11-17, after block
   59,758,659, about 00:00 UTC; the recheck must be recorded by block 61,702,659, about
   2026-11-21 12:00 UTC); `run --yes` takes about 35 minutes, so run it where a closed terminal
   does not stop it: inside tmux, as `systemd-run --user --scope -p MemoryMax=12G
   scripts/recheck-sepolia.sh run $M 2026-10-28 "$E" --yes`. One run at a time per value holds a
   lock outside the state dir (`flock` in `RECHECK_LOCK_DIR`, default `~/.cache/onyx-b6/locks`;
   a refused run names the holder and its live coordinator and observer PIDs); each run logs to
   `<state>/logs/<UTC time>/` (`<state>/logs/latest` is the last).
   What it does:
   - **The plan** (`level2-manifest recheck-plan`, below): `none` (no market), `wait` (trading
     open: the close block and its estimated UTC time; the dry run lists what `--yes` will send
     after it) and `void-only` stop cleanly; `run` goes on.
   - **The round** (state `run`, unless the registry already holds the recheck and the store has
     it): as `scripts/level2-sepolia.sh` runs a manifest round, with `--recheck 7:7` for the
     coordinator and every observer. Observers are ONLY the plan's pool: the candidates
     (`RECHECK_POOL_KEYS`, default `validator-0..15`) without every bettor of the value's
     markets and every observer of the original round (11 of 16 for market 1); the original
     store's round must be the one the registry recorded (CL7-27). The coordinator (the
     deployer, `BASE_SEPOLIA_PRIVATE_KEY` read from `.env` by the script and given to the
     coordinator in its environment only; any such variable the shell already holds is dropped)
     runs with `--pool <the plan's pool> --window-start W`. **The window (CL7-24, CL7-25):** the
     script reads the round book with the coordinator's own code (`level2-manifest book-held`):
     a draw the book holds is resumed on the window it was announced for, whatever the time (it
     may need only its receipts and record), so no second draw is announced; with none held (a
     first run, a run stopped before its draw, a draw that never landed, or a round that
     recorded no value) a new window: the plan's `windowStart`, or `ONYX_L2_LEAD` (150) seconds
     from now when that is later, since a live round is observed in real time. A new window past
     the last recheck day is refused, and so is a new round when about 40 minutes of blocks from
     the head reach the deadline; so is a state dir whose book holds no draw while the chain
     counts more draws than it (a lost book, another state dir). The live coordinator itself
     refuses a new draw less than 60 s before its window, at start and again at the moment of
     announcing: a held draw that never landed is released and then refused, with nothing sent,
     and the next run takes a new window. Each round that records no value is one of the subject's
     three draws (`DrawsExhausted` after three): the script prints the draws used and drops that
     round's pool so the next draw rebuilds it. The recheck metric is registered by `pair`
     before any market, so the runner never registers (no `--owner`). Its results store is
     `$E/observations/<source>.recheck.json` and its pin goes to `$E/observations/pins` whenever
     the store exists (`scripts/deploy-api.sh` ships both). A run that stops is finished by
     running it again: the state dir (`~/.cache/onyx-b6/<source>.recheck-<date>`, `--state DIR`)
     keeps the book, the drawn pool and the windows used, and a plan pool that differs from the
     drawn one is refused. `run` refuses unless `OnyxCorrections.registry()` is the export's
     registry (CL7-27). The RPC (`ONYX_L2_RPC`, default `https://sepolia.base.org`) must be
     Base Sepolia and serve archive state: the coordinator finds the registry's deploy block by
     reading code at past blocks, and a pruned endpoint fails there, before the draw. A loopback
     RPC is the rehearsal's fork only (chain id `RECHECK_FORK_CHAIN_ID`, 845320; CL7-23).
   - **Then** `recheck-resolve` from `RECHECK_SENDER_KEY` (miner-0, the opener: anyone may
     resolve) for the markets the round serves (one that closes too late is left to void), and
     `recheck-payout`: `withdraw` from every key of `RECHECK_BETTOR_KEYS` the contract owes
     (winners; every bettor of a void market), then `payTreasury()`. Each reads the chain first:
     a second run sends nothing. A recheck recorded on chain that the store cannot give, so
     `resolve` is not sent, fails the run with the reason.
   - **The pre-flight** (`check`, read-only, PASS/FAIL per line, exit 1 on any FAIL): the
     contract's registry is `deployment.json`'s; the recheck metric is registered with the
     manifest's recheck template and paired with its tolerance; the `.env` key is
     `OnyxSettlement.coordinator()` (`level2-manifest coordinator-address`: the address only);
     the markets, their close and deadline; a round from the close or now, whichever is later,
     fits the recheck days and the deadline; the pool without bettors and original observers holds at least the draw
     (provisional while trading is open); the original store and the receipt key exist; every
     signer's balance covers its gas (measured on the fork rehearsal, rounded up) at three times
     today's fee.
   - **Rehearsed** on a local anvil fork of Base Sepolia, end to end (`scripts/recheck-fork.sh
     $M 2026-10-28 "$E"`, about 15 minutes, not in check-all: it needs the network). It forks at
     the head with another chain id (845320, so nothing signed there is valid on Base Sepolia),
     mines past the close with no time between the blocks (the contract counts blocks; the
     fork's clock stays the wall clock, so the live coordinator's clock check holds and today is
     a recheck day), gives every signer the pre-flight names 0.01 test ETH there, then runs the
     pre-flight, the dry run, `run --yes` and `run --yes` again. **2026-11-15, market 1:**
     forked at block 59,142,037, 616,623 blocks mined; the pre-flight passed; the dry run sent
     nothing; the round drew 5 of the 11 (the observers fetched the real Daily Treasury
     Statement), all 5 agreed on 18463 and were receipted; `resolve` from miner-0: Yes won
     (recheck 18463, tolerance 0); miner-0 withdrew 19.8 tONYX (its 10 plus the No pool's 10 less
     the 2% fee, to the wei), `payTreasury` paid 5.2 tONYX (the open fee and the trading fee);
     the contract held exactly its liabilities; the second run sent nothing. `run --yes` took
     624 s on the fork (finality is 64 one-second blocks there; about 20 minutes on Base
     Sepolia). Gas: announceDraw 161,684, fixBeacon 50,424, five postReceipt 563,451,
     recordObservation 202,295 (the coordinator, 977,854), postCommit about 179,650 each,
     resolve 147,499, withdraw 97,394, payTreasury 53,314: 16 transactions, 2,174,282 gas. The
     script's budgets are these rounded up by about half. The fork differs from the live chain
     where THREATS CL7-26 says; the live pre-flight and dry run (2026-11-15, state `wait`) passed.
     **Faults, 2026-11-15 (after review H1, THREATS CL7-25;** `RECHECK_FORK_FAULT=interrupt`,
     `observers` or `lost`, each on its own fork): a SIGINT to the runner (by PID) once its coordinator had
     started stopped it before any draw (0 draws); the next run, after that window's commit close,
     found no draw in the book, dropped the saved window, picked a new one and resolved market 1
     with **one** draw. With every observer stopped (by PID) once the draw was opened, the round
     recorded no value (1 draw used, the run exited 1 and dropped its window and pool); the next
     run picked a new window, never the dead one, and resolved market 1 with **two** draws. With
     the coordinator's `announceDraw` dropped by the fork's proxy and the runner stopped while it
     waited for it (the book held draw 0, the chain none), the next run after that window's
     commit close resumed the held draw, found it never landed, released it and refused a new
     draw for the dead window ("no new draw ... nothing was sent", draws 0 -> 0); the run after
     took a new window and resolved market 1 with **one** draw. All three ended as the run
     above: Yes, miner-0 paid 19.8 tONYX to the wei, the treasury 5.2, a second run sending
     nothing (re-run 2026-11-15 after the re-review, each on its own fork).

   The commands the script runs, by hand (the reference; the values below are the
   deployment's own):

   ```
   cargo run -p onyx-level2 --example level2-manifest -- recheck-plan \
     --manifest sources/<id>/manifest.json --recheck 7:7 --record-date D \
     --rpc https://sepolia.base.org --corrections <OnyxCorrections> \
     --pool <the observer pool file> --original-store <the original's results store> \
     --out-pool recheck-pool.json [--margin-blocks N]
   ```

   It reads every market and bettor of the value from the contract's lists and prints
   `"state"`: `none`, `wait` (trading open; with `--pool`, a provisional pool too), `void-only`
   (no recheck day left, CL7-13) or `run` with `windowStart`, the markets it serves (a later
   close is left to void, CL7-R1-1) and the pool without bettors and original observers, written
   to `--out-pool`. Then run the round with the coordinator and observers as for any manifest
   round, each with `--recheck 7:7`, the coordinator with `--pool recheck-pool.json
   --window-start <windowStart>`. Until a recheck round is recorded the market voids after its
   deadline and every bet is refunded (and the open fee, since no recheck round was tried).
8. **Resolve and pay** (anyone, after the recheck is recorded): `level2-manifest recheck-resolve
   --manifest ... --recheck 7:7 --record-date D --rpc ... --corrections ... --store <the
   recheck's results store> --key-file F [--dry-run]` sends `resolve(marketId, recheckRoundId,
   recheckValue)` for every open market of the value (`--dry-run`: estimates only); then
   `level2-manifest recheck-payout ... --key-file F1 [--key-file F2 ...] --treasury-key-file F
   [--dry-run]` sends `withdraw(marketId)` for each key the contract owes and `payTreasury()`
   when the treasury is owed anything.
9. **The site** (done 2026-11-15 for market 1): the Markets page shows each market named in the
   `corrections` block of `apps/web/static/markets/base-sepolia.json`, read from the chain in the
   reader's browser and checked by `markets_view.rs`, with the note that the source is public, so
   a market mostly pays whoever checks it (ADR-0057 consequences). A new market needs its entry
   there with every field: `id`, `fact`, `metric`, `title`, `question`, `recordDate`, `source`,
   `sourceUrl`, `unit`, `scale`.

## Verify

- `cast call $ONYX_CORRECTIONS 'marketCount()(uint256)'` and `market(uint256)` show the market;
  `liabilities()` equals `cast call $ONYX_TONYX 'balanceOf(address)(uint256)' $ONYX_CORRECTIONS`.
- After a recheck, `/v1/observations/us.treasury.tga_total_deposits?subject=...` carries
  `recheck {result, valueHash, recordedAt, matches}` and `client::verify_recheck` accepts it.

## Roll back

Nothing to undo on the registry: a registered metric and a pair are append-only and harmless
without markets. To stop new markets, stop announcing them and leave the contract: open markets
still settle or void by their own rules (there is no pause, by design: no admin path, THREATS
CL7-14). A replacement is a new deployment; record both addresses.
