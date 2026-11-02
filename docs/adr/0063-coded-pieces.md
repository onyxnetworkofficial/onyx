# ADR-0063: Erasure-coded pieces instead of full copies (ROADMAP S17, first slice)

## Status
Proposed, 2026-11-02 (test network only); nothing is built. Reviewed twice the same day by fresh
adversarial reviewers, both "build with changes": the first draft (THREATS STORAGE-S17-R1: 3 High,
4 Medium) and the second (STORAGE-S17-R2: 2 High, 5 Medium; its lazy repair could not be checked
on chain and reopened the loss threshold). This is the third draft, with every change of both.
Before code: the assembly placement's gas measured, which decides how the contract is split
(decision 4). It answers the S17 item "erasure coding (k of n pieces instead of R
copies)". Capacity-weighted placement, aggregated proofs and committees past 128 nodes are later
slices. Research: RESEARCH.md "Storage network" (Storj v3, Walrus, the Reed-Solomon crates).
Plain-English master document: docs/STORAGE-NETWORK.md.

## Context
A v1 deal (ADR-0060 with amendments 1 and 2, deployed 2026-11-02 at
`0x9244eeaf5053f90ca2fbce157a3446f828c5ee43`) keeps R full copies of each shard on the R
highest-ranked nodes: R times the data on disk (3x for the live deals), and a shard is lost when
its R holders fail or one operator holds all R copies and drops them (THREATS STORAGE-4).

With k-of-n coding each holder keeps a different piece of about 1/k of the shard and any k pieces
rebuild it, at n/k times the data. What coding does and does not buy (review STORAGE-S17-R1):

- **Loss and withholding: n - k + 1 pieces.** A stripe becomes unreadable when n - k + 1 of its
  pieces are gone, so that, not k + 1, is what one operator must hold to destroy or ransom it. A
  wide code is much stronger than copies at lower cost; a narrow one is weaker. The review's exact
  numbers, for one operator with a fraction f of 128 nodes: f = 0.2 holds every copy of a v1 R = 3
  shard 0.76% of the time, enough pieces to kill a 10-of-16 stripe 2.9% of the time, and of a
  16-of-32 stripe 0.004%; at f = 1/3: 3.6%, 27% and 1.8%. So codes are allowed by n - k + 1
  (decision 1), and narrow codes are refused.
- **Fetching on demand (STORAGE-2) is not made dearer.** Reed-Solomon over these codes works
  position by position: cell c of any piece is a function of cell c of any k other pieces. A
  holder that keeps only its cells' hashes answers a check by fetching k cells (about 10 KiB), and
  an operator holding up to n - k pieces of a stripe can keep none while honest holders serve.
  Coding spreads pieces; it does not prove anyone keeps them. STORAGE-2 stays with S21 (replica
  proofs) and aggregated proofs (a later S17 slice).
- **Bandwidth grows k-fold.** Every new holder fetches the whole shard to check the encoding
  (decision 6), and a rebuild reads k pieces: first placement of 500 TB at 16-of-32 reads about
  16 PB (mostly from the publisher), against 1.5 PB for three copies; a failed 16 TB node costs
  about 256 TB of reads at k = 16, against 12.8 TB. Repair stays eager all the same (decision 7):
  lazy repair reopened the loss threshold (STORAGE-S17-R2).

## Decision (proposed, third draft)
1. **A deal names k and n.** `createDeal(header, payload, k, n, piecesRoot, epochs, sourceUrl)`.
   Allowed codes: `4 <= k <= 16`, `n >= 2k` and `n - k + 1 >= 13`, `n <= MAX_PIECES` (32):
   12-of-24 to 16-of-32 at 2x, wider at more. While a stripe is whole and `N >= 2n - k`, every
   allowed code is at least as safe as three copies against one operator with up to a third of the
   nodes (table below); the margin is thin (the crossover is at f = 0.35 for 12-of-24 and 0.38 for
   16-of-32). Coded deals need `N >= 2n - k` active nodes when made and every shard at least `k ×
   2 KiB` (smaller shards give padding-only pieces a holder passes without storing; a public salt
   would not help, it can be recomputed); other deals use v1 copies (R), which this deployment
   keeps (`k = 1, n = R`). If active nodes later fall below `2n - k`, the payer may close the deal
   at once with the unpaid deposit back (the code's safety needs that many nodes, Consequences).
   Billable size: the larger of `byteCount × n / k` and `shardCount × n × MIN_PIECE_BYTES` (a floor
   per piece, so many tiny pieces are not cheaper than their work), rounded up to whole GiB; the
   deposit is `price × billable GiB × epochs`. A new deployment (`RULES_VERSION` 4), never an
   upgrade.
2. **Encoding, pinned.** Each shard's bytes (the ADR-0059 shard file, L bytes) are cut into k data
   pieces of `P = 64 × ceil(L / (64 × k))` bytes (the last zero-padded) and n - k parity pieces by
   systematic Reed-Solomon over GF(2^16), Leopard algorithm, `reed-solomon-simd` pinned `=3.1.0`
   with its high-rate encoder chosen explicitly (its docs: high and low rate do not decode each
   other's output, and the default picks by the counts). An encoding id under `RULES_VERSION`
   names the crate version and rate; shared vectors cover every allowed (k, n) and run with each
   engine forced (AVX2, SSSE3, Neon, plain Rust); a node runs the vectors at start and refuses to
   serve on a mismatch. The shard floor of decision 1 keeps every data piece from being padding
   only.
3. **Commitments.** A piece is cut into 1,024-byte cells (the last zero-padded); its root is the
   `OnyxMerkle` root of its cells (ADR-0003; the root binds the leaf count, so the cell count can
   come from the answer, review L1). A shard's pieces root is the `OnyxMerkle` root of its n piece
   roots; the deal's `piecesRoot` is the `OnyxMerkle` root of the shards' pieces roots. The payer
   computes it; the contract stores it unchecked (it cannot decode).
4. **Placement: distinct holders by skipping.** Piece j of shard i has the key
   `keccak256("onyx-storage-piece-v1" ‖ shardKey(i) ‖ j)`. Pieces are placed in order 0..n-1;
   piece j goes to the eligible node with the fewest pieces of this stripe so far, then the highest
   weight for its key. Eligible: in the list, not jailed, not excluded from the stripe. Exclusion
   (slash, decline, notice) is per (deal, shard, node), as in v1, not per piece: a node out of one
   piece is out of the stripe (per-piece exclusion let it win another piece 10 to 43% of the time
   and cost 3x the gas, STORAGE-S17-R2). While at least n nodes are eligible every piece has its
   own holder; with fewer (any N, after declines) the same rule spreads pieces evenly. A leave moves
   the leaver's pieces and, rarely, a short chain (0.08 to 0.38 extra moves per forced move). Gas
   of placing pieces 0..31 at 128 nodes in a challenge (review STORAGE-S17-R2, forge harness):
   7.48 million with today's heap ported, 2.66 million as a plain loop, 1.09 million as the
   inline-assembly floor, against 0.23 million for v1's top 3; `OnyxStorage` is already 22,057 of
   24,576 bytes. So coded deals need an assembly placement and their own contract (or a library),
   decided by the assembly port's measured gas before code. Nodes keep placement incrementally.
5. **Challenges.** Each deal gets `challengesPerDeal × 3` one-piece slots per epoch (the answers
   per deal of a v1 R = 3 deal, so each node is asked as often as before and jail comes as fast).
   Slot s picks shard i as today, piece `j = keccak("onyx-storage-pslot-v1" ‖ seed ‖ deal ‖ s) mod
   n` and cell `c = keccak("onyx-storage-cell-v1" ‖ seed ‖ deal ‖ s ‖ node) mod cells`, and asks the
   piece's holder. Each node is then asked as often per deal as in v1 (review STORAGE-S17-R2: 6/N
   per deal-epoch), so jail comes as fast. Amendment 2's notice carries over unchanged and is
   weaker on coded deals (a piece is drawn about n times less often than a v1 copy): a forced
   slot in the notice epoch was tried in the second draft and dropped, because it made a noticed
   decline dearer than silence (a sure check against a 78% chance on a one-shard 16-of-32 deal),
   which breaks amendment 2's first rule. `answer` carries the cell and three Merkle paths (about
   35,000 to 40,000 gas more than v1, to be measured). A passed answer earns the deposit's per-epoch
   rate divided by the slots. Failure, miss, slash, jail and pay work per (deal, shard, piece,
   node); exclusion per (deal, shard, node) (decision 4).
6. **Wrong encodings: per shard.** A new holder of piece j fetches the shard (from the publisher,
   else the k data pieces, else any k pieces), checks it against the anchored manifest, computes all
   n pieces and keeps piece j only if the shard's piece roots give the deal's pieces root at index
   i. If they do not, it declines that shard (not the whole deal: one bad shard must not cost the
   publisher its good ones, review M3), with no slash. No honest node is slashed by a wrong
   encoding beyond STORAGE-16's existing window. A wrong encoding harms the publisher's data, the
   network's bandwidth and readers, not holders' bonds.
7. **Repair, eager, as in v1.** A piece moves when its holder is excluded, jails or leaves, or a
   join takes it (decision 4 names the new holder in every case); the new holder rebuilds it during
   the next epoch and is challenged from the epoch after (ADR-0060 decision 5's one-epoch rule).
   Lazy repair (second draft) was dropped: a stripe left at its repair threshold T needs only T - k
   + 1 pieces from one operator (7 for 12-of-24; at f = 1/3, 36 to 92% of stripes against 3.2 to
   3.4% for three copies), and the contract could not tell a moved, unbuilt piece from a held one
   without a table per stripe (STORAGE-S17-R2). The cost model in docs/STORAGE-NETWORK.md gets the
   k-fold repair bandwidth before any deploy.
8. **Reads.** A client reads the k data pieces first (systematic: no decode), else any k pieces,
   decodes, and accepts the shard only if it proves to the anchored manifest. A decode that fails on
   proven cells cannot name the bad piece; the client tries at most a fixed number of other subsets
   (a constant in the client), then reports the stripe as inconsistent.

## Consequences
- Disk and loss: chance that one operator with a fraction f of many nodes holds enough to kill a
  stripe (n - k + 1 of n pieces, distinct holders; binomial, so a large network):

  | Layout | Disk | f = 0.2 | f = 1/3 |
  |---|---|---|---|
  | 3 copies (v1) | 3x | 0.80% | 3.70% |
  | 10-of-16 (refused) | 1.6x | 2.67% | 26.3% |
  | 8-of-16 (refused) | 2x | 0.15% | 5.00% |
  | 12-of-24 | 2x | 0.022% | 2.84% |
  | 16-of-32 | 2x | 0.0033% | 1.64% |

  Exact figures for the weakest allowed code, 12-of-24 at f = 1/3 (review STORAGE-S17-R2): 0% at
  N = 36, 0.25% at 48, 0.57% at 64, 1.44% at 128, against 3.1 to 3.4% for three copies. These hold
  for a whole stripe on `N >= 2n - k` nodes. As honest nodes leave a deal made on more, distinct
  holders make the operator's share certain: a 16-of-32 deal made at N = 64 with an operator of 21
  nodes is lost 0.53% of the time at N = 56, 20% at 44 and 100% at 36 (three copies: 4.8%, 10%,
  19%); hence the payer's early close in decision 1.
- STORAGE-2 and the disk-saving half of STORAGE-4 are unchanged (Context). STORAGE-3 grinding gets
  cheaper in fees, not dearer: about 118 tries and 13 kept ids to win a whole 12-of-24 stripe at N
  = 115, and 126 tries and 17 ids for 16-of-32 at N = 111, against 421 to 425 tries and 3 ids for
  three copies (about 3.5x less in fees; more bonds are tied up, and they come back).
- Bandwidth: k-fold for placement and repair, with eager repair (decision 7).
- Gas: a challenge computes up to j × N placement weights and an answer carries a cell and three
  paths; both measured before deploy.
- Small networks (the live five nodes): coded deals need `N >= n + (n - k)`, so they stay copy
  deals until the network grows (12-of-24 needs 36 nodes, 16-of-32 needs 48).
- One pinned dependency with shared vectors; nodes store pieces; reads need k nodes up.
- Chain exposure: k answered cells at one position reveal that position of every piece (review
  L4); acceptable for public datasets, noted for any future private data.

## Alternatives
- **Independent per-piece rendezvous** (first draft): no extra moves, but 2 to 3 pieces of a wide
  stripe on one node and unsafe on small networks.
- **Pieces by rank:** distinct, but a leave shifts many pieces (4 to 17 extra moves per move).
- **A table of assignments on chain:** stable, but a table per stripe (ADR-0060 decision 4 avoids
  it).
- **Two-dimensional codes (Walrus RedStuff):** cheaper repair of one piece, about 4.5x overhead
  and a larger commitment; later, if repair bandwidth matters.
- **Keep R copies:** simplest; STORAGE-4 stays as it is.

- **Lazy repair** (second draft): less bandwidth, but a stripe at its threshold needs few pieces
  from one operator, and the chain cannot see unbuilt pieces (decision 7).

## Before code
1. Port the placement of decision 4 to inline assembly in a forge harness and measure it at 128
   nodes and n = 32 (the review's floor: 1.09 million gas); then choose the contract split.
2. Fix `MIN_PIECE_BYTES` and the shard floor with the gas and bandwidth numbers.
3. A third review of the decisions that changed (4, 5, 7) together with the measured gas.
