# ADR-0016: Calibration scoring is exact-integer Brier (primary) plus clamped log

## Status
Accepted, 2026-10-08 (M2 loop 13). Amended loops 14-15 after reviews (L13-F1, F6, F7; L14-F2, F3, F4): items 1, 3, 4, 7, 8.
Implemented in `crates/onyx-labels/src/score.rs`.
Answers PLAN question 4 with its stated default (Brier primary, log alongside).

## Context
Subjective labels are probabilities. The north star scores contributors on calibration against
later ground truth, never on agreement with each other (agreement rewards copying). Strictly proper
scoring rules make an honest probability the unique best report (Gneiting and Raftery 2007, RESEARCH.md).
Brier is bounded; the log score is unbounded at 0 and 1. Validators must all compute the same score,
and floats are banned (ADR-0009).

## Decision
1. A label `(address, entity, p)` committed by a contributor in round `c` is a forecast of who owns
   the address: `p` on the named entity, `1 - p` spread evenly over the other `K - 1` registered
   entities. `K` is `Registry::entity_count()` of the pinned registry, computed inside `score_all`
   (L14-F3). One forecast per contributor and address. Ground truth is a
   `Resolution { address, owner, published_round }`, built only from attestations that verify at
   their own `valid_from` (`resolutions_from_attestations`); its round is the earliest round of any
   such attestation, so a renewal cannot hide an earlier one (L14-F2). An address claimed by two
   entities is left out. Rounds come from the chain's record of finalized roots (M3).
2. A forecast counts only if `c < published_round`. `published_round` is the Onyx round in which the
   evidence was first finalized, not the statement's signer-chosen `valid_from`.
3. Primary score: Brier over the `K` entities, in [0, 2] (owner named: `(1-p)^2 (1 + 1/m)`; not
   named: `p^2 + 1 - (1 - p^2)/m`, `m = K - 1`), in units of 1e-12, each term floored. It is strictly
   proper in `p` (tested for K = 2, 3, 10, 1000), and "p = 0 for an entity that is not the owner"
   scores near the worst, not perfect (L13-F1). Contributors are ranked by cross-multiplying the
   integer sums and counts, never by a rounded mean.
4. Reported score: `-log2(probability given to the owner)` in micro-bits, clamped at 0.1%, integer
   log2 with 48 fractional bits, truncated. Reported only: truncation makes it not exactly proper at
   ppm resolution (L13-F7).
5. Inputs are canonical (forecasts sorted unique by contributor, address, entity; one resolution per
   address) and capped at 2^24 forecasts per call.
6. A score affects rewards only after 30 resolved forecasts over at least 5 owning entities.
7. Anti-farming: forecasts by a key registered to the owning entity do not count (self-resolution);
   at most 10 forecasts per (contributor, owning entity) count. This is not a Sybil defence: an
   entity using a fresh contributor key still scores perfectly on its own addresses (L14-F4). Scores
   therefore do not drive rewards until M4 binds contributors to stake.
8. Late rule scope: `committed_round < published_round` stops lookahead on Onyx's own record only.
   An attestation published elsewhere first can be copied before Onyx finalizes it; reward design
   (M4) must assume this (THREATS R-SCORE-1).

## Consequences
- Honest reporting is optimal under Brier exactly on the ppm grid (tested), and under the clamped log
  score on a grid away from the clamp (tested).
- Contributors can still choose which addresses to label (selection): someone who labels only
  obvious addresses gets a good score. Canaries assigned by validators (north star item 4, M4) and
  per-contributor coverage stats are the planned fix; until then scores are not yet tied to rewards.
- Ground truth is scarce at first (only attestations). Disclosures and onchain proofs of reserves can
  become resolution sources later, each with its own ADR.
- Labels where custody and beneficial ownership differ (an ETF's coins at a custodian) break the
  exclusive-owner assumption; label semantics must say which one a set means (labels.md).
