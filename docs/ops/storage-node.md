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
| `register --capacity-gib N --url URL` | Approves and posts the bond, registers, activates in a later block; prints `NODE_ID` |
| `status --node ID` | Epoch, the node's bond, misses, status, shards now and next epoch, earnings |
| `epoch [--epoch E]` | Every challenge of the epoch: who, which shard, passed, failed, missed, paid, slashed |
| `withdraw` | Earnings to the operator |
| `exit --node ID` | Leave the list from the next epoch (still asked in that epoch) |
| `withdraw-bond --node ID` | The bond back, two epochs after leaving |
| `settle [--epoch E]` | Report the misses of an ended epoch (any keeper may) |
| `books` | Every flow of the contract and the conservation check |
| `read --deal N --from K --to K` | Read rows from the replicas, every row checked, with fail-over |
| `deal --dir DIR --r 3 --epochs N --source-url URL` | (publisher) pay for storage of an anchored manifest |

Every command takes `--rpc URL --contract 0x… [--from-block N]`; sending ones also
`--chain-id N --key FILE` (it refuses any chain but 31337 and 84532).

## onyx-node settings
See docs/ops/config.md "Storage network". The node fetches at most `ONYX_STORAGE_FETCHES_PER_TICK`
shards per pass, checks every byte against the deal's anchored manifest, records the epoch's seed
when nobody has, answers its challenges, and with `ONYX_STORAGE_KEEPER=on` reports other nodes'
misses.

## What gets you slashed
- A challenge for a shard you hold that you do not answer in its epoch (offline, disk lost,
  fetching off).
- An answer whose row or paths do not check.
Each takes 10% of the bond (placeholder), moves that shard to the next node, and three in a row
jail the node. Keep the node online through the epoch after you ask to exit.

## On the Onyx web host (ROADMAP S15)

`scripts/storage-sepolia.sh install` runs each node as `onyx-storage@<name>`
(`deploy/systemd/onyx-storage@.service`): the API's onyx-node binary in storage mode, settings in
`/etc/onyx/storage/<name>.env`, the operator key as a systemd credential, its own dynamic user
(`User=onyx-storage-<name>`, so no node reads another's key or shards), state under
`/var/lib/onyx-storage/<name>`, 512 MiB of memory at most. Caddy serves node `nI` at
`https://<domain>/storage/nI/` (read routes only) and proxies it to `127.0.0.1:820I`. Node `n1` is
the keeper. Logs: `journalctl -u onyx-storage@n1`.
