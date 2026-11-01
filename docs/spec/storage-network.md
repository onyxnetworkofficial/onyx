# Spec: the storage network (ADR-0060)

Status: **Accepted as ADR-0060 (2026-10-30), built on a local anvil.** Test network only. Code:
`crates/onyx-storage`, `contracts/src/OnyxStorage.sol` and `OnyxStorageLib.sol`,
`onyx_api::storage`, onyx-node's storage mode, `onyx-storage`. Plain-English overview and roadmap:
docs/STORAGE-NETWORK.md. Threats: THREATS.md "STORAGE". Operator guide: docs/ops/storage-node.md.

## Scope

Many independent storage nodes hold the shards of anchored bulk datasets (ADR-0059), each shard on
R of them, chosen by rendezvous hashing; the nodes prove on chain, every epoch, that they still
hold sampled rows; they are paid from the publisher's deposit and slashed when they cannot prove;
lost copies are repaired onto other nodes; clients read from any copy and check every row. Out of
scope here: erasure coding, sealed replicas, a cold archive tier, store-on-first-use, read
payments, mainnet (docs/STORAGE-NETWORK.md "Phases").

## Interfaces

### Hashes (ADR-0011 packing: `abi.encodePacked`, integers big-endian)
| Name | Definition |
|---|---|
| node id | `keccak256("onyx-storage-node-v1" ‖ operator (20) ‖ uint64 index ‖ blockhash(registration block))` |
| deal salt | `keccak256("onyx-storage-salt-v1" ‖ blockhash(block − 1) ‖ uint64 dealId)` |
| shard key | `keccak256("onyx-storage-shard-v1" ‖ salt ‖ uint32 index)` |
| weight | `uint256(keccak256("onyx-storage-hrw-v1" ‖ shardKey ‖ nodeId))` |
| seed | `keccak256("onyx-storage-seed-v1" ‖ blockhash(start block of e) ‖ uint64 e)` |
| slot shard | `uint256(keccak256("onyx-storage-slot-v1" ‖ seed ‖ uint64 dealId ‖ uint32 slot)) mod shardCount` |
| slot row | `uint256(keccak256("onyx-storage-row-v1" ‖ seed ‖ uint64 dealId ‖ uint32 slot)) mod rows` |
| set hash | `keccak256(nodeId[0] ‖ nodeId[1] ‖ …)` in the contract's stored order |

Read as defined by ADR-0059: the manifest root `keccak256("onyx-bulk-manifest-v1" ‖ borsh(header))`;
the header's last 84 bytes (`shard_count u32, row_count u64, byte_count u64, key_min i128,
key_max i128, shards_root`, little-endian); the shard entry leaf `borsh(ShardEntry)` (84 bytes:
`index u32, root, rows u64, bytes u64, key_min i128, key_max i128`). Shared vectors:
`fixtures/storage/vectors.json` (Rust `onyx-storage` writes, `OnyxStorageVectors.t.sol` reads).

### Contract `OnyxStorage`
Writes: `register(uint64 capacityGiB, string url)`, `activate(uint64 index)`,
`cancelRegistration(uint64)`, `requestExit(bytes32)`, `withdrawBond(bytes32)`,
`createDeal(bytes header, bytes payload, uint8 r, uint32 epochs, string sourceUrl)`,
`extendDeal(uint64, uint32)`, `closeDeal(uint64)`, `recordSeed(uint64)`,
`answer(uint64 dealId, uint64 epoch, uint32 slot, bytes32 nodeId, bytes32[] nodes, bytes entry,
bytes32[] entrySiblings, bytes row, bytes32[] rowSiblings)`,
`settleMissed(uint64 dealId, uint64 epoch, uint32 slot, bytes32 nodeId, bytes32[] nodes)`,
`decline(uint64 dealId, uint32 index, bytes32 nodeId)`, `withdrawEarnings()`, `sweepTreasury()`. Views: `currentEpoch`, `epochStart`, `setHashAt`,
`activeNodes`, `seedOf`, `dealInfo`, `dealCount`, `nodeInfo`, `exclusionFrom`, `slotStatus`,
`challengedHolders`, `slotPay`, `bondFor`, `earned`, `books`, `conserved`, `params`. Events:
`Registered`, `Activated`, `NodeSet`, `Removed`, `DealCreated`, `DealExtended`, `DealClosed`,
`SeedRecorded`, `Answered`, `Slashed`, `Declined`, `BondWithdrawn`, `EarningsWithdrawn`, `TreasurySwept`.
Immutable parameters: token, book, treasury, `epochBlocks`, `pricePerGiBEpoch`, `bondPerGiB`,
`minBond`, `slashPpm`, `challengesPerDeal`, `maxMisses`. Constants: `MAX_NODES` 128, `MAX_R` 8,
`SETTLE_EPOCHS` 2, `SEED_WINDOW` 256 blocks, `MAX_DEAL_EPOCHS` 100,000, `MAX_CAPACITY_GIB` 2^30.

### Node routes (peers; outside the 12 s request deadline)
`GET /v1/bulk/manifests/{root}` → `{root, manifest, report}` (Borsh hex, the body
`POST /v1/bulk/manifests` takes); `GET /v1/bulk/shards/{root}` → the shard file's bytes. Rows are
served by the existing `GET /v1/bulk/{publisher}/{dataset}/rows`.

### Tools
`onyx-storage register | status | deal | seed | settle | withdraw | exit | withdraw-bond | close |
sweep | books | epoch | read | prove`; onyx-node `ONYX_STORAGE_*` (docs/ops/config.md).

### Rules
- **R-STOR-1 (nodes).** `register` takes `bondFor(capacity) = max(minBond, bondPerGiB ×
  capacity)`, `1 <= capacity <= MAX_CAPACITY_GIB`, a URL of 1..=256 bytes. `activate` only by the
  registrant, in `(registration block, registration block + 256]`, with a declared capacity of at
  least `ceil(committedGiB / (activeNodes + 1))`, fixes the node id and adds it to the list (at most
  `MAX_NODES`). A registration not activated in time can be cancelled for its bond.
- **R-STOR-2 (node list).** Any change to the list in epoch `c` takes effect from `c + 1`: one
  checkpoint `(c + 1, setHash)` per epoch, the last change wins; `setHashAt(e)` is the newest
  checkpoint at or before `e` (`keccak256("")` before any). `NodeSet` carries the full list.
- **R-STOR-3 (deals).** `1 <= r <= MAX_R`, `1 <= epochs <= MAX_DEAL_EPOCHS`; the header is format 1
  with ids of 1..=255 bytes, a shard count of 1..=65,536 and a byte count of at least 8 per shard; a
  source URL of
  at most 256 bytes; the payload is exactly the
  anchor of that header (any key, time and sequence); `book.report(digest)` is recorded,
  `book.factView(fact)` is accepted on exactly its `reportHash`, `book.keyState(key)` is registered
  and not suspended; at least `r` active nodes; `gib = ceil(max(byteCount, shardCount × 8 MiB) /
  GiB)`; `committedGiB + gib × r <= totalCapacityGiB`; deposit and bonds fit 128 bits; the
  deposit `price × gib × r × epochs` is transferred in (the balance must grow by exactly that).
  Start epoch = current + 2. `closeDeal` from `endEpoch` on returns `deposit − paid` to the payer.
- **R-STOR-4 (placement).** Holders of shard `i` under list `L` at epoch `e`: the `r` nodes of `L`
  with the highest weights (ties by the smaller id), skipping any node whose exclusion from
  `(deal, i)` starts at or before `e`.
- **R-STOR-5 (seeds).** `recordSeed(e)` once, while `start(e) < block.number <= start(e) + 256`.
- **R-STOR-6 (who is challenged).** In epoch `e`, slot `s` of deal `d` (`startEpoch <= e <
  endEpoch`, `s < challengesPerDeal`, seed recorded) asks the holders of its shard under
  `setHashAt(e − 1)`'s list at `e − 1`, less nodes excluded from the shard from `<= e` and nodes
  jailed from `<= e`. The caller passes that list; its hash must equal `setHashAt(e − 1)`.
- **R-STOR-7 (answers).** Only the node's operator, only in epoch `e`, once per (deal, epoch, slot,
  node). The entry is 84 bytes, its index is the slot's shard, it proves to `shardsRoot` at that
  index among `shardCount`; `rows > 0`; the row proves to the entry's root at the slot's row among
  `rows`. Passed: status 1 and the slot share. Otherwise: status 2 and R-STOR-9.
- **R-STOR-8 (pay).** Slot share = `price × gib × r / (challengesPerDeal × r)`, rounded down,
  never beyond the deal's deposit; credited to the operator's `earned`, withdrawn by it.
- **R-STOR-9 (misses and slashes).** `settleMissed` by anyone for an open challenge of an epoch
  that ended at most `SETTLE_EPOCHS` ago: status 3. A failed or missed answer takes
  `bond × slashPpm / 10^6` (rounded down) for the treasury, adds a miss (a pass clears the count),
  excludes the node from that shard from the next epoch (first exclusion kept), and jails the node
  (out of the list from the next epoch) after `maxMisses` in a row or a bond below `minBond / 2`.
  `requestExit` leaves the list from the next epoch. `withdrawBond` after `removedFrom +
  SETTLE_EPOCHS`.
- **R-STOR-10 (accounting).** For an epoch, the challenges are R-STOR-6 over every running deal;
  each ends passed (paid), failed or missed (slashed) or stays open; `onyx-storage epoch` prints it.
- **R-STOR-11 (money).** No owner and no admin function. Tokens leave only by R-STOR-3 (refund),
  R-STOR-8 (earnings), bond withdrawal or cancellation, and the treasury sweep. `conserved()`:
  `paid + refunded <= deposits`, `slashed + bondsOut <= bondsIn`, `earningsOut <= paid`,
  `swept <= slashed`, and the token balance equals `(deposits − paid − refunded) + (bondsIn −
  slashed − bondsOut) + (paid − earningsOut) + (slashed − swept)`.
- **R-STOR-12 (storage node).** Each pass, in this order: record the seed; answer each slot it is
  challenged in with a proof it checked first (fetching again a shard that does not read back); as
  a keeper, settle the open challenges of the last two epochs (an answer re-fetches only a shard
  held but damaged, for at most one call); then, trying no URL after `FETCH_BUDGET` (300 s), fetch
  (at most `ONYX_STORAGE_FETCHES_PER_TICK` successes, at most twice that in attempts, one shard of
  each deal in turn, the first deal moving each pass) the shards it holds under the placement of
  this epoch and the next and has not declined or been excluded from, from the other holders of the
  last, current and next epoch, then the deal's source URL. A manifest is asked once per deal per
  pass, from the source and the holders of shard 0 now and last epoch, and taken only when its root
  is the deal's and its signature checks, versions in order; a shard only through the bulk store's
  upload checks. A log string (URL, ids) that is not printable
  ASCII of at most 256 bytes reads as empty and never fails the pass.
- **R-STOR-13 (client read).** Take the manifest with exactly the deal's root (and a valid
  signature). For each shard overlapping the asked keys: fetch the whole shard file from its holders
  under the current, previous and next placement, then the source; accept it only when
  `check_shard` holds and its root, rows, bytes and key range equal the manifest's entry; cut the
  asked keys from it (so no row can be left out); otherwise try the next; none: unavailable, never
  unchecked rows.
- **R-STOR-14 (decline).** Only a node's operator may `decline(deal, index, node)` one shard or the
  whole deal (`index = 2^32 − 1`); the node's exclusion starts at the next epoch (the earlier
  exclusion is kept); no slash; a challenge drawn in the current epoch still stands. The storage
  node declines after 3 failed passes in an epoch after a failure in an earlier one; the whole deal
  in one call when its manifest failed, or nothing of it is held and every shard tried failed.

## Assumptions
- tONYX has no value; every price, bond and slash is a placeholder the owner sets at deploy
  (docs/STORAGE-NETWORK.md "Cost model").
- Base's block producer does not bias block hashes against the seed (testnet; H2 for mainnet).
- At most 128 active nodes; R at most 8; a node is honest about nothing (every byte is checked).
- Publishers keep their own copy until a deal has run for a while (the source URL is the last
  fetch source).
- Epoch length is long enough for every holder to fetch what changed (one day on Base Sepolia).

## Open questions
1. Weight by bytes: shards of unequal size are paid the same per answer (the publisher tool cuts
   shards of equal target size).
2. Read payments (a node may answer proofs and refuse reads): retrieval receipts or a pay-per-read
   channel?
3. Committee sampling for more than 128 nodes: a per-epoch committee list, or a Merkle proof of
   rank?
4. Erasure coding vs replicas: when do the bytes saved pay for decoding on every read?
