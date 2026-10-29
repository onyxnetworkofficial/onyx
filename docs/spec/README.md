# Protocol spec

One file per subsystem. Each has Scope, Interfaces, Assumptions and Open questions
(checked by `scripts/check-docs.sh`). Code is the source of truth for types; these files
explain the rules and the reasons.

| File | Subsystem | Built in |
|---|---|---|
| [claims.md](claims.md) | Claim model, two lanes, verification methods | M0 types |
| [recipes.md](recipes.md) | Recipe ABI, determinism, registry | M1 |
| [labels.md](labels.md) | Label sets, attestations, calibration, spread | M2 |
| [validation.md](validation.md) | Rounds, commit-reveal, sampling, canaries | M3-M4 |
| [disputes.md](disputes.md) | Bonded challenges, recompute, draws | M5 |
| [settlement.md](settlement.md) | Base contracts | M6 |
| [delivery.md](delivery.md) | API, proofs, x402, settlement endpoint | M7 |
| [pricing.md](pricing.md) | Per-call pricing, time decay | M7 |
| [rights.md](rights.md) | Data rights, entities-only rule | M0 types |
| [proof-ladder.md](proof-ladder.md) | Proof levels 1-5; level 2 re-observed feeds | Loop 57 (level 2 core) |
| [submissions.md](submissions.md) | Data submissions: onboarding questions, proof-level route, checks, pull policy, keyed sources | S1 (ADR-0038) |
| [paid-observation.md](paid-observation.md) | Observe on the first paid query: holds, tickets, refunds (draft) | not built (NS10, NL8) |
| [storage-tiers.md](storage-tiers.md) | Hot, warm and cold shards; recompute against the chain root (draft) | not built (NS13, NL10) |
| [browser-tls-prover.md](browser-tls-prover.md) | A keyed API answer proved in the browser with TLSNotary (draft) | not built (NS6, NL11) |
| [estimates.md](estimates.md) | The estimate layer: own namespace and level, method and error, never in checked totals (draft) | not built (NS14, NL3, C65) |
| [track-records-served.md](track-records-served.md) | Real track records on the daily level-2 facts (draft) | not built (NS16, NL4, C66) |
| [bounty-board.md](bounty-board.md) | Wanted data in coarse categories, never exact queries (draft) | not built (NS15, NL9, S10) |
| [more-datasets.md](more-datasets.md) | Candidate public sources for markets and insurance, with rights links and probes (draft) | not built (NL17) |
| [parametric-insurance.md](parametric-insurance.md) | A test policy that pays when an observed value crosses a threshold (draft) | not built (NL18) |
| [dataset-backing.md](dataset-backing.md) | Dataset backing: stake behind a dataset, earn from its fees, lose on a checked error (draft, NS12 input) | not built (S12) |
| [ladder-slice-2.md](ladder-slice-2.md) | Proof ladder slice 2: cross-lane rises, contradictions, level-3 ladders, `onyx-check` row levels | built 2026-10-28 (C64, NL13; ADR-0048 amendment 2), no `v: 2` route yet |
