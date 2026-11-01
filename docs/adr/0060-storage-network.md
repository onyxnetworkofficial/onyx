# ADR-0060: The storage network: rendezvous placement, on-chain storage proofs, bonded nodes

## Status
Accepted, 2026-10-30 (test network only). Supersedes ADR-0055 item 4 (deposit per GiB) and
answers ADR-0059 open question 3 (several nodes per dataset). Built in `crates/onyx-storage`
(placement, proofs, repair, accounting, ABI), `contracts/src/OnyxStorage.sol` and
`OnyxStorageLib.sol`, `onyx_api::storage` (chain view, storage-node loop, client read with
fail-over), onyx-node's storage mode (`ONYX_STORAGE_*`), the `onyx-storage` tool and
`scripts/storage-network-anvil.sh`. Spec: docs/spec/storage-network.md. Plain-English master
document: docs/STORAGE-NETWORK.md. Threats: THREATS.md "STORAGE".

## Context
The owner expects "potentially hundreds of terabytes" from many publishers. ADR-0059 lets one node
hold a publisher's dataset: shards of at most 64 MiB under Keccak Merkle roots, a manifest anchored
in `OnyxSignedBook`, every row provable. One node cannot hold hundreds of TB, and one copy is not
safe. The network needs: many independent storage nodes, each shard on several of them, a way to
check on chain that they still hold it, payment for those that do, a penalty and a repair for those
that do not, and a client that reads from any copy and needs to trust none. The chain cannot hold
the data (a 64 MiB shard would cost far more gas than a block allows), only fingerprints.

## Decision
1. **What the chain holds.** Fingerprints only: the manifest root (bound to the publisher's anchor),
   `shards_root`, the shard count and byte count; nodes, bonds, deals, seeds, exclusions and money.
   Rows never go on chain except one challenged row per answer.
2. **Deals.** `createDeal(header, payload, r, epochs, sourceUrl)`: the contract recomputes the
   ADR-0059 manifest root from the Borsh header bytes and reads the fixed-width tail (shard count,
   row count, byte count, key range, `shards_root`) and the dataset and publisher ids. It accepts
   only the exact anchor payload of that header (publisher, metric `bulk.manifest`, subject
   `Aggregate(dataset)`, value `Bytes(root)`) that `OnyxSignedBook` recorded, whose fact the book
   accepted on exactly that report, under a registered key that is not suspended. The payer
   deposits `price × ceil(bytes / GiB) × r × epochs` tONYX up front; the deal is challenged from two
   epochs after creation. This is a deliberate exception to ADR-0011 packing: the EVM recomputes
   two ADR-0059 hashes as they are defined (manifest root over Borsh, shard entry leaf as Borsh),
   because both are already signed and anchored; both layouts are fixed-width where they are read,
   and shared vectors pin them. Every new hash uses ADR-0011 packing.
3. **Nodes.** `register(capacityGiB, url)` takes a bond of `max(minBond, bondPerGiB × capacity)`;
   `activate` in a later block fixes the node id as `keccak256("onyx-storage-node-v1" ‖ operator ‖
   index ‖ blockhash(registration block))`, a hash unknown when the registration was sent. At most
   128 active nodes (an answer carries the node list). The list changes from the next epoch;
   the contract checkpoints its hash per epoch.
4. **Placement: rendezvous (highest-random-weight) hashing.** Shard `i` of a deal has the key
   `keccak256("onyx-storage-shard-v1" ‖ salt ‖ i)` (the salt from the block hash before the deal);
   each node's weight is `keccak256("onyx-storage-hrw-v1" ‖ key ‖ nodeId)`; the holders are the
   R highest, skipping nodes excluded from that shard. A join or a leave moves only the shards whose
   top R changed (about R/N of them); no table of assignments is stored anywhere.
5. **Epoch rule.** Challenges in epoch `e` use the node list of `e - 1` and the exclusions in force
   at `e - 1`, less nodes excluded from the shard or jailed by `e`: every challenged holder had at
   least one whole epoch to fetch what it newly holds.
6. **Seeds and challenges.** The seed of epoch `e` is `keccak256("onyx-storage-seed-v1" ‖
   blockhash(start of e) ‖ e)`, recorded by anyone in the 256 blocks after the start block (an
   unrecorded epoch has no challenges and pays nothing). Each deal has `challengesPerDeal` slots per
   epoch; slot `s` picks shard `keccak("onyx-storage-slot-v1" ‖ seed ‖ deal ‖ s) mod shardCount` and
   row `keccak("onyx-storage-row-v1" ‖ seed ‖ deal ‖ s) mod rows`.
7. **Answers.** A challenged holder's operator calls `answer` in the epoch with the 84-byte shard
   entry, its Merkle path to `shards_root`, the row bytes and their path to the entry's root
   (`OnyxMerkle`, ADR-0003). Indexes and leaf counts come from the challenge and the entry, never
   from the answer. A proof that holds earns the slot share `price × gib × r / (challengesPerDeal ×
   r)`, rounded down; one that does not is recorded as failed and slashed at once.
8. **Misses, slashes, repair.** After the epoch, anyone may `settleMissed` an unanswered challenge
   (within two epochs). A failed or missed answer takes `slashPpm` of the node's bond to the
   treasury account, excludes the node from that shard from the next epoch (rendezvous moves the
   copy to the next node in the ranking, which fetches it: repair) and, after `maxMisses` in a row
   or a bond below half of `minBond`, jails the node (out of the list from the next epoch; all its
   shards move). Exit is voluntary with the same one-epoch notice; bonds return after the settle
   window. A node may also `decline` one shard or a whole deal it cannot get (a withholding
   publisher or replicas): the same exclusion from the next epoch, without a slash; a challenge
   already drawn still stands (review STORAGE-R1, THREATS STORAGE-16).
9. **Money.** tONYX only. No owner, no admin path, every parameter immutable. Tokens leave only as
   earnings to the operator that answered, a bond to its operator after exit, a deal's remainder to
   its payer after the deal ends, or slashes to the immutable treasury. Conservation is a view
   (`conserved()`) and an invariant suite: the balance equals deposits not paid or refunded, plus
   bonds held, plus earnings not withdrawn, plus slashes not swept.
10. **Nodes and clients.** onyx-node in storage mode reads the chain, fetches the shards placed on
    it now and next epoch from the other holders or the publisher, checks every byte against the
    deal's anchored manifest (the same checks as a publisher upload, ADR-0059), serves them on the
    bulk routes and two peer routes (`GET /v1/bulk/manifests/{root}`, `GET /v1/bulk/shards/{root}`),
    records seeds, answers its challenges and (as a keeper) settles others' misses. A client reads a
    shard's rows from its holders in rank order, then last epoch's, then the publisher, accepting
    only rows that prove to the deal's anchored manifest; a node that is down, lacks the shard or
    serves a forged row is skipped.

## Consequences
- Many people can store data and many nodes can hold it; the chain holds fingerprints and money.
  Scaling math, costs and phases: docs/STORAGE-NETWORK.md.
- `OnyxStorage` is 20,419 bytes of runtime code (EIP-170 limit 24,576). Gas: docs/STORAGE-NETWORK.md
  "Status" and `contracts/.gas-snapshot`.
- An answer carries the whole node list (32 bytes per node) and recomputes all weights: fine to 128
  nodes, not to thousands (later: per-epoch committee lists or a Merkle proof of the node's rank).
- Challenges grow with deals, not bytes: two slots per deal per epoch, so a node's proof count
  follows how many deals it holds shards of. Aggregated proofs are a later phase.
- Placement ignores declared capacity, so every node holds about R/N of every deal; `activate`
  therefore admits a node only with room for its even share of what is committed (review
  STORAGE-R1-6; a rule in `createDeal` let one small node freeze deals, STORAGE-R2-2). Large nodes
  cannot use their extra room until weighted placement (ROADMAP S17).
- A deal is billed for its bytes or 8 MiB per shard, whichever is more (a node's work grows with
  shards; review STORAGE-R2-5).
- Payment is per answer: a node's expected pay is proportional to its share of a deal's shards
  (shards of unequal size are paid the same); rounding dust and unanswered shares return to the
  payer at close.
- Known limits, each in THREATS "STORAGE": a node may fetch a shard from another replica only when
  challenged (no sealing yet); one operator with many node ids may hold all R copies of a shard;
  the block producer can bias the seed (H2); a node may pass proofs and still refuse reads.

## Alternatives
- **Assignments stored on chain** (a table of shard -> nodes): millions of entries at 500 TB;
  rejected. Rendezvous needs no table and moves the least data on a change.
- **Consistent hashing on a ring**: also moves little, but uneven load without many virtual nodes,
  and a ring is harder to recompute on chain than R maximum weights; rejected.
- **Random challenges per node** (each node proves one of its own shards): the contract cannot list
  a node's shards without a table; a node could pick which one. Challenges per deal slot with the
  holders computed on chain need neither.
- **Proofs of replication and space-time (Filecoin's sealing)**: stop outsourcing, but need heavy
  sealing and SNARKs; later (docs/STORAGE-NETWORK.md, phase "sealed replicas").
- **Erasure coding now** (k of n pieces instead of R copies): 3× copies cost about twice the bytes
  of a 10-of-16 code; kept for a later phase, since every row would then need decoding to be served
  with its proof.
- **An outside storage network for everything** (Filecoin, Arweave): no row proofs to our own
  anchors, prices and terms we do not set; kept for a cold archive tier only.
