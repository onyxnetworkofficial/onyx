# The Onyx storage network

How Onyx keeps hundreds of terabytes from many people without one central server.

This is the master document for the storage roadmap. It is written for the owner: short sentences,
no code. The exact rules are in [docs/spec/storage-network.md](spec/storage-network.md); the
decision and its alternatives in [ADR-0060](adr/0060-storage-network.md); the threats in
[THREATS.md](../THREATS.md) section "STORAGE". Test network only: no real money anywhere.

> The owner's requirement: "Potentially hundreds of terabytes will be stored with Onyx. Many people
> will be submitting data." "Keep this system in a MD, this is very important for the future
> roadmap. Code it all now."

## 1. The problem in one paragraph

A publisher brings a large dataset. One computer cannot hold hundreds of terabytes, and one copy is
never safe: disks fail, machines go offline, operators cheat. So many independent computers
("storage nodes") must each hold a part, every part must sit on several of them, someone must check
that they really keep it, they must be paid when they do and lose money when they do not, and a
lost copy must be rebuilt somewhere else. A reader must be able to get any row from any copy and
know it is the real row. And all of this without trusting one company's server.

## 2. The words we use

| Word | Meaning |
|---|---|
| Dataset | A publisher's data, cut into shards (ADR-0059). |
| Shard | A piece of a dataset: at most 64 MiB, up to 262,144 rows. Default 8 MiB. |
| Fingerprint (root) | A 32-byte Keccak hash that changes if any byte of the data changes. |
| Manifest | The dataset's table of contents: every shard's fingerprint, row count and size. It has its own fingerprint. |
| Anchor | The publisher's signature over the manifest's fingerprint, recorded on chain in `OnyxSignedBook`. |
| Storage node | A computer that holds shards, serves them, and proves it holds them. It posts a bond. |
| Bond | tONYX a node locks up. A part is taken (slashed) when it fails a check. |
| Deal | A publisher's paid order: "keep my manifest, R copies, for N epochs". |
| R | Replication factor: how many nodes are placed on each shard and paid to answer its checks. Default 3. It does not prove R disk copies (section 6, ADR-0062). |
| Epoch | The time unit of checks and pay. On Base Sepolia now: 24 minutes (7,200 blocks of 0.2 s). |
| Challenge | A random question: "show me row 812 of shard 4,031". |
| Proof | The row and two short chains of hashes that link it to the anchored fingerprint. |
| Repair | Copying a shard to a new node when a holder failed. |

## 3. How data flows

```text
 PUBLISHER                        CHAIN (Base Sepolia)                 STORAGE NODES (many)
 ---------                        --------------------                 --------------------
 1. cut data into shards
    fingerprint every row,
    shard and the manifest
 2. sign the manifest's    ---->  OnyxSignedBook: anchor
    fingerprint                   (32 bytes + signature)
 3. pay a deal             ---->  OnyxStorage: deal
    (deposit per GiB,             (manifest fingerprint,
     R copies, N epochs)           shard count, bytes, R)
                                         |
                                         | 4. placement: each shard goes to
                                         |    the R nodes with the highest
                                         |    "rendezvous" score for it
                                         v
                                  node list + scores  ------------->  5. each node fetches ITS
                                                                         shards (from the publisher
 serves its own copy  <------------------------------------------------  or another copy) and checks
 until the nodes have it                                                 every byte against the
                                                                         anchored fingerprint
                                  6. every epoch: a seed from a
                                     past block hash picks shards
                                     and rows to check
                                         |
                                         v
                                  7. holders answer with the row  <---  proof (row + hash path)
                                     + path; the contract checks
                                     the hashes
                                  8. passed: paid from the deposit
                                     missed or wrong: bond slashed,
                                     copy moves to the next node  --->  9. repair: the next node
                                                                          in line fetches the copy
 READER
 ------
 10. asks the chain who holds shard 4,031, asks one holder, checks every row against the
     anchored fingerprint; if that holder is down or lies, asks the next one.
```

## 4. Why the chain holds only fingerprints

- **Size.** A 64 MiB shard would take thousands of blocks of calldata. A fingerprint is 32 bytes.
- **Cost.** Chain storage is the most expensive storage there is. Disks are cheap.
- **Enough.** A fingerprint plus a hash path proves any single row. The chain does not need the
  data to check that a node has it: it asks for a random row and checks its path. A node that
  dropped the data must get the row from somewhere else in time, or it fails.
- **What the chain holds:** the anchor, each deal (manifest fingerprint, shard count, bytes, R,
  epochs, deposit), the node list, bonds, the epoch seeds, which node was excluded from which shard,
  who answered, and every token movement.

## 5. Placement: who holds what

- Every node gets a score for every shard: a hash of the shard's key and the node's id. The R nodes
  with the highest scores hold that shard. This is **rendezvous (highest-random-weight) hashing**.
- No table of "who holds what" is stored anywhere. Anyone can recompute it from the node list.
- **Stable.** When a node joins, it takes only the shards where it now scores in the top R (about
  R/N of all copies). When a node leaves, only its own shards move, each to the next node in line.
  Measured: with 118 nodes, one leaving moved 0.85% of all copies (85 of every 10,000), which is
  1 in 118.
- **Fair.** Each node holds about R/N of all shards (tested: 10 nodes, 10,000 shards, every node
  between 2,700 and 3,300 copies).
- **Join with room for your share.** Placement does not look at a node's declared size, so a node
  may join only if it declared at least its even share of what is already stored (all copies ÷
  number of nodes, itself included). Later deals can raise the share; a node that runs out of room
  gives data up with `decline` (section 8), without a penalty. Placement weighted by size is a
  later phase (section 12).
- **Placement: publishers cannot choose it; operators can still grind ids.** A node's id comes
  from the hash of the block its registration landed in, which nobody knows when they send it. A
  deal's placement salt comes from the block before the deal. A publisher cannot pick its
  placement. An operator can: it sees its id one block after
  registering, activates only an id it likes and cancels the rest (ADR-0062 finding 7). In v0 it
  gets the whole bond back. The next contract (ADR-0060 amendment 1, built, not deployed) keeps 2%
  of every bond at registration, so each try costs money.
- **One epoch of notice.** Checks in an epoch use the node list of the epoch before, so a node
  always has at least one full epoch to fetch a shard before it can be asked for it.

## 6. Proofs: how the chain checks a node

1. At the start of each epoch, anyone records a seed: the hash of that epoch's first block (it
   cannot be known before).
2. For each deal, the seed picks 2 slots. Each slot picks one shard, and in it one row for each of
   that shard's holders: the row also depends on the holder's id, so holders are asked for
   different rows (next contract, ADR-0060 amendment 1; in v0 all holders get the same row).
3. Each of that shard's holders sends its row, the shard's line in the manifest, and the two hash
   paths (row to shard fingerprint, shard line to manifest fingerprint). Another holder's answer,
   public once sent, does not answer for it.
4. The contract recomputes the hashes. It needs only the fingerprint it already holds.
5. Right: the node earns its share of the deal's pay for the epoch. Wrong: 10% of its bond goes to
   the treasury at once. No answer by the epoch's end: anyone can report the miss, same penalty;
   in the next contract the reporter gets 10% of that penalty (not the node's own operator).
6. A node that fails a shard is taken off that shard from the next epoch; the next node in line
   takes it over (repair). Three failures in a row, or a bond below half the minimum: the node is
   jailed and all its shards move. In the next contract, half of the bond the node started with
   (if that is more than the minimum): about 7 penalties jail any node, whatever its bond.

**What a passed check shows (ADR-0062).** The node's operator sent a correct row in that epoch,
from any source. It does not show that the node keeps its own copy. In v0 (the contract live on
Base Sepolia) every holder of the shard is asked for the same row, so after the first answer the
others can copy it from the chain; all R holders may keep nothing while some source serves rows.
The data survives while one holder or the publisher keeps it and stays reachable until a new holder
copies it. A shard costs a holder at most one slash (it is then taken off the shard), and giving a
shard up (`decline`) is free. ROADMAP S16 closes the cheap holes, built and tested in the next
contract, not deployed yet (ADR-0060 amendment 1): a different row for each holder, a fee on every
registration so grinding ids costs money, 10% of each reported slash for the reporter, and jail
below half of a node's own bond. What stays: a holder can still fetch its own row from any source
when asked, and a shard of very few rows gives holders the same row by chance (1 row: always);
answered rows stay in calldata, so over many epochs a small shard becomes public on chain. Sealed
replicas, which would prove separate disk copies, are not planned before S17 (ADR-0062).

## 7. Reads

- A reader computes the holders of the shard it needs (the same placement as everyone).
- It asks the first holder for the rows. It checks every row's path up to the anchored manifest
  fingerprint and the publisher's signature. It accepts nothing unchecked.
- If the holder is down, does not have the shard, or sends a forged row, the reader asks the next
  holder, then last epoch's holders, then the publisher. Shown in the demo with a node killed and
  in tests with forging nodes.
- Reads are free on the test network. Paying for reads is a later phase (section 12).

## 8. Repair

- A miss or a wrong answer moves that one shard's copy to the next node in line.
- A jailed or exiting node's shards all move, each to its own next node. They spread over all other
  nodes, not onto one (tested: one of 10 nodes failing sends its shards to all 9 others, none more
  than twice its fair share).
- The new holder fetches the shard from the remaining holders or the publisher and checks every
  byte. Each node fetches at most 8 shards per pass (setting), so a big failure does not flood
  the network.
- A node that finds one of its own shards damaged on disk fetches it again before answering.
- **Giving data up (decline).** A node that cannot get a shard placed on it (the publisher and
  every other copy refuse or are down) gives it up with `decline` instead of waiting to be
  slashed. It waits for 3 failed tries in an epoch after a failed try in an earlier one, so a short
  outage never gives data up. When nothing of a deal can be had, the whole deal goes in one
  transaction. Deals take turns for downloads, and one pass never spends more than 5 minutes on
  them, so a publisher who withholds data cannot slow down anyone's proofs. A check already drawn for this epoch still stands; the
  copy moves to the next node from the next epoch. No penalty.

## 9. Payment

| Flow | From | To | When |
|---|---|---|---|
| Deposit | Publisher | Contract | When the deal is made: price × billed GiB × R × epochs (billed GiB: the bytes, or 8 MiB per shard if that is more) |
| Pay | Contract | Node that answered right | Each passed check: the epoch's pay ÷ (2 slots × R) |
| Registration fee (next contract) | Node's bond | Treasury | At registration: 2% of the bond, never returned, even when the registration is cancelled |
| Slash | Node's bond | Treasury | Each wrong or missed answer: 10% of the bond |
| Reporter's reward (next contract) | A missed answer's slash | Whoever reported the miss | 10% of that slash, withdrawn like earnings; nothing when the node's own operator address reports it (an operator that reports from a second address gets the 10% back, so a miss costs it 9%) |
| Refund | Contract | Publisher | When the deal ends: what was not paid out (missed checks, rounding) |
| Bond back | Contract | Node operator | Two epochs after leaving, or when a registration is cancelled (less the fee) |

- Prepaid: a publisher who does not pay gets nothing placed.
- No owner, no admin key, no way to move funds except the flows above.
- **Conservation, checked on chain:** tokens held = deposits not yet paid or refunded + bonds +
  earnings and rewards not withdrawn + slashes and fees not yet sent to the treasury. A test suite
  runs this after random sequences of every action; the demo checks it at the end and, with
  `onyx-storage books --replay yes`, rebuilds the same totals from the contract's events.
- Every division rounds down; the leftover stays in the deal and goes back to the publisher.

## 10. Scaling: what 100 TB and 500 TB need

### Assumptions (change any of these and redo the arithmetic)
| Input | Value used | Why |
|---|---|---|
| 1 TB | 10^12 bytes | Disk makers' unit |
| R (copies) | 3 | Default; survives 2 lost copies of any shard when every holder keeps its own copy (not proven: section 6) |
| Node disk | 16 TB usable | One large disk or a small array; a placeholder |
| Fill limit | 80% | Room for repair copies |
| Shard size | 64 MiB at scale (8 MiB today) | Fewer shards to place and check |
| Shards per manifest | at most 65,536 | ADR-0059 cap (4 TiB per manifest at 64 MiB) |
| Checks | 2 slots per deal per epoch | Contract parameter |
| Node list | at most 128 nodes | Contract cap (an answer carries the list) |

### The arithmetic
| | 100 TB | 500 TB |
|---|---|---|
| Bytes stored with R = 3 | 300 TB | 1,500 TB |
| Disk needed at 80% fill | 375 TB | 1,875 TB |
| Nodes of 16 TB | 24 | 118 (under the 128 cap) |
| Nodes of 100 TB | 4 (too few: at least 10 for safety) | 19 |
| Shards at 64 MiB | 1,490,117 | 7,450,581 |
| Shards at 8 MiB | 11,920,929 | 59,604,645 |
| Manifests (deals) needed at 64 MiB | 23 or more | 114 or more |
| Answers on chain per epoch (2 slots × 3 copies per deal) | 138 or more | 684 or more |
| Copies that move when one 16 TB node fails | its ~12.8 TB, spread over 23 nodes | its ~12.8 TB, spread over 117 nodes |
| Per receiving node when one fails | about 560 GB | about 110 GB |
| Placement recompute on a node (whole network) | about 30 s (24 nodes) | about 12.5 min (118 nodes) |

How the last row was measured: a node computed 1.19 million scores per second on one core of the
development machine while other builds ran (`cargo run --release -p onyx-storage --example
bench-placement -- 200000 118`: 200,000 shards over 118 nodes in 19.8 s). 7,450,581 shards × 118
nodes ÷ 1.19 million per second ≈ 740 s. A node does this only when the node list changes, and only
for deals it may hold; it can use all its cores.

### Measured gas (forge gas report medians, 2026-10-31)
| Action | Gas | Note |
|---|---|---|
| `answer` with 5 nodes | about 180,000 | small rows, short paths |
| `answer` with 128 nodes | about 448,000 + calldata | execution measured by `test_answerGasAtTheNodeCap` (2026-11-02, after the heap: was 518,726); the list adds 4 KiB of calldata |
| `answer` with 128 nodes, 125 declined | about 1.63 million + calldata | the worst case, `test_answerGasAtTheNodeCapWithDeclines` (was 5.97 million before the heap, THREATS STORAGE-S16-R1-2); `settleMissed` in the same case 1.59 million (review). With 5 nodes the heap costs about 3,000 more per answer than the scan did |
| `createDeal` | about 292,000 | includes the book checks |
| `register` + `activate` | about 106,000 + 218,000 | median; `activate` checks the joining node's share |
| `settleMissed` | about 162,000 | median; more when it jails |
| `decline` | about 52,000 | one shard or a whole deal |
| `recordSeed` | about 46,000 | once per epoch |

At 500 TB (684 answers per epoch with about 118 nodes) the answers cost roughly 684 × 0.6 million
≈ 400 million gas per epoch. What that costs depends on Base's gas price on the day, an input the
owner reads (section 11). Aggregated proofs (section 12) cut this by answering once per node.

## 11. Cost model (inputs, no invented prices)

The contract takes these at deploy; the deploy script's defaults are **placeholders** only.

| Input | Placeholder in the deploy script | Who sets it |
|---|---|---|
| Price per GiB per copy per epoch | 0.001 tONYX | Owner |
| Epoch length | 43,200 blocks (one day at 2 s) | Owner |
| Bond per declared GiB | 0.01 tONYX | Owner |
| Minimum bond | 10 tONYX | Owner |
| Slash per failed check | 10% of the bond | Owner |
| Failures in a row before jail | 3 | Owner |
| Checks per deal per epoch | 2 | Owner |
| Gas price on Base | (read on the day) | Market |
| Disk cost per TB-month for a node | (owner's estimate) | Market |
| Bandwidth cost per TB moved | (owner's estimate) | Market |

Formulas:
- Publisher pays per epoch: `price × ceil(max(dataset bytes, shards × 8 MiB) ÷ 1 GiB) × R`. Many tiny
  shards cost nodes work per shard, so each shard is billed as at least 8 MiB.
- A node earns per epoch, on average: `price × (GiB of shards it holds)`.
- A node spends per epoch: `(answers it sends) × (gas per answer) × (gas price)` plus its disk and
  bandwidth.
- A node is worth running when the second line is larger than the third. The owner picks the price
  so that it is, for the disk and gas prices of the day.

## 12. Phases

| Phase | What | State |
|---|---|---|
| v0 | Replicas (R = 3), rendezvous placement, on-chain row proofs, pay, slash, repair, reads with fail-over, bonded nodes, conservation on chain | **Built and tested on a local chain** |
| v0 live | Deploy on Base Sepolia, run five real storage nodes, deals for published datasets; nodes read only new chain logs each pass and keep their placement between changes; one node is the keeper | Chain and placement cache built; `scripts/storage-sepolia.sh` and the node service ready ([deploy-storage.md](ops/runbooks/deploy-storage.md)); contract deployed 2026-11-01 (`0x2423837572b58caf82896491ff6ee23080849419`, block 52986889); nodes funded; first `register` stopped on a public-endpoint limit, fixed (THREATS STORAGE-S15-1); **live 2026-11-01**: five nodes on the web host (/storage/n1..n5), three deals (btc.price, eth.price, btc.mvrv: 3 copies, epochs 2 to 44); epoch 13: 18 challenges, 18 passed, 0.036 tONYX paid, nothing slashed, `conserved()` true; the public endpoint's rate limit fixed (STORAGE-S15-4); **run finished 2026-11-02**: 612 of 612 challenges passed over 34 epochs, nothing slashed, deals closed, books balanced |
| Cheap holes closed | A different challenged row for each holder, a 2% registration fee so grinding ids costs money, 10% of each reported slash for the reporter, jail below half of a node's own bond (ADR-0060 amendment 1) | Built and tested on a local chain (2026-11-02); not deployed: the Base Sepolia contract keeps v0 (ROADMAP S16) |
| Sealed replicas | Each copy stored in a node-specific encoded form, so a node cannot fetch only when asked. Compared and not built: every sound design is too slow, needs seal units v0 does not have, or needs one key holder for every repair (ADR-0062). One copy per operator address was tried and withdrawn (THREATS STORAGE-S16-R1) | Later, after S17 (ROADMAP S21) |
| Erasure coding | Split each shard into k-of-n pieces (for example 10 of 16) instead of 3 full copies: about half the disk for more safety; each holder keeps a different piece, so one operator saves disk only with more than k pieces of a stripe (the fix for STORAGE-4, ADR-0062 finding 6); aggregated proofs (one answer per node per epoch); placement weighted by declared size | Later (ROADMAP S17) |
| Cold archive tier | Old, rarely read data kept on an outside storage network, with Onyx keeping only the fingerprints and a fetch path | Later, owner's choice of network (ROADMAP S18) |
| Store on first use | The publisher keeps the bulk until the first paid read (ADR-0055 item 2) | Later (ROADMAP S18) |
| Read payments | Nodes paid per read, so serving is rewarded, not only holding | Later (ROADMAP S19) |
| More than 128 nodes | Per-epoch committees or a proof of rank instead of the whole list | Later (ROADMAP S17) |
| Unbiasable seed | A randomness beacon the block producer cannot bias | Before mainnet (ROADMAP H2) |
| Mainnet | Outside audit (H5), keys behind a multisig (H1), a real token | After the owner decides (Phase H) |

## 13. Status (kept current)

| Part | Status | Evidence |
|---|---|---|
| Placement, proofs, repair, epoch accounting, event replay of the books (`crates/onyx-storage`) | Done | 27 Rust tests, shared vectors `fixtures/storage/vectors.json` |
| Contract `OnyxStorage` + `OnyxStorageLib` | Done (local) | 39 unit and fuzz tests, 4 invariants over 16 actions (decline, registration fee, cancel, rewarded reports and a steered miss-and-report included), 7 vector tests, a heap-order fuzz test, deploy script test; 21,700 bytes (limit 24,576; 2026-11-02, with the heap and ADR-0060 amendment 1) |
| Storage-node mode in onyx-node (`ONYX_STORAGE_*`) | Done (local) | `onyx_api::storage` tests: fetch only placed shards, refuse forged bytes, answer with proofs the contract accepts |
| Client read with fail-over | Done (local) | Test: down, missing and forging replicas skipped; demo: one node killed |
| Operator tool `onyx-storage` | Done | register, status, deal, epoch, settle, withdraw, exit, books, read |
| Local five-node demo (`scripts/storage-network-anvil.sh`) | Done | see "Last demo run" below |
| Adversarial reviews | STORAGE-R1 (9 findings) and STORAGE-R2 (a fresh review of those fixes, 6 findings): all fixed with tests except nodes that decline everything still counting in N (S16) | THREATS "STORAGE-R1", "STORAGE-R2" |
| Decline (`decline`, one shard or a whole deal) | Done (local) | 3 forge tests, 5 node tests, in the money invariants |
| Base Sepolia | `OnyxStorage` `0x2423837572b58caf82896491ff6ee23080849419` at block 52986889 (2026-11-01): epochs of 7,200 blocks, 0.001 tONYX per GiB per copy per epoch, bond 0.01 tONYX per GiB (10 tONYX minimum), slash 10%, 2 checks per deal per epoch, jail after 3 misses; `conserved()` true. Five node keys funded (0.00012 test ETH, 20 tONYX each). Epochs are 24 minutes (0.2 s blocks, STORAGE-S15-5). Five nodes run since 2026-11-01 (10 tONYX bond, 6 GiB each); three deals ran to block 53303689 (2026-11-02 01:30 UTC) and were closed (see "First live run" below); a longer run needs test ETH for the nodes first | ROADMAP S15, THREATS STORAGE-S15 |

### First live run (Base Sepolia, 2026-11-01 to 2026-11-02)
| What | Result |
|---|---|
| Nodes | 5 on one web host, 5 operator addresses, 10 tONYX bond and 6 GiB each; n1 also the keeper |
| Deals | btc.price, eth.price, btc.mvrv: 1 shard each, 3 copies, epochs 2 to 43, 0.126 tONYX deposit each |
| Epochs with checks | 34 (10 to 43, about 14 hours); no seed was recorded in 2 to 9, before the keeper ran, so those epochs had no checks and paid nothing |
| Checks | 612 of 612 passed (18 per epoch), 0 missed, 0 slashed, nobody jailed |
| Pay | 0.306 tONYX: n1, n3 and n4 0.068 each, n5 0.102, n2 nothing (it held none of the 9 copies) |
| Close | 0.072 tONYX refunded to the payer; every node withdrew its earnings; `conserved()` true; the contract holds the 50 tONYX of bonds |
| Gas left | nodes 0.00004 to 0.00012 test ETH; the next run needs a faucet top-up first |

### Last demo run (2026-11-01, local anvil, debug build, epochs of 40 blocks)
| What | Result |
|---|---|
| Dataset | 98,051 synthetic rows in 8 shards (4 MiB), R = 3 over 5 nodes |
| Placement | 5, 5, 6, 4 and 4 shards per node |
| First fetch | every node's shards fetched and checked in 3.4 s |
| First epoch | 6 challenges, all passed and paid, answered in 2.3 s; one answer 195,220 gas |
| Node killed | reads kept working (49,026 rows), failing over past it |
| Node with no data | its forged row refused on chain and slashed; slashed 6 times in all |
| Killed node | jailed; 7 copies of its shards repaired onto nodes 2-4, 1.2 s after the jail epoch |
| Money | `conserved()` true on chain; held 4.2265 tONYX = balance |
| Total | 113.6 s, `STORAGE NETWORK DEMO PASSED` |
