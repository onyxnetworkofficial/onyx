# ADR-0062: Replica proofs: no sealed replicas yet, three cheap holes closed first, what R promises

## Status
Accepted, 2026-11-02 (test network only), after a fresh adversarial review (THREATS
STORAGE-S16-R2, 10 findings). Answers the first half of ROADMAP S16 ("sealed replicas", THREATS
STORAGE-2 and STORAGE-4). Sealed replicas are not built. Decision item 3 (three holes the review
found in v0) is the S16 work; each part is specified as an ADR-0060 amendment and reviewed before
code. Plain-English master document: docs/STORAGE-NETWORK.md.

## Context
In v0 (ADR-0060) a holder proves a shard by sending one plain row and its hash paths before the
epoch ends (7,200 blocks, 24 minutes on Base Sepolia; the seed is the epoch's start-block hash, so
the whole epoch is the answer window). Two gaps were known:

- **Outsourcing (STORAGE-2).** A node can keep nothing and fetch the row only when it is asked.
- **Colluding holders (STORAGE-4).** One operator whose nodes hold several copies of a shard can
  keep one disk copy and answer for all of them.

ROADMAP S16 named "sealed replicas" as the fix: every holder stores its copy in a form only it can
use, so a check of that form shows the holder has its own copy. "One copy per operator address" was
built and withdrawn first (THREATS STORAGE-S16-R1): any rule keyed on an address pays operators to
split addresses. This record checks the sealed-replica designs we know before any code.

A sealed replica can protect in one of two ways:

- **Time.** Making a holder's sealed form takes longer than the answer window.
- **Cost.** Making it when asked costs more than keeping it on disk between checks.

## Findings

Measured on the development machine (AMD Ryzen 5 5600X, one core, while other builds ran;
`openssl speed`, 2026-11-02): an RSA-2048 private-key operation takes 0.785 ms (1,274 per second);
SHA-256 of 32 bytes runs at about 5.3 million per second. Other numbers are arithmetic from stated
inputs; the reviewer checked each one.

1. **A chained seal fails against a holder that has the plain data.** In a chain, element i of the
   sealed form depends on element i - 1. A holder that keeps the plain data (a colluding operator)
   or fetches it (an outsourcer) keeps only every k-th sealed element, 32 bytes each, and rebuilds
   any other element from the one before it. If a whole seal takes T and the window is W, about
   T / W stored elements are enough (T / (W - fetch time) for an outsourcer): with T = 5 minutes
   and W = 30 seconds, 10 to 11 elements (about 350 bytes) stand in for a whole sealed copy. Each
   check then costs at most T / c of work for c stored elements, so the cost bound fails the same
   way. Slower steps change T, not this ratio.
2. **A sound time-bound seal needs what v0 does not have.** Sound designs (Filecoin's stacked
   depth-robust graph labelling, from its public specification; not measured here) give each
   element several far-back parents over many layers, so keeping a small part forces a long
   sequential rebuild. They need (a) a check that the seal was done right (a SNARK, or many spot
   checks of labels with their parents); (b) a seal time far above the answer window on the
   attacker's fastest hardware; and (c) seal units large enough to pay for (b). An answer is a
   transaction that must land despite busy blocks and a failed endpoint, so W cannot be much under
   30 seconds. At a minimum of T = 10 W = 5 minutes per unit, a 16 TB node holding 64 MiB shards
   sealed one by one does 238,419 seals, about 19,900 CPU-hours, again for every copy repair moves
   to it. Larger units (32 GiB: 466 per 16 TB) need far more than 5 minutes each to be sound, and
   no longer match v0's per-shard placement, repair and decline. A sealed holder also needs a plain
   copy (more disk) to serve reads quickly.
3. **Sealing by a trapdoor holder needs that holder for every repair.** In the
   Damgård-Ganesh-Orlandi design (2019) the publisher, or whoever holds an RSA trapdoor, makes each
   holder's replica, masked per holder; anyone decodes or checks it with the public key (on chain,
   one modexp per 256-byte block: about 5,500 gas under EIP-2565, more under newer pricing; Base's
   price not checked). No SNARK and no timing are needed. But every copy that repair moves needs a
   new replica from the key holder: one failed 16 TB node (about 12.8 TB of copies) is about
   10,900 CPU-hours of private-key work (0.785 ms per 256-byte block; about 850 CPU-hours per TB
   per copy). If the key holder is gone, no new holder can join that data.
4. **A secret key per holder breaks reads or durability.** If the publisher encrypts each copy
   with a key only it knows, a reader cannot decode a copy either, so honest holders must also keep
   the plain data, and the data can be rebuilt from the copies only while the publisher keeps its
   key.
5. **A cost-bound seal needs many more checks.** Sealing each chunk on its own (no chain) works
   only if each stored chunk is checked often enough that sealing it on demand costs more than
   keeping it. v0 checks 2 rows per deal per epoch, so a holder of a 65,536-shard deal is asked
   about a given shard about once every 32,768 epochs (about 1.5 years). Raising the rate needs
   one answer per node per epoch covering many rows (aggregated proofs, ROADMAP S17) or a lottery
   over a node's sealed chunks (a new pay design).
6. **Erasure coding answers STORAGE-4 without sealing** (review finding 5). With k-of-n coding
   (for example 10 of 16) every holder keeps a different piece and answers for a different piece.
   An operator saves disk only if it holds more than k pieces of one stripe (11 of 16 here), so the
   collusion that sealing was meant to stop needs about f^11 instead of f^3. Storj, Sia and Walrus
   rely on coded pieces and audits, not on proofs of replication. This is ROADMAP S17.
7. **v0 has three cheaper holes than the missing seal** (review findings 1 to 3, checked against
   the code):
   - **Node ids can be ground.** The id is `keccak(operator, index, blockhash(registration
     block))`. The operator sees that hash one block later, calls `activate` only when the id
     ranks high for a shard it wants, and otherwise lets the registration lapse:
     `cancelRegistration` returns the whole bond after 256 blocks. Registrations do not count
     against the 128-node cap, so tries run in parallel. Simulated tries to put R ids above every
     honest node on one chosen shard: median 21 (5 nodes, R = 3), 456 (118 nodes, R = 3), 1,339
     (120 nodes, R = 8). Every live deal so far has one shard, so one shard is the whole deal.
   - **Every holder is asked for the same row.** `slotRow(seed, deal, slot)` has no node id, so the
     first answer puts the row and its hash paths in calldata and the other holders copy it within
     the epoch, with no copy of their own and no download.
   - **The slash deters little.** A slash also takes the node off that shard, so a shard costs a
     holder at most one slash, of 10% of what is left of its bond. `decline` is free, so a holder
     that fetches on demand declines as soon as its source fails. `settleMissed` pays its caller
     nothing, and any passed check resets the count of misses toward jail.

## Decision
1. **No sealed replicas in S16.** A chained seal is unsound (finding 1); a sound time-bound seal
   needs seal units, timing and a proof system that per-shard placement does not have (finding 2);
   trapdoor sealing needs the key holder for every repair (finding 3); per-holder secret keys break
   reads or durability (finding 4); a cost-bound seal needs aggregated proofs first (finding 5).
2. **What R promises, stated the same way everywhere.** R is the number of node ids placed on each
   shard. In an epoch, those of them not excluded from the shard or jailed by then are asked (so
   sometimes fewer than R). A passed check shows that the node's operator submitted a correct row
   in that epoch, from any source. It does not show that the node keeps a copy: all R holders may
   keep none while some source serves the rows. The data survives while one holder or the
   publisher keeps it and stays reachable until a new holder copies it. THREATS STORAGE-2 and
   STORAGE-4 stay open with this wording; docs/STORAGE-NETWORK.md says it in plain words.
3. **Close the three holes of finding 7 first (S16)**, each as an ADR-0060 amendment, reviewed
   before code, with a redeploy on Base Sepolia when test ETH allows:
   - **Ids that cost money to grind.** Every registration pays a fee it never gets back, so each
     try at a better id costs part of a bond (ADR-0060 amendment 1 item 2).
   - **A row per holder.** The challenged row is drawn from the seed, the deal, the slot and the
     node id, so each holder of the challenged shard proves a different row (item 1).
   - **Slashes that bite.** The reporter of a miss gets 10% of the slash, and a node is jailed
     below half of its own activation bond (items 3 and 4). A slash for declining a shard the node
     has already proven was reviewed and moved to the decline-accounting design (THREATS
     STORAGE-S16-R3).
4. **The publisher's lever is R, for shards nobody targets.** Ids can still be ground at a price (item 3),
   an operator that runs a fraction f of N nodes holds every copy of a given shard with a
   probability of C(fN, R) / C(N, R), about f^R with many nodes (with 3 of 10 nodes and R = 3 it is
   0.83%, not 2.7%). A deal of S shards expects about S × f^R such shards:

   | f | R = 3, one shard | R = 3, S = 65,536 | R = 5, one shard | R = 5, S = 65,536 | R = 8, one shard | R = 8, S = 65,536 |
   |---|---|---|---|---|---|---|
   | 0.1 | 0.1% | 65.5 | 0.001% | 0.66 | 0.000001% | 0.0007 |
   | 0.2 | 0.8% | 524 | 0.03% | 21 | 0.0003% | 0.17 |
   | 1/3 | 3.7% | 2,427 | 0.4% | 270 | 0.015% | 10 |

   R is at most 8 (`MAX_R`). Erasure coding (finding 6) is the better lever and comes with S17.
5. **When to look again.** STORAGE-4: erasure coding in S17. STORAGE-2: new ROADMAP item S21,
   after aggregated proofs (S17) raise the rows each node proves per epoch; check a cost-bound seal
   (finding 5) first. Decline accounting (STORAGE-R2-2's residual) stays in S16 behind item 3.

## Consequences
- No sealing code. The v0 contract and the S15 live run are unchanged by this record; item 3
  changes the contract, so it needs a new deployment.
- The promise is narrower than "R copies", and every document says so.
- Outsourcing stays possible while a source serves rows; after item 3 it needs a download per
  check and risks a slash on every shard it has passed a check on.
- Revisit: ROADMAP S17 (erasure coding) and S21 (replica proofs).
