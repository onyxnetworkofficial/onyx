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

**Rehearsed:** locally by `scripts/corrections-anvil.sh` and `scripts/recheck-anvil.sh` (both in
check-all): the same contracts, the live manifest's real template ids, a market opened as step 6
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

7. **The recheck round** (THREATS CL7-3, CL7-20): plan it with the runner (built 2026-11-01,
   `onyx_level2::corrections`; the values below are the deployment's own):

   ```
   cargo run -p onyx-level2 --example level2-manifest -- recheck-plan \
     --manifest sources/<id>/manifest.json --recheck 7:7 --record-date D \
     --rpc https://sepolia.base.org --corrections <OnyxCorrections> \
     --pool <the observer pool file> --original-store <the original's results store> \
     --out-pool recheck-pool.json [--margin-blocks N]
   ```

   It reads every market and bettor of the value from the contract's lists and prints
   `"state"`: `none`, `wait` (trading open), `void-only` (no recheck day left, CL7-13) or `run`
   with `windowStart`, the markets it serves (a later close is left to void, CL7-R1-1) and the
   pool without bettors and original observers, written to `--out-pool`. Then run the round with
   the coordinator and observers as for any manifest round, each with `--recheck 7:7`, the
   coordinator with `--pool recheck-pool.json --window-start <windowStart>`. Rehearsed end to
   end on a local anvil (`scripts/recheck-anvil.sh`), not yet on Base Sepolia: until a recheck
   round is recorded the market voids after its deadline and every bet is refunded (and the open
   fee, since no recheck round was tried).
8. **Resolve** (anyone, after the recheck is recorded): `level2-manifest recheck-resolve
   --manifest ... --recheck 7:7 --record-date D --rpc ... --corrections ... --store <the
   recheck's results store> --key-file F` sends `resolve(marketId, recheckRoundId,
   recheckValue)` for every open market of the value; then each winner `withdraw(marketId)`;
   anyone `payTreasury()`.
9. **The site**: add the contract to `apps/web/static/markets/base-sepolia.json` and the reader
   (`markets_view.rs`) once a market exists (TODO S6, THREATS CL7-19), with the note that the
   source is public, so a market mostly pays whoever checks it (ADR-0057 consequences).

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
