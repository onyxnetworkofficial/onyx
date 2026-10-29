# ADR-0017: Label-set versions link by content hash; label metrics publish a spread over >= 2 sets

## Status
Accepted, 2026-10-08 (M2 loop 15). Amended loop 16 (items 2 and 4). Implemented in `crates/onyx-labels/src/version.rs` and
`crates/onyx-labels/src/spread.rs`; demonstrated on real blocks in
`adapters/source-btc/tests/golden.rs::net_flow_spread_across_two_label_sets`.

## Context
North star item 6: label-set versioning with spread. A number that depends on labels (an exchange's
net flow) changes with the label set, and no single set is ground truth. Customers need to see how
much the number moves with the set. Old versions must stay retrievable while a root pins them
(labels.md, R-REMOVE-1).

## Decision
1. Versions: each published version of a named set is a `Manifest { name, version, content_hash,
   supersedes, published_round }` in an append-only `VersionLog`. Version `n + 1` must name the
   content hash of version `n`, be published in a later round, and change at least one label.
   `diff(old, new)` lists added, removed and changed labels.
2. Spread: a label-dependent metric is computed against at least 2 distinct label sets
   (`MIN_SETS`). `spread()` merges per-set rows by key and publishes each set's value plus the
   min and max. Sets must be distinct and ordered by content hash, so one set cannot be counted twice
   to fake a narrow band; rows must be canonical. Distinctness is judged on `labels_hash` (labels
   only, so a renamed copy is caught), and each set's values carry the root of the run that made
   them (loop 16, L15-F5).
4. Each manifest also names the previous manifest's hash, so the whole log, rounds included, is
   chained; `VersionLog::verify` checks a log received from elsewhere (loop 16, L15-F7).
3. The spread layer does not change recipes or the recipe input: each set is a separate run with its
   own shard and root.

## Consequences
- On the fixture blocks, OKX net flow is 148,783,915 sats with the full test set and 2,706,271,883
  sats with half of its OKX addresses (both matched by an independent Python recount,
  `scripts/recount-entity-flow.py`). The band shows how much the number depends on the labels.
- Which sets a metric's spread must use is a publishing policy, not yet enforced (M7 delivery).
- The version log is a library type; storing it onchain or in governance records is M6/M7 work.
