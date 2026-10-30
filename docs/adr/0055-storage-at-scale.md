# ADR-0055: Storage at scale: design inputs for the CL5 implementation

## Status
Proposed, 2026-10-29 (docs/contributor-loop.md CL5). A short design note only: CL5 is being
implemented in parallel under its own ADR, which supersedes this one wherever they differ. This
note records the inputs and the open questions the implementation should answer. Testnet only.

## Context
Own-data samples are capped at 8 KiB (ADR-0040, which leaves larger payloads to a separate
decision); recipe row batches at `MAX_ROWS_BYTES` = 24 MiB + 4 KiB (`onyx_coordinator::cas`).
ADR-0052 lets a node drop row batches it can rebuild from chain input, but contributed bulk
cannot be rebuilt. The contributor loop asks for a shard size limit, store on first use, a
storage deposit per GB and node tiers (archive, full, pruned).

## Decision
Inputs for the implementation, each building on something that exists:
1. **Shards of rows, at most 64 MiB.** Rows (UTF-8 CSV or JSON Lines, ADR-0040's formats), each
   at most 64 KiB; a shard's root is the ADR-0003 Merkle root over its rows, so a row is sold with
   a proof as shard rows are. A new CAS kind `bulk` with its own cap; recipe batches keep 24 MiB.
2. **Store on first use through ADR-0049.** Until a paid query, Onyx keeps only a manifest (shard
   roots, row and byte counts, rights, the 8 KiB sample's hash), reviewed like any submission
   (ADR-0039). A paid query on an unstored shard is a prepaid hold; the submitter's CL2 client
   uploads the wanted shard (or the node pulls a mirror through ADR-0053's `onyx-fetch`); the node
   rebuilds the root and runs the ADR-0040 guards over every row before serving; only then is the
   hold charged.
3. **Availability failure.** No shard by a deadline no longer than the ADR-0049 hold limit: the
   hold is released in full and the listing gets a strike. **A slash never goes to the buyer**:
   a buyer paid for a failure could grief an offline submitter for profit. A guard hit is not a
   strike (a guard can be wrong); it holds the listing for review.
4. **Deposit per GiB, earned back by use, slashed when idle.** A listing locks
   `DEPOSIT_PER_GIB` tONYX per declared GiB; paid uses release it; a listing with no paid use
   after `IDLE_PERIOD` expires and keeps `IDLE_SLASH_PPM` for the treasury. Refunds may be paid
   into the submitter's credits (ADR-0056). Every number is a placeholder (NS12).
5. **Node tiers on top of ADR-0052:** archive (row batches hot or warm, never cold; every fetched
   bulk shard kept), full (ADR-0052 as built; fetched bulk kept hot or warm), pruned (rows only
   within a window, a cache for bulk, older rows by recompute or from an archive node). **Bulk is
   never cold**, and a node drops a fetched bulk shard only when enough archive nodes advertise
   it; with one node today the hosted node is archive and the submitter is the second copy.

## Consequences
- The implementation ADR decides the numbers, the contract and the routes; this note does not
  bind it.
- Threats the implementation should cover: an uploaded shard with another root, the upload route
  used to exhaust disk, catalog spam with huge declared sizes, a buyer griefing an offline
  submitter, personal data inside a large shard that only the sample was reviewed for, the last
  copy of bulk dropped, a false strike by the reporter.

## Alternatives
Upload everything at listing (Onyx stores spam); always fetch from the submitter (every query waits
on one machine); fixed byte chunks instead of rows (a buyer gets a chunk, not a row with a proof).

## Open questions
1. Is 64 MiB per shard right, and what is the per-dataset ceiling?
2. Strikes rest on the operator's word that a shard did not arrive (as reveal receipts rested on
   the coordinator, REVLOG-1): is a small slash with a public evidence hash enough on testnet?
3. Who pays for keeping a shard after its first use: a storage share of each fee in `OnyxRevenue`?
4. Warm storage on an object store costs money (ADR-0052 open question 1): the owner's call.
