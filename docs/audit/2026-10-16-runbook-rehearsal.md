# Launch runbooks: rehearsal on a local fork (ROADMAP L15, LC-19)

Date: 2026-10-16. Branch `runbooks` from main `7b9d4e4`. Tools: anvil 1.8.5 forking
https://sepolia.base.org (read-only; every transaction stayed in the fork's memory), cast 1.8.5,
`onyx-node` and the `demo-local` driver built in release from this branch, Python 3 (standard
library). Keys: anvil's public dev account 0 (deployer, coordinator, OnyxRevenue owner) and the
driver's per-actor keys made in a scratch directory; no `.env` was read, nothing was sent to Base
Sepolia, `scripts/demo-testnet.sh testnet` was not run.

Runbooks: [stop-serving-shard.md](../ops/runbooks/stop-serving-shard.md),
[rotate-reporter.md](../ops/runbooks/rotate-reporter.md),
[redeploy-new-ledger.md](../ops/runbooks/redeploy-new-ledger.md),
[release-source-terms.md](../ops/runbooks/release-source-terms.md).

## Result

| Runbook | Rehearsed | Outcome |
|---|---|---|
| (a) stop serving a shard | yes | Shard 404 `unknown_shard`, its market 404 `unknown_market`, `servable` 2 to 1, `notHeld` 0 to 1; rollback served it again on the first poll |
| (b) rotate the reporter | yes | `setReporter` status 1 with `ReporterSet`; old key refused at node start (exit 1) and on chain (`NotReporter`); a paid request after rotation settled from the new key |
| (c) redeploy, new ledger id | yes (fork deploy and node switch; not the testnet deploy or the announcement edits) | New ledger id, node on the new contract, old state dir refused, old roots `verifyRow` true at the old address; rollback to the old ledger worked |
| (d) source terms release step | yes (script) | Exit 1 today: Binance's site terms (S8) were never readable; everything else read 2026-10-14 |

Two code changes came out of the rehearsal (each with a unit test, below): the node now refuses
to start with an operator key that is not `OnyxRevenue.reporter()`, and `onyx-node
--check-config` prints each catalog entry's shard id and the operator key's address.

## Why the code changed

- **Shard ids.** Runbook (a) needs the catalog entry of a shard id. Held entries carry the Borsh
  spec, not the id, so with two shards of one metric an operator could not tell which to remove.
  `--check-config` now prints `catalog held[i] metric M shard 0x...` (and `shards[i] round R
  ...`); test `catalog_file::tests::entry_ids_name_each_listed_and_held_shard`.
- **Reporter key.** Runbook (b) must show which key the node holds without printing it, and a node
  started with the wrong key would have every settle and credit revert `NotReporter`
  (`OnyxRevenue.settle` and `credit` are `onlyReporter`). `--check-config` prints `operator key
  address 0x...`; a start with payments on reads `reporter()` and stops with both addresses on a
  mismatch; test `app::tests::the_start_refuses_an_operator_key_that_is_not_the_reporter`.

## Setup

```sh
W=<worktree>; R=<scratch dir outside the repo>
export CARGO_TARGET_DIR=$W/target
systemd-run --user --scope -q -p MemoryMax=12G cargo build -j 4 --release --locked \
  -p onyx-node -p onyx-source-btc --bin onyx-node --example demo-local
anvil --fork-url https://sepolia.base.org --port 18900 --accounts 1 --silent --config-out $R/anvil.json &
# deployment 1 (fork block 48209362 at start)
ONYX_DEMO_DRIVER=$W/target/release/examples/demo-local ONYX_DEMO_ANVIL_CONFIG=$R/anvil.json \
  ONYX_DEMO_RPC_PORT=18900 ONYX_DEMO_API_PORT=18910 ONYX_DEMO_KEYS=$R/keys1 \
  ONYX_LIVE_DIR=<main checkout>/fixtures-live/node ONYX_RESERVE_DIR=<main checkout>/fixtures-live/tail \
  systemd-run --user --scope -q -p MemoryMax=12G scripts/demo-testnet.sh fork --export-node $R/export1
cast rpc evm_setIntervalMining 2 --rpc-url http://127.0.0.1:18900
(set -a; . $R/export1/node.env; set +a; exec $W/target/release/onyx-node)   # 127.0.0.1:18911
```

Deployment 1: `REHEARSAL PASSED`, 8 min 25 s wall time under load (driver: 152 transactions,
94 s of chain time). Settlement `0xa230cddf6487c515464591dc2f13884145c2a318`, OnyxRevenue
`0xc38de02fc864b4c04214c6d47eb1f484b407a3d4`, deploy block 48209365. Held shards: round 1
`btc.entity-flow` `0xd129...1878` (root `0x9978...727e`) and round 3 `btc.entity-reserve`
`0xeb90...e7e9` (plain onyx-node refuses the reserve shard with 403
`serving_check_unavailable`, as designed; it needs onyx-node-btc). The node served both shards
after its second poll. Payments `self-settle`; reporter = the export's `operator` key
`0x55d4...a973`; owner = treasury = anvil account 0.

The runbooks are written for the systemd host (`/etc/onyx`, `sudo`, `systemctl`); the rehearsal
ran the same steps on the export directory (`node.env` in place of `/etc/onyx/node.env`, SIGTERM
and a new process in place of `systemctl restart`). The `onyx_check` helper was run in that local
form (sourcing the export's `node.env`, `env "$@"` overrides), including the
`ONYX_CATALOG=<other file>` override. Not run: `sudo`, `systemctl`, `journalctl`, the
`/var/lib/private/onyx` paths, `cast send --interactive`.

## (a) Stop serving a shard

Before: `/v1/proofs/1/0xd129...1878/0` 200, `/v1/export/1/0xd129...1878` 200,
`/v1/settlement/okx-net-flow-demo` 409 (`proof_level_not_accepted`: the demo market is below the
level markets settle on), `servable` 2.

1. `--check-config` printed `catalog held[0] metric btc.entity-flow shard 0xd129...1878` and
   `held[1] metric btc.entity-reserve shard 0xeb90...e7e9`, the same ids the driver's summary
   printed.
2. Backup copied; the runbook's Python step printed `removed held[0] (btc.entity-flow) and 1
   market(s) on 0xd129...1878`; `--check-config` on the result: `1 held shard(s)`, only `0xeb90...`.
3. SIGTERM: `stopped (drained: requests and settles finished, no unknown outcomes)`; restart.
   The log named only round 3 as served.
4. After: proof 404 `{"error":{"code":"unknown_shard","message":"no such shard"}}`, export 404
   (same), market 404 `unknown_market`; `/v1/health` `indexer`: `servable` 1, `notHeld` 1,
   `events` 2, `removed` 0; the shard's own status `not_held`.
5. Takedown step: `getFinalizedRoot(1, 0xd129...1878)` on the settlement returned
   `0x9978...727e`; the file `cas/rows/99789e60...727e` was moved to a 0700 directory.
6. Rollback: rows moved back, backup catalog restored, restart: `indexer: round 1 shard
   0xd129...1878 ... serving it` within 1 s of the start (the indexer state file already had the
   event), proof 200 again.

## (b) Rotate the reporter

1. Roles on the fork: `reporter()` `0x55d4...a973` (the node's key), `owner()` = `treasury()` =
   `0xf39f...2266` (anvil account 0).
2. New key: `umask 077; openssl rand -hex 32 > operator-new.key` (mode 600, 65 bytes with the
   newline); `--check-config` with `ONYX_OPERATOR_KEY_FILE` pointing at it printed `operator key
   address 0xd4aa2874e926864d9771fdabd8eb128e71ce9470`.
3. Gas: `cast send --unlocked --from <owner> <new> --value 0.01ether` status 1.
4. Drain: `receiptsPending` 0, `intentsOpen` 0, `reporter.inFlight` 0; SIGTERM, drained.
5. `cast send --unlocked --from <owner> <revenue> 'setReporter(address)' <new>`: status 1, tx
   `0x4aee3eb7...8bb8`, block 48210272, one log with topic `0x4f9f7d6f...2cef`
   (= `cast keccak 'ReporterSet(address)'`) and data = the new address. `reporter()` then returned
   `0xd4aa...9470`. The same call from the old key reverted `NotOwner` (`0x30cd7471`).
6. Node started with the **old** key: exit 1 in under 1 s, `ONYX_OPERATOR_KEY_FILE holds the key
   of 0x55d4...a973, but OnyxRevenue.reporter() is 0xd4aa...9470: install the reporter's key, or
   have the owner call setReporter (docs/ops/runbooks/rotate-reporter.md)`.
7. Old key copied aside, new key installed (0600); start: `operator key 0xd4aa...9470 is
   OnyxRevenue's reporter`; `/v1/health` `acceptingPayments` true, reporter `lastError` null.
8. `cast call --from <old key's address> <revenue> 'registerObservation(bytes32)' 0x..01`:
   reverted with `0xe5ce9c09` (`NotReporter`).
9. Paid request after the rotation (run during (c)'s rollback, on this same ledger): with the
   catalog's `freshWindowSecs` stretched to 10 years so the old rows are priced (rehearsal only),
   an unpaid GET answered 402; the smoke client (`tests/smoke_client.rs`, payer = the run's
   `customer` key holding 2,500 test USDC units) got `HTTP/1.1 200 OK` after 1.26 s. OnyxRevenue's
   USDC went 6,250 to 8,750 (+2,500, the row price); the new reporter's nonce went 0 to 1; the
   old reporter's nonce stayed 4. `settles.intentsOpen` 0, `unknownOutcomes` 0.

## (c) Redeploy and announce a new ledger id

1. Deployment 2 on the same fork (`ONYX_DEMO_API_PORT=18920`, keys in `keys2`, automine on for
   the deploy): `REHEARSAL PASSED` (driver: 131 transactions, 118 s of chain time). Settlement
   `0xd89a868d4df11ac06d6ed511d755e3f9ebf03a15`, OnyxRevenue
   `0x1a41a2bca5f98ec165f3b42a210cc604fb962287`, deploy block 48210286. Same data, so the same
   shard ids and roots; different settlement code hash (`0x3a74...e53e` vs `0x31d9...266e`).
2. Ledger ids: old `0xee6d8d3e...5b03`, new `0x74f12168...0504`. Coordinator of both: anvil
   account 0 (the rehearsal reused the deployer; a leaked-key redeploy would not). New revenue:
   owner = treasury = anvil account 0, reporter = `keys2/operator.key` (`0xc3a8...b05b`),
   `ledger()` = the new settlement.
3. Old manifest and catalog archived as `ops-store/ledgers/84532-<old settlement>.json` and
   `.catalog.json`.
4. Drain 0/0/0, SIGTERM, drained. `--check-config` on deployment 2: `settlement 0xd89a...3a15`,
   2 held shards, `operator key address 0xc3a8...b05b`.
5. New manifest with the **old** state dir: exit 1, `indexer state .../export1/state/indexer.json:
   written for another chain, contract or deploy block (use a new state file)`.
6. New manifest, new state dir, same listen port: round 1 served 16 s after the start (fresh
   indexer state, scanning from block 48210286); `/v1/health` `chainId` 84532, `contract`
   `0xd89a...3a15`, `servable` 2. A proof response's `verify.contract` was `0xd89a...3a15`.
7. Old roots: on the old settlement `getFinalizedRoot(1, 0xd129...1878)` returned root
   `0x9978...727e`, binding `0x5e07...3bd6`, finalized at 48209652; `verifyRow` with the served
   row and sibling returned `true`; the same row with its last byte changed returned `false`. The
   new settlement returned the same root, finalized at 48210573, `verifyRow` `true`.
8. Health watch (`--once`, overlay with chain 84532, the anvil deployer and the new OnyxRevenue):
   exit 0, 28 checks ok and 2 skipped (no site); `balances.revenue` 6,250 = 7,500 - 1,250 on the
   new contract.
9. Rollback: node stopped (drained), started on the archived old manifest and old state dir:
   `contract` `0xa230...a318` again, round 1 served at once.

Not rehearsed: the Base Sepolia deploy (`demo-testnet.sh testnet`), the repository edits and
notice of the "Announce" steps, the website rebuild.

## (d) Source terms release step

`python3 -I scripts/check-source-terms.py` on 2026-10-16: exit 1. Every served source with a URL
is `ok` at 2 days old (read 2026-10-14), S1, S2, S12 `ok` as consensus data, S3 `ok` via S5 and
S6; `UNREAD S8` (Binance's site terms, never readable by fetch) and `FAIL S4` (label set v2
includes S8). `--today 2027-01-13` (91 days later): every URL row `STALE`, exit 1. `--lint`
exit 0; `--self-test` 17 cases ok (stale, future, 90 days passes and 91 fails, unread, not
recorded, missing or extra source id, `via` a failing or unserved row, bad values, no threshold,
no section).

## Cleanup

The node and anvil were stopped (SIGTERM; the node logged `stopped (drained: ...)` each time).
Keys, exports and logs stayed in the session scratch directory outside the repository.

## Review (fresh reviewer, 2026-10-16, after merging main `825efd7`)

Second, independent run of runbook (b) step by step on a new anvil fork of Base Sepolia
(`demo-testnet.sh fork --export-node`, 154 transactions, `REHEARSAL PASSED`; same release
`onyx-node`), in the export-directory form described above: every command and expected output
of steps 1-10 matched (`0 0 0` drain, `stopped (drained: ...)`, `status 1` with the
`ReporterSet` topic and data = `NEW`, `NotOwner` `0x30cd7471` from the old key, `reporter()` =
`NEW`, old key refused at start in 0.1 s with exit 1, new key `is OnyxRevenue's reporter`,
health `True` with `lastError: None`, `registerObservation` from `OLD` reverted `0xe5ce9c09`).
The rollback (setReporter back to `OLD`, old key file back) started with `OLD` as reporter.
Also checked: payments `off` with no key starts and serves (no reporter read); an RPC that
refuses connections stops the start with exit 1 after 32 s (the earlier settlement read, 8
tries); every read is bounded by the 10 s RPC timeout.

Findings, all fixed on this branch:

| # | Where | Finding | Fix |
|---|---|---|---|
| RB-1 | redeploy step 8 | `verifyRow` used index `0` and an undefined `$LEAFS`: cast failed (`expected at least one digit`) | Step 2 saves one old proof; step 8 reads round, shard, row, index, leaf count and siblings from it; run on the fork: root, `true`, and `false` for a changed byte |
| RB-2 | redeploy step 6 / rollback | The old ledger's operator key was overwritten with no copy, so the rollback's "reinstall the old key" had nothing to reinstall | Step 6 copies it to `operator.key.84532-$OLD` first (shred at once if it leaked); rollback is now a command list |
| RB-3 | redeploy step 5 | The drain one-liner crashed (`TypeError`) on a node with payments off (`reporter` is null) | `(h["reporter"] or {}).get("inFlight", 0)` |
| RB-4 | stop-serving step 4 | The catalog edit raised `KeyError` on a catalog with no `markets` (optional in the schema), and only handled `held` although the page says to do the same to `shards` | `LIST=held` or `shards`; markets optional; run on the fork's catalog with and without `markets` |
| RB-5 | all three | `scripts/health-watch.py --once` without `--node`/RPC alerts (`require`), and after a redeploy it reads the old `revenue` address | Point at health-watch.md step 1; redeploy step 9 passes an overlay with the new `revenue` (checked with `--print-config`) |
| RB-6 | check-source-terms.py | A `via` row counted as read before its references were judged: `S3 via S3` passed (exit 0) and a chain `via` a failing `via` row reported `ok` | `via` rows are covered only by rows read directly; two self-test cases added (19) |
