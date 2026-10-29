# Spec: the estimate layer (draft, NORTH-STAR NS14, north-star loop NL3, ROADMAP C65)

Status: **Built, first slice 2026-10-28** (ADR-0050; lane B drafted it 2026-10-27). The method
registry, node loading and a client rerun of a pinned method are left (ROADMAP C67). Prior art: RESEARCH.md "Lane B batch 2 (2026-10-27)".

## Scope

The partners asked that estimates never pollute checked totals (NORTH-STAR section 1). An
estimate is a value Onyx did not check: a model's output, an interpolation, a heuristic label's
total. It may be useful and it may be sold, but:

- it lives in its own **namespace**: every estimate metric id starts with `estimate.`, and no
  other metric may;
- it has its own **level**, `E`, which is **not** a rung of the proof ladder (levels 1-5 stay as
  they are, on chain and in Borsh); an estimate never rises into the ladder, and a ladder never
  holds an `E` step;
- it always carries its **method** (a pinned id and the hash of the method's text or program) and
  its **error** (an interval with its stated coverage, or `unknown`, shown as such);
- it is **never an input of a checked value**: no recipe input, no label set used for checked
  totals, no level-2 template, no track-record resolution, and no total on the site.

Out of scope: which estimates Onyx publishes first (open question 1) and their price.

## Interfaces

- **`crates/onyx-core/src/estimate.rs`** (new, sector-agnostic): `Estimate { metric, subject,
  as_of, value: Value, method: MethodId, method_hash: Hash32, error: ErrorBand, inputs:
  Vec<Hash32> }`, `ErrorBand::{Interval { lo: Value, hi: Value, coverage_ppm: u32 }, Unknown}`,
  `MetricId::is_estimate()` (the `estimate.` prefix), `Estimate::validate()` (prefix, `lo <= value
  <= hi`, same value kind, coverage in 1..=999,999 ppm, sorted unique inputs). Borsh like every
  hashed structure (ADR-0002); no floats (ADR-0009). Not a `ProofLevel` variant: adding one would
  renumber nothing on chain but would let an estimate reach code that matches on levels.
- **`crates/onyx-api/src/catalog.rs`**: `Catalog::add_*` for checked data refuse an `estimate.`
  metric; a new `Catalog::add_estimates` refuses any other metric. `crates/onyx-api/src/lib.rs`:
  `GET /v1/estimates/{metric}?subject=...` answers `{metric, subject, value, level: "E",
  proofLevel: null, ladder: null, method, methodHash, error, inputs}`; every other value route
  refuses an `estimate.` metric with 404 `unknown_metric`. `docs/api/openapi.json` and
  `scripts/check-openapi.py` cover the route.
- **Recipes and labels.** `crates/onyx-labels`: a label whose provenance is a heuristic (cluster
  inference, a model) carries `provenance: estimate` and is refused by the label-set builder for
  any set a checked shard pins; `crates/onyx-coordinator/src/recompute.rs` (`preflight`): a recipe
  input whose id is an `estimate.` metric is refused before a round opens.
- **Track records.** `crates/onyx-track`: `Resolution.level` already must be 1-3; a resolution
  whose evidence names an estimate is refused (an estimate never judges a forecast).
- **Site.** `apps/web/src/lib/components/EstimateCard.svelte` (new): the value, "Estimate, not
  checked", the method and the error band, no level badge from the ladder. `apps/web/src/lib/data.ts`
  and `catalog.ts`: totals are typed over checked rows only, so an estimate row cannot be summed.

## Rules

1. **R-EST-1.** A metric is an estimate if and only if its id starts with `estimate.`.
2. **R-EST-2.** No checked value (levels 1-4) and no level-5 resolution has an estimate among its
   inputs, label sets or evidence; each entry point refuses one.
3. **R-EST-3.** An estimate answer always shows its method and error; `Unknown` is shown as
   "error unknown", never left out.
4. **R-EST-4.** An estimate never gets a ladder step and never states a proof level.

## Tests that prove the Done-when

- `onyx-core::estimate::tests::an_estimate_outside_the_namespace_is_refused` and
  `::a_band_that_does_not_hold_the_value_is_refused`.
- `onyx-api` `tests::a_checked_catalog_refuses_an_estimate_metric`,
  `tests::estimates_route_answers_method_and_error`,
  `tests::a_value_route_refuses_an_estimate_metric` (the "API refuses to mix them" test).
- `onyx-labels` `a_checked_label_set_refuses_an_estimated_label`; `onyx-coordinator`
  `recompute::tests::preflight_refuses_an_estimate_input`; `onyx-track` `a_resolution_from_an_estimate_is_refused`.
- e2e "an estimate is shown apart and never in a total" (`apps/web/tests/`).

## Assumptions

- Today no shipped metric is an estimate, so nothing existing moves namespace.
- A method can be pinned by hash (a document or a program); a reader can see what was done.

## Open questions

1. The first published estimate. Candidates: a 30-day trend of a level-2 daily value with its
   interval (an interpolation, honest about error); an exchange's reserve that only one node's
   scan backs (today 403 `not_cross_checked`, THREATS RES-1) shown as an estimate with
   `Unknown` error. The second is label-derived, so it stays withheld while `ONYX_RIGHTS=fallback`.
2. Should estimates be priced like level-4 claims or free? They carry no proof cost.
3. Does an estimate's method id need a registry entry on chain, or is the hash in the answer
   enough for a testnet?
