# ADR-0010: Sector-agnostic core, sectors as adapters and recipes

## Status
Accepted, 2026-10-06 (M0).

## Context
Onyx starts with crypto but must take equities, macro, commodities, prediction-market settlement
and private pooled data (Cypher) without a rewrite.

## Decision
- `crates/*` are core and never depend on `adapters/*` or `recipes/*`. `scripts/check-boundaries.sh`
  checks the dependency graph in CI.
- Sector specifics enter through `onyx_core::SourceAdapter` implementations and recipes.
- Offchain sectors use `Snapshot::Document` plus `OfficialSourceMatch` or `CrossSourceQuorum`.
- Every claim carries `DataRights`; `DataRights::check_ingest` refuses `Redistribution::Forbidden`.
- Adding a `VerificationMethod` variant is a core change and needs an ADR.

## Consequences
- Core names no chain, vendor or exchange. Chain-specific helpers live in adapters.
