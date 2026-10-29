# Onboarding a new chain or sector

## Scope

Checklist for adding a chain (ETH, SOL, BNB) or a sector (equities, macro, commodities, prediction markets, Cypher).

## Current state (M0)

Only `adapters/source-btc` exists, as a stub. Boundary rule ([ADR-0010](../adr/0010-crate-boundaries.md)): core crates never depend on adapters or recipes; `scripts/check-boundaries.sh` enforces it in CI.

## Plan

Checklist:

1. **Sector doc**: write `docs/sectors/<sector>.md`: metrics, subjects, sources, verification method per metric, rights.
2. **Rights review**: for every source, fill `DataRights`. If redistribution is forbidden, stop. See [legal-data-rights.md](legal-data-rights.md).
3. **Verification method**: pick from the existing `VerificationMethod` variants:
   - chain data → `Recompute`
   - official publication → `OfficialSourceMatch` with `Snapshot::Document`
   - several independent sources → `CrossSourceQuorum`
   - entity self-signed → `FirstPartyAttestation`
   - judgment → `CalibratedJudgment`
4. **Adapter**: new crate `adapters/source-<name>` implementing `onyx_core::SourceAdapter`. Fixtures first, live source later.
5. **Recipes**: new crates under `recipes/`, with golden tests and determinism tests.
6. **Fixtures**: pinned snapshots with checksums under `fixtures/<name>/`.
7. **Threats**: add sector-specific attacks to `THREATS.md`.
8. **No core change.** If a core change seems needed, write an ADR first and get it approved.

## Open questions

- Should each sector have its own label set namespace (`onyx.entities.eth`) or one cross-chain entity registry with per-chain address sets?
