# Spec: pricing and time decay

## Scope
Per-call prices and the rule that old data becomes free. Built in M7 (`onyx_api::catalog`,
ADR-0023 with its loop-39 amendment).

## Interfaces
- `DatasetPricing { flat_micro_usdc, floor_rows }`, `Catalog::export_price(meta, finalized_unix, now, rows)`,
  `Catalog::set_dataset_pricing`, `Catalog::default_dataset_pricing` (ADR-0025). Where fees go: revenue.md.
- `PricingPolicy { base_price_micro_usdc: u64, fresh_window_secs: u64 }` per metric: the price
  of one row.
- `PricingPolicy::price(age_secs) -> u64`: `base` if `age < fresh_window`, else `0`.
- Age = now minus the shard's clock start, in unix seconds (AUD-2-F2):
  `ShardMeta::clock_start(finalized) = min(finalized, data_time + MAX_DATA_LAG_SECS)` (one day),
  where `finalized` is the root's finalization time (`FinalRoot.finalized_unix`: the block time of
  the `finalized` block that recorded it on chain, or the settlement time of the off-chain ledger)
  and `data_time` is `ShardMeta::snapshot_unix`: a document snapshot's bound `published_unix`, a
  block snapshot's end-block header time. An `onyx-node` catalog file gives the end block's
  80-byte header as `endHeader` (AUD-2R-F2): the node checks that it hashes to the snapshot's
  tip hash and reads the time from it; a stated `snapshotUnix` must equal it, a block snapshot
  without a header (or of a chain with no header format, today anything but `btc.*`) is refused,
  and any data time after now + 2 hours (`MAX_FUTURE_SECS`) is refused, a document's attested
  `published_unix` included. The coordinator cannot restart the clock by finalizing old data
  late: data older than a day is aged from its own time. Without a data time the clock is
  `finalized`. Row answers carry `snapshotUnix` (informational) so a client can show the clock.
- `Catalog::price(meta, finalized_unix, now, rows)`: the metric's policy (else
  `default_pricing`, else free) times `rows`, saturating. Proofs, metrics and settlement are one
  row; an export is `leafCount` rows.
- A metric lookup is priced from the newest finalized candidate shard (before the subject is
  looked up, so the price says nothing about it), even when the row comes from an older shard.
- Amounts go to x402 as a decimal string in USDC atomic units (6 decimals), e.g. `2500` = 0.0025 USDC.
- Level-2 observations (`Catalog::observation_price(metric, settled_unix, now)`, ADR-0027
  amendment 4): one row of the metric's policy, aged from `settledUnix` =
  `FeedTemplate::decay_start(subject, window_end)`.
  **Invariant (L2API-R3-F1): the moment level-2 data becomes free is a pure function of the
  registered template and the subject.** No input an operator, round opener, observer or client
  chooses can move it:
  - a **dated** subject (one fact per record date) ages from the end of the *earliest* window its
    lag allows for that date (`DateLag::earliest_window_day` + `window_secs`). A round opened
    later in the lag (up to `max_calendar_days`) observes the same fact and prices identically;
    delaying a round only shortens the paid time it can sell.
  - a **fixed** subject has one fact per window; each window ages from its own `window_end`.
    A served round's window can never end in the future: its commit close is at or after
    `window_end` and its reveal close at most `MAX_CLOCK_SKEW_SECS` after the checker's clock, so
    `window_end <= now + 300` (an upper bound; L2API-R4-F3). A newer window is newer data, not
    a reset clock; older, now-free windows stay reachable with `as_of` or `window_end`, and a
    client's `min_window_end` floor is applied before payment (L2API-R4-F2).
  - the template that sets the clock is the one pinned for the metric
    (`ONYX_OBSERVATION_TEMPLATES`, re-read from `OnyxRegistry.metrics` when the store names a
    registry); a store with another template, e.g. another lag, is refused at load (L2API-R4-F1).
  Round config (commit and reveal closes) is never a price input (L2API-R2-F2). A round whose
  reveal close is after the checker's clock plus `MAX_CLOCK_SKEW_SECS` (300 s) is not served at
  all (L2API-R1-F2).

## Rules
- Integers only (micro-USDC).
- Data older than the window is open: free, still with proofs.
- `age` saturates at 0: a finalization time in the future is still fresh, never free early.
- Residuals (THREATS AUD-2-F2, AUD-2R-F2): a document's `published_unix` is attested by the
  source adapter, not proven, only bounded by now + 2 hours; a block header's time is the miner's
  (consensus bounds it to about two hours); in-process demo catalogs keep the finalization clock.
- An export costs as much as buying each row, unless the metric (or the catalog default) has a
  `DatasetPricing` (ADR-0025): then `min(rows * per_row, max(flat, min(rows, floor_rows) * per_row))`
  with the decayed per-row price. Never more than every row, never less than `floor_rows` rows, free
  once old. Defaults: flat 20,000 micro-USDC (2 cents), floor 4 rows; recommended per-answer price
  2,500 micro-USDC (0.25 cents, `RECOMMENDED_ANSWER_MICRO_USDC`). The site reads all three from
  `fixtures/revenue/pricing-defaults.json`, drift-checked against the code (AUD-2-F11).
- A contributor pass (revenue.md R-REV-5) discounts fresh prices for addresses with accepted work.
- A catalog that cannot be read is an error (500), never price 0.
- `Restricted` data is not served at any price (delivery.md), so it never decays to free.

## Assumptions
- A step function is enough for the prototype; smoother decay can come later.
- The indexer records `leafCount` with each shard, so an export is priced before any row is read.

## Open questions
- Default window per sector (crypto metrics vs. macro releases).
- Paid licenses for `Restricted` data (now never served; see Rules).
- Bulk discounts for exports once streaming exists.
