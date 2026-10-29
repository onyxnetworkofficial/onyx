# ADR-0050: The estimate layer: estimates live apart, level `E`, never in a checked value

## Status
Accepted, 2026-10-28 (docs/NORTH-STAR.md NS14; docs/north-star-loop.md NL3; ROADMAP C65). First
slice implemented by `onyx_core::estimate`, `onyx_api::catalog::Catalog::add_estimates`,
`GET /v1/estimates/{metric}`, the refusals in `ShardMeta::from_spec`, `Catalog::add_shard`,
`Catalog::add_observations`, `onyx_labels::validate_label`,
`onyx_coordinator::recompute::preflight` and `onyx_track::compute`, and the data page's
estimate card (`apps/web/src/lib/estimates.ts`, `EstimateCard.svelte`). Design draft:
docs/spec/estimates.md (lane B).

## Context
The partners asked that estimates never pollute checked totals (NORTH-STAR section 1). Some useful
values cannot be checked: a model's output, an interpolation, a heuristic label's total, an
average that speaks about days nobody measured. Onyx may serve them, but a buyer must never take
one for a checked value, and no checked value, total, market or track record may rest on one.

## Decision
1. **Namespace.** A metric is an estimate if and only if its id starts with `estimate.`
   (`onyx_core::is_estimate_id`, R-EST-1). The same prefix marks an estimate used as a source
   (`SourceId::is_estimate`) and, as the evidence kind `estimate` or `estimate.*`, a label that
   rests on an estimate.
2. **Level `E`, outside the ladder.** There is no `ProofLevel` variant for an estimate, so no code
   that matches on levels can meet one, and an estimate never has a ladder step (R-EST-4). The API
   answer states `level: "E"`, `proofLevel: null`, `ladder: null`, `checked: false`.
3. **Method and error, always.** `Estimate` carries a method id, the hash of the method's text or
   program, an error band (`Interval { lo, hi, coverage_ppm }` or `Unknown`) and the hashes of its
   inputs. `Estimate::validate` refuses a band that does not hold the value, ends of another kind,
   a coverage outside 1..=999,999 ppm, a zero method hash and unsorted inputs. `Unknown` is shown as
   "error unknown", never left out (R-EST-3). Values are integers and fixed point (ADR-0009).
4. **Never an input of a checked value (R-EST-2).** Each entry point refuses one:
   `ShardMeta::from_spec` and `Catalog::add_shard` (a shard under an estimate metric),
   `Catalog::add_observations` (a level-2 feed), `preflight` (a recipe whose source is an
   estimate), `validate_label` (an estimated label in any label set, plain or dated), and
   `onyx_track::compute` (a resolution by or about an estimate).
5. **Served apart.** Estimates live in their own catalog map and answer only at
   `GET /v1/estimates/{metric}?subject=...`; every other value route answers an estimate metric with
   404 `unknown_metric`, and the estimates route answers a checked metric the same way. Free on
   testnet (an estimate costs no round).
6. **On the site.** The data page shows estimates in their own section, each card marked
   "Estimate, not checked", with method, band and inputs and no level badge. The first one is made
   in the reader's browser from checked figures: the mean of the daily fees over the whole UTC days
   of the checked Bitcoin window, with a 95% Student's t interval, integer satoshis throughout.

## Consequences
- A buyer can always tell an estimate from a checked value by its name alone, and a client that
  reads `proofLevel` gets null, never a number it could compare to a checked one.
- Adding an estimate never moves a checked total: the totals are computed from checked rows and
  checked observations only, and no checked map can hold an estimate.
- Left (ROADMAP C67): loading estimates into `onyx-node` from a file, a price policy for estimates,
  an on-chain registry entry for method ids (spec open question 3), and the single-scan reserve as
  an estimate once label rights allow it.
