# Running a storage node (ADR-0060)

A storage node is an onyx-node with a bulk store and the storage settings. It holds the shards the
placement gives it, serves them, proves them on chain each epoch, and is paid for every passed
proof. Test network only. Overview: docs/STORAGE-NETWORK.md; rules: docs/spec/storage-network.md.

## What you need

- A disk for the shards (`ONYX_BULK_DIR`); declare at most what it holds.
- A fresh key file (0600) for the node's operator, with test ETH for gas and tONYX for the bond.
- A public HTTPS URL for the node (peers and readers fetch from it).

## Commands (`onyx-storage`)

| Command | What it does |
|---|---|
| `register --capacity-gib N --url URL` | Approves and posts the bond, registers, activates in a later block; prints `NODE_ID`. On a contract with ADR-0060 amendment 1, 2% of the bond is a registration fee, never returned |
| `status --node ID [--since-block N] [--shards yes\|no]` | Epoch, the node's bond, misses, status, shards now and next epoch, earnings. `url` is filled only with `--since-block`; the shards are read without asking only while the open deals have at most 1,024 shards (see "What the commands read") |
| `epoch [--epoch E]` | Every challenge of the epoch: who, which shard, passed, failed, missed, paid, slashed |
| `withdraw` | Earnings to the operator |
| `exit --node ID` | Leave the list from the next epoch (still asked in that epoch) |
| `withdraw-bond --node ID` | The bond back, once the epoch is more than two past the one the node left the list in: three to four epochs after `exit` |
| `settle [--epoch E]` | Report the misses of an ended epoch (any keeper may; with amendment 1 the reporter earns 10% of each slash, unless it reports its own node) |
| `books [--replay yes]` | Every flow of the contract and the conservation check; `--replay yes` also rebuilds the totals from the contract's events (one log call per 200 blocks: for a local chain or a short history) |
| `declines` | Every node and every deal with its declines (`shard_declines` and `whole_deal_declines`, one per shard or deal and node), the declines whose one-epoch notice set when the node leaves, and its `slashes`, from the contract's events (ADR-0060 amendment 2 item 1): a node that declines much more than others with the same deals stands out |
| `read --deal N --from K --to K [--since-block N]` | Read rows from the replicas, every row checked, with fail-over |
| `deal --dir DIR --r 3 --epochs N --source-url URL` | (publisher) pay for storage of a manifest that has its own report (`DIR/report.bin`) |
| `deal --dir DIR --catalog FILE --r 3 --epochs N --source-url URL` | (publisher) the same for a manifest that a catalog anchors (ADR-0067): `FILE` is the catalog from `onyx-publish catalog-build`, its report is `FILE.report` |
| `deal --root 0x… --node URL --r 3 --epochs N --source-url URL` | (anyone) the same from a node's answer to `GET /v1/bulk/manifests/{root}`: its own report, or the catalog header, leaf proof and catalog report |

Every command takes `--rpc URL --contract 0x… [--from-block N] [--log-span N]`; sending ones also
`--chain-id N --key FILE` (it refuses any chain but 31337 and 84532).

**A deal through a catalog.** The tool checks the proof with the contract's own rules before it
sends anything, and refuses a catalog that does not list the manifest. It reads the contract's
`RULES_VERSION()` first: below 4 it refuses a catalog deal and a format-2 manifest. The contract
refuses a catalog that the publisher has replaced (`CatalogSuperseded`: the book holds a report
for the next catalog number under the same key). So a deal must use the publisher's newest
catalog. A node that holds the catalogs serves that proof: its manifest answer proves the
manifest from the newest catalog it holds that lists it (`--root 0x… --node URL`). Before the
token approval the tool asks the book what the contract will ask: does the anchor's report hold,
and is there a newer catalog. It stops with `CatalogSuperseded`, `NotAnchored` or `KeySuspended`
and sends nothing. If a new catalog lands while the deal is on its way, the contract refuses it:
send the deal again once the node has the new catalog. Other refusals (too few nodes, no
capacity, too few tokens) are still found only by the transaction itself, after the approval.

**What the commands read.** `status`, `epoch`, `settle`, `prove`, `seed` and `books` read the
contract's view functions at the head block (`currentEpoch`, `dealInfo`, `nodeInfo`,
`activeNodes`, `setHashAt`, `exclusionFrom`, `books`, `conserved`). Their cost no longer grows
with the contract's age: before, they read every log since the deploy block, about 35,000 calls
on a contract 7 million blocks old. What it grows with now: one `dealInfo` call for every deal
ever made, one `nodeInfo` call a listed node, and `exclusionFrom` calls. `epoch` and `settle`
read `exclusionFrom` only for the shards that epoch's challenge slots pick (2 shards a deal and
epoch, about 3 calls each, whatever the deal's size). `prove` reads none. `status --node` needs
them for every shard of the open deals (about 3 calls a shard): it reads them when those deals
have at most 1,024 shards together or with `--shards yes`; else `shards_now` and
`shards_next_epoch` are null and `shards_note` says why. `read` reads them for the shards its
key range needs. `register` reads the `Activated` logs since its own registration block only. Logs are now read only where no view has the answer, `--log-span`
blocks a call (default 200, the limit of sepolia.base.org), with a progress line on stderr every
50 calls:

- `epoch`: the `Answered` and `Slashed` logs of the blocks of that epoch and the two epochs after
  it (the amounts paid and slashed).
- A node list that is no longer the newest (a node joined or left since the epoch asked for):
  `NodeSet` logs, backwards from that epoch's first block until the list is found. `--since-block
  N` stops the search at block N; the command then says so if the list was not found.
- `status --since-block N`: the node's URL, from the `Registered` logs of blocks N and later.
  Without it `url` is null.
- `read`: the nodes' URLs and the deal's source, from the `Registered` and `DealCreated` logs
  since `--since-block` (default: the deploy block). Give the block before the deal was made, or
  the nodes' registration, to keep it short; a node whose URL was not read is not asked.
- `declines` and `books --replay yes` read every log since the deploy block: they report the
  whole history.

## onyx-node settings
See docs/ops/config.md "Storage network". The node fetches at most `ONYX_STORAGE_FETCHES_PER_TICK`
shards per pass, checks every byte against the deal's anchored manifest, records the epoch's seed
when nobody has and a deal is checked in that epoch, answers its challenges, and with `ONYX_STORAGE_KEEPER=on` reports other nodes'
misses.

**Removals** (ADR-0059 amendment 4 item 5a slice 2a). About hourly (`ONYX_STORAGE_REMOVALS_EVERY_SECS`)
a separate task asks for the publisher's newest catalog leaf of a few placed datasets, first from
`ONYX_STORAGE_CATALOG_NODES` (set it to the hosted query node), then each deal's source, then its
peers, and keeps a removal only when its tombstone and catalog report check at one finalized block.
While kept (`ONYX_STORAGE_REMOVALS=on`, the default) the dataset's rows, info and listing answer 410
`bulk_dataset_removed` with `learned`; the node still holds, proves and serves its shards by root, so
its deals' checks are untouched. Only the book's withdrawal of that catalog drops it. The task sends
no transaction; declining such deals is slice 2b.

**Match the node to the contract.** From ADR-0060 amendment 1 each holder proves its own row;
amendment 2 adds a notice on declines and names the shard in `Answered`. A node of this release
reads the contract's `RULES_VERSION()` (4, ADR-0067: deals through a catalog, format-2 manifests)
first and sends nothing (no seed, answer, settle or decline) to any other contract, such as the
S15 deployment `0x2423…9419`, an amendment 1 deployment or a rules-3 deployment: its log says
"this node follows rules v4". Sending nothing does not avoid penalties: the node then misses that contract's
checks and keepers slash and jail it. So exit the node from the old contract first (`exit`, then
`withdraw-bond` three to four epochs later), then upgrade and register on the new deployment. On the
web host the storage units start the same binary as the API, so the nodes exit before the new API
is deployed: docs/ops/runbooks/deploy-storage.md "Move from v3 to v4".

## Health

`GET /v1/health` of a storage node carries a `storage` object (null on a node outside storage
mode; THREATS OBS-STORAGE-1, review OBS-STORAGE-R1), so a loop that is stuck or failing no longer
answers like a healthy one. Times are unix seconds on this host's clock.

| Field | Meaning |
|---|---|
| `node` | This node's id |
| `epoch`, `wanted`, `held`, `fetched`, `fetchFailed`, `answered`, `answerFailed`, `unanswerable`, `seeds`, `settled`, `declined` | The last tick that ended well: its epoch, shards placed here now or next epoch, of those held, fetched and failed fetches, answers sent, answers whose send failed (out of gas, a fee cap, a nonce), challenges it could not answer (shard not held or not provable), seeds recorded, misses settled and shards declined |
| `startedUnix` | When the node started |
| `tickStartedUnix` | When the running tick began; null between ticks |
| `lastTickUnix` | When the last tick ended, well or not; null before the first, which waits a random part of the period |
| `lastOkUnix` | When the last tick that ended well ended |
| `lastTickOk` | Whether the last tick ended well; null before the first |
| `failedInARow` | Ticks in a row that ended in an error (0 after one that ended well) |
| `lastErrorClass` | The last tick's error class: `rpc_rate_limited`, `rpc_unreachable`, `rpc_error`, `contract_version`, `snapshot_disagrees`, `store` or `other`; null when it ended well. Never the error text (it may name a provider URL and its key): that is in the node log only (`onyx-node: storage: tick:`) |
| `everySecs` | The loop's period (`ONYX_STORAGE_EVERY_SECS`) |
| `logsReadThrough` | The last block the kept chain logs cover ("Restarts" below): it climbs to the chain's safe block while the node catches up, then follows it; null when none are kept, and on a restarted node until its first pass has checked the file against the chain |
| `removals` | `mode` (`on`, `log`, `off`), removals `kept`, placed datasets `neverLearned` and the last learn pass's finalized block `checkedAt` (0: none ran) |

The health watch alerts `storage.tick_age` when a tick has run more than 900 s or no tick has
started for 300 s (or 3 x `everySecs`) since the last ended, `storage.tick_failing` at the third
failed tick in a row and `storage.answers` when the last good tick missed answers for 3 runs
(docs/ops/runbooks/health-watch.md).

## Restarts (the chain log cache)

Each pass reads the contract's logs (node lists, registrations, deals, declines, slashes, passes)
in `eth_getLogs` ranges of 200 blocks and keeps those of safe blocks, so the next pass reads only
the newer blocks. The kept logs are also saved in the node's state folder,
`ONYX_STATE_DIR/storage-logs-0x<contract>.json` (`/var/lib/onyx-storage/<name>/state/` on the web
host; THREATS STORAGE-RESTART-1, review STORAGE-RESTART-R1): when a pass changed them (a new
event) and every 50 ranges they move otherwise, each save anchored at its last block (that block's
hash) and written whole (a temporary file of its own, sync, rename, folder sync). Before this, a
restart read the whole history from the deploy block again: about 14,900 calls through the public
endpoint on 2026-11-09, and no finished tick for well over 20 minutes.

A restart loads the file and reads on from its next block: a node that was current asks for at
most about 50 ranges, not the whole history. The file is used only when it is for this contract,
`ONYX_STORAGE_FROM_BLOCK`, the node's events and fold rules, of version 2, at most 32 MiB and
decodes whole (its check matches). The first pass then checks it against the chain before using
it: the RPC's chain id (`eth_chainId`) must be the file's and its last block must have the same
hash. A different chain id or hash drops it; an RPC that does not know that block yet (a backend
behind) fails the pass with `rpc_error` and the file is checked again next pass, never dropped
for that. The node's first pass after a start, and one about every day after, also compares the
contract's `exclusionFrom` with the kept logs for the nodes ranked at or above it on each shard
(at most 64 calls a check): a difference drops the logs and fails that pass
(`snapshot_disagrees`), and the next pass reads every log again. A file not used is logged
(`onyx-node: storage: log cache <path>: ignored (...)` or `dropped (...)`) and never fails the
start; when the node drops its kept logs (a reorg, logs that disagree with the contract) it removes
the file too.

**A wrong view.** If the node's view looks wrong (placement or exclusions that differ from
`onyx-storage status`, a node it should replace that it never fetches for), stop the node, delete
`storage-logs-0x<contract>.json` from its state folder and start it: it reads every log again from
the deploy block (one whole read, about 15,000 calls on 2026-11-09). The file holds only public
chain data, so deleting it is always safe; keep it with the state folder otherwise. Watch
`storage.logsReadThrough` on `/v1/health` climb to the chain's safe block after a restart.

## What gets you slashed
- A challenge for a shard you hold that you do not answer in its epoch (offline, disk lost,
  fetching off).
- An answer whose row or paths do not check.
Each takes 10% of the bond (placeholder), moves that shard to the next node, and three in a row
jail the node. A bond below half of the minimum also jails it; with ADR-0060 amendment 1, below
half of the bond the node started with when that is larger (about 7 slashes, whatever the bond).
Keep the node online through the epoch after you ask to exit.

**Keep shard files.** With ADR-0060 amendment 2, a decline of a shard your node passed a check on
in the decline's epoch or the 7 before it starts one epoch later, and the node is still checked on it in that epoch. So
keep every shard's file for at least 9 epochs (K + 2, K = 7) after the node's last passed check on
it. The node deletes no shard file today. Never prune shard files by hand.

## On the Onyx web host (ROADMAP S15)

`scripts/storage-sepolia.sh install` runs each node as `onyx-storage@<name>`
(`deploy/systemd/onyx-storage@.service`): the API's onyx-node binary in storage mode, settings in
`/etc/onyx/storage/<name>.env`, the operator key as a systemd credential, its own dynamic user
(`User=onyx-storage-<name>`, so no node reads another's key or shards), state under
`/var/lib/onyx-storage/<name>`, 512 MiB of memory at most. Caddy serves node `nI` at
`https://<domain>/storage/nI/` (read routes only) and proxies it to `127.0.0.1:820I`. Node `n1` is
the keeper. Logs: `journalctl -u onyx-storage@n1`.
