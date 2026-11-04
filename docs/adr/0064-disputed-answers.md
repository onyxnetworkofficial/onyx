# ADR-0064: Storage answers posted once per node and checked when disputed (ROADMAP S17)

## Status
Proposed, draft 2 (2026-11-03, test network only); nothing is built. Second review the same day
(THREATS STORAGE-S17-R6): "build with the listed changes"; they are in "Draft 3: changes from
review 2" at the end, and the build waits for them and for batched anchors (docs/TRACKER.md
2026-11-03). Draft 1 was reviewed the
same day (THREATS STORAGE-S17-R5: redraft, 3 High); every finding is taken below and marked with
its tag. It answers the S17 item "aggregated proofs (one answer per node per epoch, more
samples)". Research: RESEARCH.md "Answers checked only when disputed". Plain-English master
document: docs/STORAGE-NETWORK.md.

## Context
In the deployed contract (ADR-0060 with amendments 1 and 2, `0x9244eeaf5053f90ca2fbce157a3446f828c5ee43`)
every challenged holder sends one `answer` per challenged slot, and the contract checks each one
in full: the node list of the previous epoch (32 bytes per node in calldata, hashed against
`setHashAt(e - 1)`), the shard's ranking over every node (`_challenged`: one weight per node and a
heap), the two Merkle paths, then pay or slash. Measured execution (docs/STORAGE-NETWORK.md
section 10): about 180,000 gas at 5 nodes, about 471,000 at 128 nodes (plus 4 KiB of node list),
about 1.74 million in the worst case (128 nodes, 125 declined).

Two slots per deal per epoch is why sparse sampling stays open (THREATS STORAGE-14): more samples
multiply this cost. Sending many answers in one call with the same full checks shares only the
node list and the transaction base (about 15% at 128 nodes). The lever is to check fewer answers
on chain, as Filecoin does for Window PoSt ([FIP-0010](https://github.com/filecoin-project/FIPs/blob/master/FIPS/fip-0010.md)).

Two facts of Base Sepolia, read 2026-11-03: it applies EIP-7623's calldata floor (a call to an
account with 1,000 nonzero bytes estimates 61,825 gas, not the 37,000 of 16 gas a byte), so a call
that is mostly data pays about 40 gas a byte; and the EIP-2935 history contract
(`0x0000F90827F1C53a10cb7A02335B175320002935`) has code, so block hashes stay readable for 8,191
blocks, not 256: about 27 minutes, since Base Sepolia makes 5 blocks a second (STORAGE-S15-5).

## Rule that every part keeps (H1)
**Every input of the challenge of epoch e is final before e starts.** Who is challenged in e
(`_challenged`: the node list of e - 1, exclusions and their start epochs, jails) may depend only
on calls made before e begins. Nothing written later, not a claim, not a late dispute, may move a
past or current epoch's challenge. Each part below says which state it writes and when.

## Decision (proposed)
A new contract version, `OnyxStorage` v2 (`RULES_VERSION` 4), beside v1. v1 deals keep their
rules until they end. A v2 node registers and bonds in v2 (its own node list and capacity check);
one disk promised to both is the operator's risk, as with ADR-0063 (review STORAGE-S17-R4 L1).

1. **One post per node per epoch** (`post(e, node, leaves, answers)`, its operator, in epoch e,
   once per `(node, e)`, after `seedOf[e]` is recorded, not while jailed).
   - `answers` are typed and bounded (M3): for each item the shard's manifest line (at most
     `MAX_ENTRY` bytes), its path (at most 16 hashes: 65,536 shards), the row (at most `MAX_ROW`
     bytes) and its path (at most 32 hashes). Lengths are checked at posting; contents are not.
   - Each leaf is `keccak(0x00, RULES_VERSION, node, e, dealId, slot, keccak(answer))` (L3: domain
     separated). Leaves are strictly increasing by `(dealId, slot)`, so no slot twice. The contract
     builds `postRoot` over the leaves with the onyx-merkle tree shape the row paths already use
     (an unpaired last hash is carried up), and stores `postRoot`, the count `m` and the post's
     total pay weight (below) for `(node, e)`.
   - `post` must be a top-level call (`msg.sender == tx.origin`), so the answers are in the
     transaction's input and anyone can read them without traces (M3). It emits
     `Posted(node, e, postRoot, m)`.
   - It writes the decline notice's last pass for each item (`lastPass` of `(dealId, shard,
     node)`, the shard from `seedOf[e]`), in epoch e itself (H1), except for a key that already
     has a decline or a slash: then `lastPass` is frozen (H1). No node list and no ranking at
     posting.
2. **One spot check per post, weighted by pay, opened by anyone** (H3, M2, L2).
   - The seed of epoch e + 1 picks one item, each with probability `slotPay(dealId) / P`, where
     `P` is the sum over the post's items (computed at posting from each item's deal; an item
     whose deal does not exist, whose epoch is outside the deal's, or whose slot is at or above
     `challengesPerDeal` weighs 0 and is a fault on its own, below).
   - Anyone may open it, from the first block of e + 1 until the end of e + W, with
     `spot(e, node, leaf, path, answer, nodes)`: the leaf is checked against `postRoot`, then
     today's full check (node list of e - 1, ranking, both Merkle paths). The caller pays the gas;
     the keeper opens every spot as part of its round, as it records seeds today.
   - Pass: the post becomes payable. Fail (a wrong row, a slot the node is not challenged on, an
     item that names no running deal or no slot): the node is jailed from e + 2, slashed
     `min(bond, max(1, m) × slashPpm × bond)`, and the whole post is unpaid; the opener gets
     `REPORTER_PPM` of the slash (not the node's own operator).
   - Not opened by the end of e + W: anyone may call `settleSpot(e, node)`, with the same result as
     a failed spot (the node, or anyone, could have opened it).
   - Expected value of one bad item j (pay `p_j`, post pay `P`, spot slash `S`): the node gains
     `p_j` only if the spot misses j, and loses the whole post plus S if it hits: `p_j - (p_j / P)
     (P + S) = -(p_j / P) S`, below zero for every item, with no watcher. With S = m slashes, a
     garbage item costs about one slash in expectation, the price of a reported miss, so posting
     garbage is no cheaper than missing (H3 b).
3. **Disputes by anyone until the window closes.** Until the end of epoch e + W (W = 2 epochs,
   48 minutes at the deployed 7,200-block epoch of 0.2 s blocks; at least 12 hours on mainnet, which must outlast OP Stack forced inclusion of 12 hours
   and watcher downtime: L1), `dispute(e, node, leaf, path, answer, nodes)` runs the same checks
   as `spot` on any other item. Success: one slash as for a wrong answer today (10% of the bond),
   the item unpaid, the node excluded from that shard from the next epoch, the disputer paid
   `REPORTER_PPM` of the slash. The spot item cannot be disputed again (it is checked once).
   A dispute of a correct answer reverts; the disputer pays its gas.
4. **Misses.** `settleMissed` keeps its rule for a challenged holder of slot `(dealId, slot)` in
   e. For a node that posted, the reporter also shows the slot is not in the post: the two
   neighbouring leaves (or the first or last leaf) with their paths, O(log m), never the whole
   list (M3).
5. **Pay after the window, by a deadline** (H2). `claim(e, node, leaves)` from epoch e + W + 1
   to the end of e + W + C (C = 2 epochs), by anyone (the pay goes to the operator), once the spot
   passed: the leaves rebuild `postRoot`; each undisputed item of a running deal earns `slotPay`
   of its deal (capped by the deal's deposit as today); a `claimed` flag stops a second claim.
   The miss streak is reset only when e is later than the node's last slash epoch (H1). Pay not
   claimed by the deadline stays in the deal.
6. **Closing and leaving** (H2, M2). `closeDeal` is allowed from `endEpoch + W + C + 1` (the last
   epoch's claim deadline has passed), and refunds what is unpaid then. `withdrawBond` is allowed
   from `removedFrom + W + 2`, so a node's last spot and disputes settle against its bond.
7. **Seeds** (M1). `recordSeed(e)` reads the first block of e from `blockhash` or, past 256
   blocks, from the EIP-2935 history contract (8,191 blocks). If the seed of e + 1 is never
   recorded, the spot of e's posts uses the seed of e + 2; if that is missing too, e's posts are
   void: no pay, no slash.

Then raise the samples: `challengesPerDeal` from 2 to 8 costs about 1.8 times today's gas at
5 nodes and about two thirds of today's 2-slot gas at 118 nodes (table below), the step THREATS STORAGE-14 waits
for. In expectation every bad item loses money with nobody watching; a watcher turns "in
expectation" into "every time". STORAGE-14's gain therefore holds against rational nodes and needs
watchers against irrational ones.

## Estimate (arithmetic, to be measured in the build; M4)
Inputs: an answer's proof about 1.5 KiB; a call that is mostly data pays the EIP-7623 floor,
21,000 + 40 gas a byte; a full answer today is 21,000 + 16 gas a calldata byte + execution
(180,000 at 5 nodes, 471,000 plus 4 KiB of list at 128 nodes; about 168,000 + 2,370 per node in
between); one spot is one full answer; a claim about 50,000 + 10,000 per item; the per-item
`lastPass` writes at posting fall inside the data floor.

| Network, answers per node per epoch m | Today, m full answers | Proposed: post + spot + claim | Ratio |
|---|---|---|---|
| 5 nodes, 2 slots (m about 2.4) | about 0.55 million | 0.17 + 0.23 + 0.07 = 0.47 million | 1.2x |
| 5 nodes, 8 slots (m about 9.6) | about 2.2 million | 0.61 + 0.23 + 0.15 = 0.98 million | 2.2x |
| 118 nodes, 2 slots (m about 6) | about 3.3 million | 0.39 + 0.55 + 0.11 = 1.05 million | 3.2x |
| 118 nodes, 8 slots (m about 23) | about 12.7 million | 1.43 + 0.55 + 0.28 = 2.26 million | 5.6x |

At today's 5 nodes and 2 slots it saves little; it pays for more samples at any size and for scale.

## Consequences

### What it changes in the threat model
- **A wrong answer is no longer refused at once.** It is caught by the pay-weighted spot (with
  probability its share of the post's pay) or by any watcher within W epochs. Every bad item has a
  negative expected value without a watcher (part 2).
- **Pay waits W epochs and must be claimed within C more.** A node that leaves waits out its last
  window.
- **Repair.** A garbage poster is jailed from e + 2 when the keeper opens its spot in e + 1 (v1:
  excluded from e + 1), at the latest from e + W + 2 through `settleSpot`.
- **Unchanged.** A passed check shows a correct row from any source, not a kept copy (ADR-0062);
  answers are public in calldata as today.

## Options considered
- **A. Many full answers in one call:** about 15% at 128 nodes, no new trust. The fallback.
- **B. Posted answers, a pay-weighted spot, disputes (this ADR).**
- **C. One succinct proof for all answers (a SNARK or a KZG multi-opening):** a trusted setup or
  an audited proof system that fits the gas budget (ADR-0062's reasons). Not now.

## Open questions for the next review
1. Is a spot slash of m slashes too harsh for an honest node with one stale row, given it can check
   its own answers before posting?
2. Does the decline notice still earn its place in v2, now that `lastPass` is written at posting?
3. Build now or at scale: the live five nodes save 1.2x at 2 slots; the gain is the 8-slot sampling.
</content>
</invoke>

## Draft 3: changes from review 2 (THREATS STORAGE-S17-R6)
1. **The decline notice is computed once, at `decline`, and stored** (H1): later posts never move
   an exclusion. Items with no running deal or a slot at or above `challengesPerDeal` are refused at
   posting. A test checks that `_challenged(e)` gives the same holders read at the start of e and
   at e + W + 1 after arbitrary posts, declines and disputes.
2. **k = max(1, ceil(m x slashPpm)) spot items** per post, drawn by pay weight (M1), so a garbage
   item costs about one slash in expectation at every m; and the keeper checks every post off
   chain and disputes every fault: watching is the main defence for repair timing, the spot is the
   backstop when no one watches.
3. **The pick is provable** (M2): each leaf carries its prefix weight, computed by `post`; an item
   is a pick when `prefix <= r < prefix + slotPay` for `r = keccak(tag, seed, node, e, i) mod P`.
   `post` hashes the answers itself and never takes leaves from the caller. A successful dispute
   of a pick counts as a failed spot.
4. **Posts readable without traces** (M3): the sender has no code (`msg.sender == tx.origin` and
   `msg.sender.code.length == 0`, for EIP-7702), and `Posted` carries each leaf's `(dealId, slot,
   answer hash, shard)`, so node mirrors track `lastPass` from logs.
5. **Times in blocks, not hours** (M4): the epoch length is a deployment parameter, stated in
   seconds with Base's 0.2 s blocks; W covers at least 12 hours on mainnet. `recordSeed(e)` is
   allowed only within epoch e (a late seed must not open retroactive misses); the e + 2 fallback
   applies only after seed e + 1's window has passed, and `settleSpot` waits until both are
   decided.
6. **Smaller rules** (L1-L5): jail from the epoch after the spot is settled; the miss streak is
   reset when a spot of epoch e passes and no fault of an epoch at or after e is recorded, and a
   failed spot adds one miss; nodes open their own spots (the keeper only those still closed near
   the window's end); a failed spot voids the node's unclaimed posts; in a void epoch nobody is
   paid or slashed, non-posters included. The gas table adds the post's own storage (about 44,000)
   and uses m = 24 for 8 slots at 118 nodes (ratio 5.7).

