# ADR-0063: Erasure-coded pieces instead of full copies (ROADMAP S17, first slice)

## Status
Proposed, **parked** 2026-11-02 (test network only); nothing is built. Four fresh adversarial
reviews the same day (THREATS STORAGE-S17-R1 to -R4), each "build with changes" or "another draft".
The code rule, distinct holders, per-stripe exclusion and eager repair held from the third review
on; repair timing failed three reviews in a row (lazy repair, one-epoch rebuilds, a fixed lag), so
by the project's rule the design is parked rather than drafted a fifth time (docs/TRACKER.md
2026-11-02). Why parking costs nothing now: coded deals need at least `2n - k` nodes (36 for
12-of-24) and the live network has five. What a fifth draft must do is in "Parked: what is left".
It answers the S17 item "erasure coding (k of n pieces instead of R copies)". Research: RESEARCH.md
"Storage network". Plain-English master document: docs/STORAGE-NETWORK.md.

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

## Decision (proposed, fourth draft; parked)
1. **A deal names k and n.** `createDeal(header, payload, k, n, piecesRoot, epochs, sourceUrl)`.
   Allowed codes: `4 <= k <= 16`, `n >= 2k` and `n - k + 1 >= 13`, `n <= MAX_PIECES` (32):
   12-of-24 to 16-of-32 at 2x, wider at more. While a stripe is whole and `N >= 2n - k`, every
   allowed code is at least as safe as three copies against one operator with up to a third of the
   nodes (table below); the margin is thin (the crossover is at f = 0.35 for 12-of-24 and 0.38 for
   16-of-32). Coded deals need `N >= 2n - k` active nodes when made; other deals use v1 copies (R),
   which this deployment keeps (`k = 1, n = R`). Shards under `k × 2 KiB` give padding-only pieces
   a holder passes without storing (a public salt would not help, it can be recomputed); the header
   carries only the shard and byte counts, so the chain checks the average shard size and nodes
   decline any smaller shard under decision 6 (review STORAGE-S17-R3 L1). Early close: when a
   stripe has fewer than `2n - k` eligible nodes in the list in force (exclusions are permanent, so
   eligibility per stripe, not N, is what the safety needs), the payer may close the deal from the
   epoch after next, after a minimum of `MIN_CODED_EPOCHS` paid epochs; the holders keep the pay
   of every epoch until then (a payer that exits its own node to trigger a close at once would
   otherwise get back what holders spent fetching, STORAGE-S17-R3 M4).
   Billable size: the larger of `byteCount × n / k` and `shardCount × n × MIN_PIECE_BYTES`, rounded
   up to whole GiB, with `MIN_PIECE_BYTES` = 8 MiB, v1's floor per copy (`MIN_BILLED_SHARD_BYTES`):
   each piece is one holder's placement, fetch and checks, as a copy is, so many tiny pieces are
   not cheaper than their work (a 16-of-32 deal of small shards pays for 32 holders per shard,
   v1 for 3). The deposit is `price × billable GiB × epochs`. A new deployment (`RULES_VERSION` 4), never an
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
   weight for its key. Eligible for epoch e, as ADR-0060 decision 5 splits it: in the list of e - 1,
   not jailed and not excluded from the stripe by e - 1; a node jailed or excluded by e keeps its
   place and its count for e but is not asked (so its pieces move for e + 1, never into a holder
   asked in the same epoch; review STORAGE-S17-R3 M3; shared vectors pin this split, fewer than n
   eligible nodes and chain moves). Exclusion
   (slash, decline, notice) is per (deal, shard, node), as in v1, not per piece: a node out of one
   piece is out of the stripe (per-piece exclusion let it win another piece 10 to 43% of the time
   and cost 3x the gas, STORAGE-S17-R2). While at least n nodes are eligible every piece has its
   own holder; with fewer (any N, after declines) the same rule spreads pieces evenly. A leave moves
   the leaver's pieces and, rarely, a short chain (0.08 to 0.38 extra moves per forced move above
   the minimum N, 0.57 to 0.65 at `N = 2n - k`). Gas: weights hashed in inline assembly; on the
   first excluded winner the contract reads every node's exclusion once and masks them, so declines
   cannot force rescans (review STORAGE-S17-R3 M2: any operator can decline a whole deal for free,
   which pushed the lazy reads to 4.37 million gas; read once, 1.10 million with no declines and
   1.65 million at worst for the last of 32 pieces at 128 nodes, holders unchanged in all 14 test
   cases; v1's top 3: 0.23 million). Nodes keep placement incrementally. The contract split is in
   "Before code".
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
   node); exclusion per (deal, shard, node) (decision 4). The notice's last pass is kept with the
   piece index it was for (it fits in the `Excl` slot) and applies only to that piece: a node moved
   to another piece of the stripe by a chain is not under notice for a piece it never held (about
   78% of chain moves on one-shard deals would misfire otherwise, STORAGE-S17-R3 L2).
6. **Wrong encodings: per shard.** A new holder of piece j fetches the shard (from the publisher,
   else the k data pieces, else any k pieces), checks it against the anchored manifest, computes all
   n pieces and keeps piece j only if the shard's piece roots give the deal's pieces root at index
   i. If they do not, it declines that shard (not the whole deal: one bad shard must not cost the
   publisher its good ones, review M3), with no slash. No honest node is slashed by a wrong
   encoding beyond STORAGE-16's existing window. A wrong encoding harms the publisher's data, the
   network's bandwidth and readers, not holders' bonds.
7. **Repair, eager, and a copy where it can be.** A piece moves when its holder is excluded, jails
   or leaves, or a join or a chain takes it (decision 4 names the new holder in every case). When
   the old holder is still serving (a join, a chain move, an exit, a noticed decline) the new holder
   copies the piece from it, P bytes checked against `piecesRoot`; only jails, slashes and crashes
   need a rebuild from k pieces. A rebuild of a whole share is k-fold (a 128-node 16-of-32 network
   holding 500 TB would ask about 124 TB of a joiner, 11.5 Gbit/s over a one-day epoch), and a
   holder late with it would be slashed, excluded and moved again, a cascade (STORAGE-S17-R3 H1).
   Three immutable deploy parameters bound the work per epoch, all from one stated figure, the
   bandwidth a node gives storage work, `B` (proposed 100 Mbit/s, about 1.08 TB per one-day
   epoch; the operator guide states it as a requirement):
   - **Repair lag `G`.** A piece moved by a jail, slash or crash is challenged from `G` epochs after
     the move (a copy, from 1 epoch as in v1). After one node of the largest allowed capacity
     `C_max` is jailed, each of the other eligible nodes rebuilds about `k × C_max / (N - 1)`
     bytes; `G = ceil(k × C_max / ((N_min - 1) × B × T))` with `N_min = 2n - k`. For `C_max` =
     16 TiB and one-day epochs: about 6.0 TB per node, 5.5 to 5.6 days at 48 (16-of-32) or 36
     (12-of-24) nodes, so `G` = 6; at 128 nodes it would be 3 for 16-of-32 (2.05 epochs). During those epochs the moved piece
     is not checked (as in v1's one epoch, longer), and the stripe has n - 1 checked pieces.
   - **Join ramp.** A node activated in epoch `a` is eligible for a stripe in epoch `e` only when
     `keccak("onyx-storage-ramp-v1" ‖ node ‖ deal ‖ shard) mod J < e - a`, with `J = ceil(C /
     (B × T))` from its own declared capacity `C`: its stripes arrive evenly over J epochs (16 TiB:
     J = 17 at one-day epochs), each by copy, at most about `B × T` bytes per epoch. One hash per
     candidate, no state; stripes not yet eligible keep their holders.
   - **Minimum term.** Coded deals run at least `MIN_CODED_EPOCHS = 2k` epochs (32 for k = 16), and
     the early close of decision 1 cannot come before: each first holder reads the whole shard,
     k pieces' worth, and earns about one piece's worth an epoch, so 2k epochs repay that fetch
     twice over at the deal's own price (a rough rule, revisited with real prices).
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
1. **Placement gas, measured (done 2026-11-02).** The second review's forge harness, rerun: the
   holder of the last piece (j = 31) at 128 nodes, weights in inline assembly, exclusion read only
   for a winner, costs 1,086,402 gas with no declines, 2,231,624 with 64 of 128 nodes declined and
   3,360,893 with 96; the third review found that lazy reads reach 4.37 million when declines leave
   fewer than n eligible, and that reading every exclusion once caps it at 1.65 million (decision
   4). v1's top 3 cost 234,086.
1b. **The contract split, measured (2026-11-02, stubs that compile, solc 0.8.30 via-IR as
   `contracts/foundry.toml`).** The reviewer's one-contract stub was 27,867 bytes, 3,291 over
   EIP-170. Two options were built:

   | Option | Main runtime | Margin | Linked libraries | v1 gas |
   |---|---|---|---|---|
   | (a) one contract, v1's heap placement and the anchor checks also in libraries | 23,843 | 733 | three (4.6, 4.6, 2.9 KB) | `answer` +6.4k to +6.8k (N 5 to 128), `createDeal` +4.7k |
   | (b) a coded-only `OnyxStorageCoded`, placement and the three-path check in one library | 22,918 | 1,658 | one (4,606 B) | unchanged |

   Chosen, proposed: **(b)**. Twice the margin of (a) for the join ramp and later fixes, v1's gas
   and code untouched (the live contract at `0x9244…ee43` keeps running with no re-review), one
   library instead of three. Its costs: a node serving both contracts bonds and pays the
   registration fee twice under two ids, may promise one disk to both (capacity is counted per
   contract), and a jail or slash in one does not carry to the other; nodes track two seeds and two
   node lists; `N >= 2n - k` counts only the coded contract's nodes; the registry code is copied.
   Packing `k` and `n` with the payer keeps `createDeal` from paying for a new storage slot (+20,000
   gas otherwise). The deploy script checks the library's code against the build (it runs by
   DELEGATECALL on the contract's storage and tokens). Stubs: the session's scratch folder
   `split/` (not in the repository).
2. **Floors (done 2026-11-02).** `MIN_PIECE_BYTES` = 8 MiB (v1's floor per copy) and coded shards
   of at least `k × 2 KiB` (decision 1).
3. Reviews done: STORAGE-S17-R1, -R2, -R3 (this fourth draft takes R3's changes). A fourth review
   of decision 7's numbers (`B`, `G`, the ramp, the minimum term) together with split (b).

## Parked: what is left (review STORAGE-S17-R4)
A fifth draft, when the network nears `2n - k` nodes or aggregated proofs (a later S17 slice)
change what is checked, must:
1. Apply the repair lag to every move, not only jails (a whole-deal decline or an exit followed by
   refusing to serve turns a copy into a k-fold rebuild for free), and make it dynamic: a
   rebuild-debt counter on chain (each jail, exit and whole-deal decline adds about `k × committed
   / N / (N - 1)`; `B × T` drains each epoch; the lag is `1 + ceil(debt / (B × T))`), with handed-off
   pieces kept by the old holder for the lag.
2. Leave the join ramp off new deals (it gates only stripes of deals that started before the
   node's first epoch in the list) and cap declared capacity at `C_max` in `register`.
3. Delay a coded deal's first checks by `ceil(n × bytes / (N × B × T))` epochs and count first
   placements in the same debt.
4. Credit a rebuilt piece's skipped slots on its first pass after the lag, so a rebuild is paid.
5. Re-measure gas with the lag's second placement and the ramp (about 2.3 to 2.5 million per
   answer warm), and state `B × T` as one immutable in bytes per epoch.

