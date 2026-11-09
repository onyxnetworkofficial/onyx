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
| `status --node ID` | Epoch, the node's bond, misses, status, shards now and next epoch, earnings |
| `epoch [--epoch E]` | Every challenge of the epoch: who, which shard, passed, failed, missed, paid, slashed |
| `withdraw` | Earnings to the operator |
| `exit --node ID` | Leave the list from the next epoch (still asked in that epoch) |
| `withdraw-bond --node ID` | The bond back, two epochs after leaving |
| `settle [--epoch E]` | Report the misses of an ended epoch (any keeper may; with amendment 1 the reporter earns 10% of each slash, unless it reports its own node) |
| `books [--replay yes]` | Every flow of the contract and the conservation check; `--replay yes` also rebuilds the totals from the contract's events (one log call per 200 blocks: for a local chain or a short history) |
| `declines` | Every node and every deal with its declines (`shard_declines` and `whole_deal_declines`, one per shard or deal and node), the declines whose one-epoch notice set when the node leaves, and its `slashes`, from the contract's events (ADR-0060 amendment 2 item 1): a node that declines much more than others with the same deals stands out |
| `read --deal N --from K --to K` | Read rows from the replicas, every row checked, with fail-over |
| `deal --dir DIR --r 3 --epochs N --source-url URL` | (publisher) pay for storage of an anchored manifest |

Every command takes `--rpc URL --contract 0x… [--from-block N]`; sending ones also
`--chain-id N --key FILE` (it refuses any chain but 31337 and 84532).

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
reads the contract's `RULES_VERSION()` (3) first and sends nothing (no seed, answer, settle or
decline) to any other contract, such as the S15 deployment `0x2423…9419` or an amendment 1
deployment: its log says "this node follows rules v3". Sending nothing does not avoid penalties: the node then misses that contract's
checks and keepers slash and jail it. So exit the node from the old contract first (`exit`, then
`withdraw-bond` two epochs later), then upgrade and register on the new deployment.

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
