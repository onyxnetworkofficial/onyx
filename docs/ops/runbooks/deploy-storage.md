# Runbook: deploy the storage network on Base Sepolia (ADR-0060, lead only)

**Trigger:** the lead decides to put the storage network live (ROADMAP S15). The live contract on
Base Sepolia since 2026-11-19 is v4 (ADR-0067, `RULES_VERSION` 4): `OnyxStorage`
`0x80833ee1b5f14349c6d41e75259974d29cd86e6b` (block 60983049) with its reader `OnyxStorageAnchor`
`0x69124747008fa162799226a31dc00f389c425c91`. Retired: v3, `0x9244eeaf5053f90ca2fbce157a3446f828c5ee43`
(block 53457522, 2026-11-02, S16, `RULES_VERSION` 3; the nodes left it and took their bonds back on
2026-11-19), and v0, `0x2423837572b58caf82896491ff6ee23080849419` (2026-11-01). All were deployed with
`scripts/storage-sepolia.sh` (it runs the steps below; state in `.onyx-testnet/storage-sepolia.json`).
The public endpoint answers `eth_getLogs` for at most 200 blocks per call (THREATS STORAGE-S15-1)
and HTTP 429 past its rate limit: the five nodes share one address, so each runs one pass a minute
and keeps what it read across failed passes (STORAGE-S15-4). Many nodes: use your own endpoint.
Rehearsal: `scripts/storage-network-anvil.sh`. Plain-English overview: docs/STORAGE-NETWORK.md.

**What it is:** one new contract, `OnyxStorage` (no proxy, no owner, every parameter immutable),
next to the deployed `OnyxSignedBook` (`0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5`, CL5) and a
tONYX test token. Storage nodes are onyx-node processes with `ONYX_STORAGE_*` set.

**Money:** tONYX (no value) for deposits, bonds and pay; test ETH for gas. Measured gas on anvil:
`createDeal` about 292,000, `register` about 106,000 + `activate` about 181,000, `answer` about
171,000 with 5 nodes (about 511,000 of execution with 128), `settleMissed` about 145,000,
`recordSeed` about 46,000. On v4 (2026-11-19, forge): `createDeal` about 308,000 and
`createDealInCatalog` about 348,000 with a path of 14 levels, both before calldata.

**Who:** the operator with the deployer key in `.env` (never printed, never committed). The
deployer gets no role in the contract. The treasury is an address the owner names (a multisig
before mainnet, ROADMAP H1). Each storage node operator uses its own fresh key.
**Time:** about 30 minutes for the contract, then about 10 minutes per storage node.

## Preconditions

1. A checkout of the commit to deploy with `scripts/check-all.sh` green; `forge`, `cast` in
   `~/.foundry/bin`.
2. `.env` holds `BASE_SEPOLIA_PRIVATE_KEY` and `BASE_SEPOLIA_RPC_URL`; load it with
   `set -a; . ./.env; set +a` in the shell that runs forge; never `cat` it.
3. A tONYX token on Base Sepolia (from `DeployMarkets`, or a fresh `OnyxTestToken` whose minter
   is the deployer) and its address.
4. The owner's parameters (docs/STORAGE-NETWORK.md "Cost model"). The script's defaults are
   placeholders: one-day epochs (43,200 blocks), 0.001 tONYX per GiB per copy per epoch, 0.01
   tONYX bond per declared GiB with a 10 tONYX minimum, 10% slash, 2 checks per deal per epoch,
   jail after 3 misses.

## Steps

1. Dry run (no transaction): from `contracts/`,
   ```sh
   ONYX_STORAGE_TOKEN=<tONYX> ONYX_SIGNED_BOOK=0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5 \
   ONYX_STORAGE_TREASURY=<treasury> \
   forge script script/DeployStorage.s.sol:DeployStorage --rpc-url "$BASE_SEPOLIA_RPC_URL"
   ```
   It refuses any chain but 84532 and 31337 and a token or book address without code. Override a
   parameter with its variable (`ONYX_STORAGE_EPOCH_BLOCKS`, `ONYX_STORAGE_PRICE_PER_GIB_EPOCH`,
   `ONYX_STORAGE_BOND_PER_GIB`, `ONYX_STORAGE_MIN_BOND`, `ONYX_STORAGE_SLASH_PPM`,
   `ONYX_STORAGE_CHALLENGES`, `ONYX_STORAGE_MAX_MISSES`).
2. Deploy: the same command with `--broadcast --slow`. Note the address and the deploy block.
3. Read back: `cast call <storage> 'params()(uint64,uint128,uint128,uint128,uint32,uint32,uint32,uint256)'`
   equals the parameters asked; `cast codesize <storage>` is under 24,576; `conserved()` is true.
4. Record the address, deploy block and parameters in the deployment manifest and in
   docs/STORAGE-NETWORK.md "Status".

## Each storage node

1. On the node's server, a fresh key file (0600): `umask 077; python3 -I -c 'import secrets;
   print(secrets.token_hex(32))' > /etc/onyx/storage.key`. Fund its address with test ETH and the
   bond in tONYX.
2. Register (bond, register, activate in a later block):
   `onyx-storage register --rpc "$RPC" --chain-id 84532 --contract <storage> --from-block <deploy block>
   --key /etc/onyx/storage.key --capacity-gib <usable GiB> --url https://<node host>/`.
   It prints `NODE_ID=0x…`.
3. Configure onyx-node (docs/ops/config.md "Storage network"): `ONYX_BULK_DIR` (the disk),
   `ONYX_STORAGE_CONTRACT`, `ONYX_STORAGE_NODE_ID`, `ONYX_STORAGE_KEY_FILE`,
   `ONYX_STORAGE_FROM_BLOCK`; raise `ONYX_BULK_QUOTA_GIB` and `ONYX_BULK_MAX_GIB` to the disk. The
   public URL must be HTTPS (peers refuse plain HTTP except on loopback). Start it.
4. Check: `onyx-storage status --rpc "$RPC" --contract <storage> --from-block <deploy block> --node <id>`
   lists the node and, once a deal exists, its shards for this and the next epoch.

## A first deal

1. The publisher's dataset is anchored (docs/ops/bulk-publish.md) and served by its own node.
2. `onyx-storage deal --rpc "$RPC" --chain-id 84532 --contract <storage> --from-block <deploy block>
   --key <publisher gas key> --dir <dataset dir> --r 3 --epochs <N> --source-url https://<publisher node>/`.
3. After two epochs: `onyx-storage epoch ... --epoch <start>` shows every challenge passed.
4. When the deals end: `scripts/storage-sepolia.sh close` closes each recorded deal (the unpaid
   deposit goes back to the payer), withdraws each node's earnings and prints `conserved()`.

## Redeploy (a new contract version)

1. Close the old deals (`scripts/storage-sepolia.sh close`) once they end.
2. Move `.onyx-testnet/storage-sepolia.json` aside (for example `storage-sepolia-v0.json`).
3. Build onyx-node, onyx-storage and onyx-publish from the commit and deploy the API with
   `scripts/deploy-api.sh` (a node refuses a contract whose `RULES_VERSION` it does not know).
4. `plan`, `deploy --yes`, `register`, `install`, `deal`. Run `fund --yes` only when a key is
   short. Done this way on 2026-11-02 (S16).
5. The old contract keeps each node's bond until that node runs `onyx-storage exit --node ID`
   and, three to four epochs later, `onyx-storage withdraw-bond --node ID` against it (node gas). On
   2026-11-02 the five v0 bonds (10 tONYX each, no value) were left there to save test ETH.
   From v3 on, take the bonds back first: "Move from v3 to v4" below.

## Move from v3 to v4 (ADR-0067 slice 5; run live 2026-11-19)

**What moves.** The five nodes leave `OnyxStorage` v3 (`0x9244…ee43`, rules 3) and join a new
`OnyxStorage` v4 (rules 4: deals on catalog-anchored datasets, format-2 manifests). v4 is two
contracts from one deployment: `OnyxStorage` and the stateless reader `OnyxStorageAnchor` that
its constructor creates. Every step below is one command of `scripts/storage-sepolia.sh`. Each
step can be run again: it reads the chain first and sends only what is still missing. A step
that sends needs `--yes`; without it, it prints what it would send and sends nothing. All of it
is tested with fake tools (`scripts/storage-sepolia-tests.sh`). It was run once against the
live chain, on 2026-11-19: "What the live run showed" below. The steps stay as written, for the
next move.

**The rule behind the order.** A node must never be on a contract's list, with a deal to answer
for, while no process can answer. A node of the new build follows rules 4 only: pointed at v3 it
sends nothing, and then misses v3's checks. `scripts/deploy-api.sh` replaces the one `onyx-node`
binary on the host that the storage units also start. A running unit keeps the old program until
it restarts, but a crash or a reboot would start the new one. So:

- the nodes leave v3 (step 2) **before** the new API is deployed (step 5);
- on v4, `install` (step 9) follows `register` (step 8) at once, and the first deal (step 11)
  comes only after every node's health shows the new node id.

**Why leaving v3 is safe.** v3 has two deals. Both ran for epochs 2 to 10 and were closed on
2026-11-02 (docs/STORAGE-NETWORK.md "Second live run"); v3 was at epoch 1032 on 2026-11-19. So
no node holds a shard that is still checked. The script does not trust this note: `exit-old`
reads `dealCount()` and every `dealInfo` and refuses to send while any deal on the contract is
not closed and not past its end. It does not work out which node holds which deal: with 3 copies
on 5 nodes every node holds shards of every deal, so one running deal blocks every exit. A node
that asks to leave is still on the list, and still asked, until the current epoch ends.

**Before you start.**

- A decision on v4's epoch length. It is fixed for the contract's life. The script's default is
  `STORAGE_EPOCH_BLOCKS=7200` (24 minutes). ADR-0067 open question 4 recommends 432,000 (one
  day): then set `STORAGE_EPOCH_BLOCKS=432000` for `plan` and `deploy --yes`, and choose
  `STORAGE_DEAL_EPOCHS` to match (the default 42 is then 42 days). `plan` prints the length.
- `onyx-node`, `onyx-storage` and `onyx-publish` built from the commit to deploy, with
  `scripts/check-all.sh` green.
- Test ETH: see "Gas" below. Top up with `scripts/testnet-faucet.py` if a key is short.
- `RPC=https://sepolia.base.org` in the shell for the `cast call` checks below (they read only).
  `V3=0x9244eeaf5053f90ca2fbce157a3446f828c5ee43`.

### Steps

| # | Command | Sends | Check after it |
|---|---|---|---|
| 1 | `scripts/storage-sepolia.sh close` | Only for a deal not yet closed or earnings not yet withdrawn | Every deal line says `already closed`; every node line says `nothing to withdraw` on a second run; the last line is `conserved: true` |
| 2 | `scripts/storage-sepolia.sh exit-old --yes` | One `requestExit` per node, from the node's key | `cast call $V3 'nodeInfo(bytes32)(address,uint8,uint64,uint256,uint32,uint64,uint64,uint64)' <node id> --rpc-url $RPC`: the 2nd value is 2 (exiting), the 6th is the epoch the node is off the list from. `cast call $V3 'activeNodes()(bytes32[])' --rpc-url $RPC` is empty |
| 3 | Wait 72 to 96 minutes (see "Waiting") | Nothing | `scripts/storage-sepolia.sh withdraw-old` (no `--yes`) says for each node how many epochs, blocks and minutes are left, and sends nothing |
| 4 | `scripts/storage-sepolia.sh withdraw-old --yes` | One `withdrawBond` per node | `nodeInfo`: the 2nd value is 4 (withdrawn), the 4th (bond) is 0. The last line: `conserved: true` and what v3 still holds. Expect about 5.2 to 5.7 tONYX (5.2e18 to 5.7e18 units): the treasury's part, that is the five registration fees and the slashes, less the reporters' rewards. Each node's tONYX is back in its own address |
| 5 | `scripts/storage-sepolia.sh archive-old` | Nothing (it changes the state file only) | It prints `archived N entries of 0x9244… under old_v3`. A second run says `already archived` |
| 6 | Deploy the API from this commit: `scripts/deploy-api.sh <export dir> <onyx-node binary>` (docs/ops/deployment.md) | Nothing on chain | The public node is healthy. `GET https://<domain>/v1/bulk/manifests/<root>` of a catalog dataset has a `catalog` field. The storage units still run the old program; `storage.tick_failing` may alert if one restarts before step 9 (it then refuses v3: `lastErrorClass` is `contract_version`). That is harmless: the nodes have left v3 |
| 7 | `scripts/storage-sepolia.sh plan`, then `scripts/storage-sepolia.sh deploy --yes` | One deployment from the deployer (both contracts) | It prints `OnyxStorage <address> at block <N> (23098 bytes); conserved: true; RULES_VERSION: 4` and `OnyxStorageAnchor <address> (4646 bytes)`. Check: `cast call <v4> 'RULES_VERSION()(uint8)' --rpc-url $RPC` is 4; `cast call <v4> 'anchor()(address)' --rpc-url $RPC` is the printed reader; `cast call <v4> 'params()(uint64,uint128,uint128,uint128,uint32,uint32,uint32,uint256)' --rpc-url $RPC` starts with the epoch length you chose |
| 8 | `scripts/storage-sepolia.sh fund --yes` | From the deployer: test ETH up to the target and tONYX up to 20 per node, 100 for the payer | Each printed node line has `tONYX=20000000000000000000`. The step fails if a node is still below the bond |
| 9 | `scripts/storage-sepolia.sh register`, then at once `scripts/storage-sepolia.sh install` | Per node: approve, `register`, `activate`. `install` sends nothing on chain: it writes each unit's settings (contract, node id, deploy block) and restarts the five units | `register` prints `NODE_ID=0x…` for each node. `cast call <v4> 'activeNodes()(bytes32[])' --rpc-url $RPC` lists five ids; `nodeInfo` gives status 1 and a bond of 9.8 tONYX (10 less the 2% fee). `install` ends with `nN answers at …` for each node. Then `GET https://<domain>/storage/nN/v1/health`: `storage.node` is the new id, `storage.lastTickOk` is true, `storage.lastErrorClass` is null, `storage.logsReadThrough` climbs |
| 10 | Record the deployment (see "After the deployment") | Nothing | `bash scripts/check-docs.sh` passes |
| 11 | `scripts/storage-sepolia.sh deal-catalog --yes <root>` (see "The first catalog deal") | From the payer: one approval, one `createDealInCatalog` | It prints `DEAL_ID=0 … anchor=catalog-<number> … epochs=<start>..<end>`. `cast call <v4> 'dealInfo(uint64)(address,bytes32,bytes32,bytes32,uint32,uint8,uint64,uint64,uint64,uint256,uint256,bool)' 0 --rpc-url $RPC`: the 2nd value is the root |
| 12 | Watch three epochs from the deal's start (see "What to watch") | The nodes' answers | Every check passed, nothing slashed, `conserved()` true on v4 and on v3 |

### Waiting (blocks of 0.2 s)

- **Step 3, the bond.** v3 has epochs of 7,200 blocks (24 minutes). A node that asks to leave in
  epoch E is off the list from E + 1, and `withdrawBond` needs an epoch above E + 1 +
  `SETTLE_EPOCHS` (2), so E + 4 at the earliest: three whole epochs plus the rest of epoch E.
  That is 72 to 96 minutes. (ADR-0067 section 5 said 48 minutes; the contract says otherwise.)
  `withdraw-old` reads `SETTLE_EPOCHS()`, `params()` and the head block, so it gives the exact
  figure.
- **Step 7.** One transaction: under a minute.
- **Step 9.** Three transactions and one block's wait per node: about a minute per node through
  the public endpoint. `install` waits up to 2 minutes per node for its health.
- **Step 11.** `deal-catalog` reads the contract's logs from the deploy block, 200 blocks a call:
  90 calls for each hour since step 7. Do it on the same day.
- **Step 12.** A deal made in epoch E is first checked in epoch E + 2. With 7,200-block epochs:
  24 to 48 minutes to the first checks, then three epochs, so about 2 hours in all. With
  432,000-block epochs: 1 to 2 days, then 3 days.

### Gas (rough; price about 0.006 gwei, as seen on 2026-11-03)

| Step | Paid by | Transactions | Gas | Test ETH |
|---|---|---|---|---|
| 1 `close` | payer, nodes | 0 when all is closed | 0 | 0 |
| 2 `exit-old` | each node | 5 | about 600,000 (not measured: an estimate of 120,000 each) | 0.0000036 |
| 4 `withdraw-old` | each node | 5 | about 350,000 (not measured: an estimate of 70,000 each) | 0.0000021 |
| 7 `deploy --yes` | deployer | 1 | 6,111,928 (measured on a local chain) | 0.000037 |
| 8 `fund --yes` | deployer | up to 12 | about 450,000 | 0.0000027, plus the test ETH it hands out |
| 9 `register` | each node | 15 | about 1,670,000 (approve 46,000, `register` 106,000, `activate` 181,000, times 5) | 0.00001 |
| 11 `deal-catalog` | payer | 2 | about 433,000 (approve 46,000, the deal 386,800) | 0.0000026 |
| 12 each epoch | nodes | about 7 | about 1,070,000 (one seed, six answers) | 0.0000064 |

The move without the watching is about 9.6 million gas: about 0.00006 test ETH at this price,
ten times that if the price is ten times higher. Each node pays about 520,000 gas of it (about
0.0000031 test ETH); on 2026-11-19 each node held enough. `fund --yes` also **hands out** test
ETH: it tops each node up to 0.00012 and the payer up to 0.00003, so up to 0.00063 from the
deployer if the keys were empty. Set `STORAGE_FUND_NODE_WEI=0 STORAGE_FUND_PAYER_WEI=0` to hand
out none and top up tONYX only. Budget as in ADR-0067: 0.0003 test ETH, more if `fund` tops up
ETH.

### The bond on v4

`onyx-storage register` reads `bondFor(capacity)` from the contract and approves exactly that:
0.01 tONYX per GiB, at least 10 tONYX, so 10 tONYX for a node of 6 GiB. 2% of it is the
registration fee. The bond comes from the node's own address. The nodes were slashed on v3, so
three of them get less than 10 tONYX back in step 4 (8.82, 7.938 and 7.938). `fund --yes`
covers the gap: the deployer mints each node up to 20 tONYX, whatever it holds. `fund` refuses
to run if its target were below the bond, and fails if a node is still short after it.
`register` checks each node's tONYX against the bond before it sends that node's approval.

### The first catalog deal

1. List small datasets on the public node: `GET https://<domain>/v1/bulk?limit=100`. Each row
   has `publisher`, `dataset`, `version`, `root`, `bytes` and `shards`.
2. Pick a row with a low `version` (well under 64: say under 30), few `shards` and `stored_shards`
   equal to `shards`. A storage node takes versions in order: it fetches the deal's version and
   every earlier one back to version 1, at most 64 steps (ADR-0067 open question 6). A dataset
   that changes every day passes 64 in about two months, so prefer one that changes seldom.
3. Check that a catalog anchors it and that the node proves it:
   `GET https://<domain>/v1/bulk/manifests/<root>` must have a `catalog` field (`number`, `root`,
   `header`, `leaf`). A dataset with its own report has none; use `deal` with its folder for
   that.
4. `scripts/storage-sepolia.sh deal-catalog --yes <root>`. The root is `0x` and 64 lowercase hex
   digits. The tool checks the proof with the contract's rules and asks the book before the
   approval. If it stops with `CatalogSuperseded`, the publisher's next daily catalog landed and
   the node does not hold it yet: wait until the node has it, then run the same command again.
   Nothing was sent.
5. The deposit is 0.001 tONYX per GiB (at least one), per copy, per epoch: 0.126 tONYX for 3
   copies and 42 epochs of a dataset under 1 GiB.

### What to watch (three epochs from the deal's start epoch)

- `scripts/storage-sepolia.sh status`: every node active, its bond unchanged, no misses; the
  epoch section lists this epoch's checks.
- `onyx-storage epoch --rpc $RPC --contract <v4> --from-block <deploy block> --epoch <E>` for each
  of the three epochs, once it has ended: 6 checks (2 a deal, 3 copies), all passed, nothing
  slashed.
- Each holder's `GET https://<domain>/storage/nN/v1/health`, object `storage`: in the first epoch
  `wanted` equals `held` (the node fetched its shards; `fetchFailed` is 0); in each epoch
  `answered` is above 0 on a node that was asked; `answerFailed` and `unanswerable` stay 0;
  `lastTickOk` is true. One node records the seed in the first 51 seconds of each epoch
  (`seeds`): an epoch without a seed has no checks and no pay.
- The health watch stays quiet: `storage.up`, `storage.tick_age`, `storage.tick_failing`,
  `storage.answers` (docs/ops/runbooks/health-watch.md).
- Rows from a storage node check: fetch rows of the dataset from
  `https://<domain>/storage/nN/` and run `onyx-check --bulk` on them (docs/ops/bulk-publish.md).
- `cast call <v4> 'conserved()(bool)' --rpc-url $RPC` and the same on `$V3`: both true.
- When the deal ends: `scripts/storage-sepolia.sh close`.

### After the deployment (step 10)

1. **docs/STORAGE-NETWORK.md, section 13.** Add the v4 line in the same form as the v3 one:
   `` `OnyxStorage` `0x<v4 address>` at block <deploy block> `` with the date, the commit,
   `RULES_VERSION` 4, both code sizes, the reader's address, the parameters and the deploy cost.
   Mark v3 retired there and at the top of this runbook.
2. **The early record's contract list** is `deploy/early-record/contracts.json` in this
   repository (ADR-0065). Done in the repository on 2026-11-19 (ADR-0065 amendment 1): the old
   file is kept byte for byte as `contracts-1.json` (every published day names its hash), and
   `contracts.json` is list 2, in force from 2026-11-20, with the v3 entry and this one:
   ```json
   {"name": "OnyxStorage (v4)", "abi": "storage-v4", "address": "0x80833ee1b5f14349c6d41e75259974d29cd86e6b", "doc": "docs/STORAGE-NETWORK.md", "scan_from": 60983049, "scan_from_is": "deploy", "block_doc": "docs/STORAGE-NETWORK.md", "block_quote": "`OnyxStorage` `0x80833ee1b5f14349c6d41e75259974d29cd86e6b` at block 60983049", "why": "The deploy block, stated with the address."}
   ```
   `storage-v4` counts the same five events as `storage-v3` (`Activated`, `DealCreated`,
   `Answered`, `Slashed`, `MissReward`: their shapes did not change in v4). `OnyxStorageAnchor`
   needs no entry: it has no events, no state and no roles. **The daily run counts v4 only once
   its own tool and checkout are updated**: docs/ops/early-record.md "Changing the contract
   list" has the steps and the day by which to do them. Until then it goes on with list 1 and
   publishes each day without the v4 rows; the first version of list 2 then reads v4 from its
   deploy block, so no work is lost.
3. **Source verification.** The script does not verify source. By hand, from `contracts/`, with
   an explorer that takes it (not tried): `forge verify-contract <v4 address>
   src/OnyxStorage.sol:OnyxStorage --chain 84532 --constructor-args $(cast abi-encode
   'c(address,address,address,uint64,uint128,uint128,uint128,uint32,uint32,uint32)' <the ten
   values in the order of params()>)` and `forge verify-contract <reader address>
   src/OnyxStorageAnchor.sol:OnyxStorageAnchor --chain 84532` (no constructor arguments).
4. The node's settings need no hand edit: `install` wrote the new contract, node id and block.
   The node's saved log file is named by contract address, so v3's and v4's do not mix. The v3
   shard files stay on disk: never delete shard files by hand.

### What the live run showed (2026-11-19)

Nothing differed from the order of the steps above.

1. **The wait (step 3) was 89 minutes.** The nodes asked to leave in epoch 1041 (five
   transactions) and their bonds were free from epoch 1045. That is inside the 72 to 96 minutes
   given above.
2. **The bonds came back (step 4):** 9.8, 9.8, 8.82, 7.938 and 7.938 tONYX. `conserved()` was
   true.
3. **The treasury's part stays in v3.** After every node withdrew, v3 still holds 5.2336 tONYX:
   the registration fees and the slashes, less the reporter's rewards. That is inside the 5.2
   to 5.7 expected in step 4. No step of this runbook moves it.
4. **The deployment (step 7)** was one transaction: `OnyxStorage` 23,098 bytes, `OnyxStorageAnchor`
   4,646 bytes, `RULES_VERSION` 4, epochs of 7,200 blocks. It was estimated at 7.9 million gas and
   0.000087 test ETH (at a highest fee of 0.011 gwei) before it was sent. The gas table above
   says 6.1 million gas (measured on a local chain) and 0.000037 test ETH at 0.006 gwei: plan
   for the higher figure.
5. **What else was recorded.** `close` found both deals closed.
   `archive-old` moved 23 entries under `old_v3`. After step 6 the public node's manifest answer
   for a catalog dataset carried the `catalog` field. `fund --yes` left each node key at 0.00012
   test ETH and 20 tONYX and the payer at 0.00003 test ETH and 100 tONYX. After step 9 all five
   nodes were active with new node ids and each health answer had `lastTickOk` true.
6. **The deal (step 11)** is deal 0, through the publisher's catalog 16. The tool asked the
   book before the approval.
7. **The first epoch of checks (step 12):** 6 of 6 passed, nothing slashed. `onyx-storage epoch`
   answered in seconds through the public endpoint. The later epochs are in
   docs/STORAGE-NETWORK.md "Third live run".

Not done in that run: source verification (it needs an account on a block explorer); `close`,
which waits for the deal's end; the roll-out of the early record's second contract list
(docs/ops/early-record.md "Changing the contract list").

### Going back

| You are after | What is changed | How to go back |
|---|---|---|
| Step 1 | Ended deals are closed, earnings are paid out | Nothing to undo: both only pay out what was owed |
| Step 2 (`exit-old`) | The nodes are off v3's list. **This cannot be undone**: a node id that left cannot rejoin | No deal is open on v3, so nothing is lost by being off it. To serve v3 again a node must register again (a new id, a new 2% fee): take the bond back (step 4), then `onyx-storage register` against v3 by hand, set the new id in the unit's settings, and keep the old program on the host. The script will not do this: it holds the old ids |
| Steps 3 and 4 | The bonds are back in the nodes' addresses | As after step 2 |
| Step 5 (`archive-old`) | The state file: v3's entries sit under `old_v3` | Move them back to the top level by hand. Nothing is deleted, nothing was sent |
| Step 6 (new API) | The public node and the program the storage units start | The API's own rollback (docs/ops/launch-checklist.md "Rollback"). The storage units need nothing while they are on no contract |
| Step 7 (`deploy --yes`) | A v4 contract exists; no node, no deal | A contract cannot be removed, and an unused one costs nothing. A wrong one (for example the wrong epoch length): `STORAGE_OLD_RULES_MAX=4 scripts/storage-sepolia.sh archive-old` moves it under `old_v4`, then `plan` and `deploy --yes` again |
| Step 9 (`register`, `install`) | The nodes are on v4's list (bond 9.8 tONYX each, fee paid) | Leave v4 the same way: `STORAGE_OLD_RULES_MAX=4` with `exit-old --yes`, then `withdraw-old --yes` after the wait, then `archive-old`. Without that variable the three steps refuse a rules-4 contract, so they never empty the live one by mistake |
| Step 11 (the deal) | A deal runs until its end epoch; its deposit is in the contract | A deal cannot be stopped. Keep the nodes answering until it ends, then `close` returns what was not paid out. `exit-old` refuses while it runs |

If a step fails half way (a send that was not mined, a read that failed), run the same command
again. It reads each node's state from the contract, skips what is done and sends the rest. If
it says a transaction is `not yet mined`, wait for it and run again.

## Rollback

There is no admin path. To stop: no new deals; nodes `exit` (bonds back three to four epochs later); running
deals end at their last epoch and `close` refunds the rest. A broken contract is replaced by a new
deployment, never upgraded.
