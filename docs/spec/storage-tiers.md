# Spec: storage tiers, hot, warm, cold (draft, NORTH-STAR NS13, north-star loop NL10, ROADMAP S11)

Status: **Accepted as ADR-0052 (2026-10-28), slice 1 built** (`onyx_coordinator::tiers`); the node's
use of it is ROADMAP S11. Drafted 2026-10-27 by lane B. Prior art: RESEARCH.md "Lane B: paid observation, storage tiers, browser TLS
prover (2026-10-27)".

## Scope

A node holds every finalized shard's rows (`cas/`, `FsCas`, at most `MAX_ROWS_BYTES` per batch)
for as long as it runs. That does not scale with "any data". NS13 lets a node keep a shard at one
of three tiers and still answer every query with a value the client checks against the chain:

| Tier | What the node keeps | First answer | When |
|---|---|---|---|
| hot | the rows in `cas/` on local disk (today) | milliseconds | recently finalized, often queried |
| warm | the row batch compressed in cheaper storage (another disk, an object store) | seconds | queried now and then |
| cold | the finalized root (on chain), the recipe's pinned hash, the chain-input reference, and every pinned input that is not on a chain (label sets with their salts, spec params such as a reserve shard's base: small files, kept for good) | a recompute: the recipe run again over its input | rarely queried, cheap to recompute |

Out of scope: who pays for storage (NS12), the paid trigger of a recompute (NL8 gives the model:
a hold, then the work), level-2 observation stores (they are small JSON files and stay hot).

## Interfaces

- **Tier per shard** in the node's state: `hot | warm | cold`, with the time of the last move.
  `/v1/health` `indexer` gains `{hot, warm, cold, recomputing}` counts.
- **Moves.** hot to warm after `ONYX_TIER_WARM_AFTER` days without a query; warm to cold after
  `ONYX_TIER_COLD_AFTER`; any tier to hot on demand. A move never deletes the last copy before the
  next tier's copy is written and read back (its hash equals the batch hash).
- **Cold answer.** A query for a cold shard answers `202` with a ticket (the NL8 shape) while the
  node fetches the input, reruns the pinned recipe (`recipes/out`, by hash) in `onyx-recipe-host`,
  and checks that the rows hash to the **on-chain finalized root**; only then the shard goes hot
  and the ticket serves. A recompute whose root differs is refused and logged (a conflict, never
  served).
- **Which shards may go cold.** Only shards whose input can be fetched again byte for byte from a
  durable public source: chain data (Bitcoin and Ethereum blocks by height and hash) qualifies;
  an observation of a live web page does not (the page changes), so level-2 values never go cold.
  The shard's spec says `recomputable: true` with the input reference (block range and hashes).
- **Admission to cold (review NL10-1).** A shard may go cold only after the node has rebuilt it
  once from the referenced input and the kept pinned inputs and the rows hashed to the chain
  root; a shard whose rebuild fails stays warm and raises an alert. This proves the input
  reference and the kept files are enough before the rows are dropped.
- **Salted shards (review NL10-2).** A shard built from a format-3 label set (ADR-0037 amendment
  2) needs the salted rows (or the publisher's `set_secret`) to rebuild; the cold tier keeps the
  salted label set file, never the secret, and refuses to make such a shard cold without it.
- **Recompute is paid and limited (review NL10-3).** A cold answer needs a hold (the NL8 model);
  each account has at most M recomputes queued and the node runs at most N at once, each under the
  recipe host's fuel limit and the `MemoryMax` of the node's unit.

## Assumptions

- Recipes are deterministic (ADR-0009: no floats; the recipe host's fuel limit), so a rerun
  gives the same rows; the golden tests already rerun them.
- The finalized root stays readable on chain (Base keeps state; event logs may need an archive
  provider later, the way EIP-4444 moves old history out of execution clients).
- A Bitcoin block range is fetchable from any full node or from the committed fixtures; the
  input's hashes in the spec make a wrong input fail before the recipe runs.

## Open questions

1. Warm storage: a second local disk, or an object store (costs money: owner's call)? A local
   compressed folder needs no decision and gives the tier logic its tests.
2. Should the node keep the input for warm shards so that cold is only "drop the input", or drop
   both? Inputs are larger than rows for block recipes.
3. (Decided by review NL10-3: nobody recomputes for free; see Interfaces.)
4. Proof that the recompute ran is the root match; does the ladder (NL2) record a "recomputed"
   step, or is it the same proof as before? Leaning: same level, a note in the evidence.

## Threats the build must test

Review NL10 (lane B, 2026-10-27): THREATS NL10-1..4. A node that answers "busy" to every cold
request keeps a shard unavailable without lying (NL10-4): until several nodes hold shards (NS4),
the health watch counts cold requests not served within the deadline.


A requester who asks for many cold shards at once (paid and limited, a queue with N in flight), a
recompute that differs from the root (refused, a conflict alert, never served), an input source
that lies (the input hashes in the spec), a move that loses the last copy (write, read back,
then delete), a cold level-2 value (refused by the `recomputable` rule).
