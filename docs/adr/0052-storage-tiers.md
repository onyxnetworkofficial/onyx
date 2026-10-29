# ADR-0052: Storage tiers: hot, warm and cold row batches, cold only after a rebuild matched

## Status
Accepted, 2026-10-28 (docs/NORTH-STAR.md NS13; docs/north-star-loop.md NL10; ROADMAP S11).
Implemented, slice 1, by `onyx_coordinator::tiers` (`TieredStore`, `RecomputeQueue`). The node
does not use it yet: the `/v1/...` cold answer (`202` with a ticket), the health counts and the
move schedule are the next slice (ROADMAP S11). Design draft: docs/spec/storage-tiers.md (lane B).

## Context
A node keeps every finalized shard's rows in `cas/` for as long as it runs. That does not scale
with "any data". Most row batches are recomputable: the recipe is deterministic (ADR-0009) and
its input is chain data that can be fetched again by height and checked against the pinned
hashes. A node should be able to drop rows it rarely serves and rebuild them on demand, without
ever serving a row that does not hash to the root on chain, and without losing data that cannot
be rebuilt.

## Decision
1. **Three tiers per row batch.** *Hot*: in the node's store. *Warm*: in a second, cheaper store
   (a `Cas` like the first; a local folder for now, spec open question 1: an object store costs
   money and is the owner's call). *Cold*: no rows. Only row batches move: recipes, label sets,
   attestations and params stay in the hot store for good, so a cold shard keeps what a rebuild
   needs. `TieredStore` is a `Cas`: rows are read hot, then warm.
2. **A move never drops the last copy first.** hot to warm and warm to hot copy the batch, read it
   back under the same key (the store rechecks the hash), and only then drop the old copy.
3. **Cold only after a rebuild matched (THREATS NL10-1).** warm to cold reruns the pinned recipe
   over the input fetched again and the kept files (`recompute::compute`) and drops the rows only
   when they hash to the finalized root on chain. A failed rebuild (input not fetchable, other
   bytes, a recipe that does not end `Ok`, another root) keeps the shard warm and returns the error
   for an alert.
4. **Only durable chain inputs (spec "Which shards may go cold").** A shard goes cold only with a
   recipe and a `Block` or `BlockRange` snapshot. A `Document` input (a page may change) and lanes
   without a recipe stay hot or warm. Level-2 observation stores are not row batches and never move.
5. **Kept inputs are checked (NL10-2).** Before a rebuild the recipe, the pinned label set file and
   every attestation it cites must be in the kept store; a salted format-3 set is kept as its file,
   never its secret, and a shard whose set file is gone stays warm.
6. **A recompute is checked and stored only on a match.** A cold shard's rebuilt rows are stored hot
   only when they hash to the chain root the caller read; a mismatch is a conflict, refused, never
   stored or served.
7. **Recomputes are paid and limited (NL10-3).** `RecomputeQueue` refuses a request without a hold
   (the NL8 model, ADR-0049: held, charged when served, released otherwise), caps each account's
   open requests and the node's queue, runs a fixed number at once, oldest first, and refuses the
   same shard twice for one account.
8. **Unserved requests are counted (NL10-4).** `RecomputeQueue::overdue` counts requests open past
   a deadline, for the health watch once the node uses the queue.
9. **The tier map survives a restart.** `save` writes root to tier as JSON through a renamed file;
   `load` refuses a hot or warm entry whose rows are not where it says, so a lost copy is found at
   start instead of at the first query.

## Consequences
- A node can keep finalized history without keeping every row batch.
- A cold answer costs a recompute: seconds to minutes, paid by the requester.
- The rebuild depends on the chain input staying fetchable (a Bitcoin full node, or the committed
  fixtures for the test data) and on the finalized root staying readable on chain.
- Not built yet (ROADMAP S11): the node's use of `TieredStore` (`ONYX_TIERS`), the move schedule
  (`ONYX_TIER_WARM_AFTER`, `ONYX_TIER_COLD_AFTER`), the `202` cold answer with an NL8 ticket,
  `/v1/health` `indexer.{hot,warm,cold,recomputing}`, the health-watch alarm on overdue requests,
  and compression of the warm copy. One process moves tiers at a time; two movers on one folder
  are not supported.

## Alternatives considered
- **Drop the input too and keep only the root** (spec open question 2): that is the cold tier; the
  warm tier keeps rows, not inputs, because block inputs are larger than rows.
- **Record a "recomputed" step on the proof ladder** (open question 4): no. A recompute that matches
  the root is the same proof as before; the level does not change.
