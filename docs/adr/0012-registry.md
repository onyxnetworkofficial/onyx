# ADR-0012: Reviewed registries for entities, sources and address forms

## Status
Accepted, 2026-10-06 (M0 loop 3). Interface: `onyx_core::Registry`; in-memory impl `StaticRegistry`.

## Context
Loop 1 and 2 reviews showed that grammar checks alone cannot stop:
- self-declared rights (a vendor feed labelled as public chain data, R-RIGHTS-1, R-RIGHTS-2);
- labels or subjects naming people with well-formed ids (`entity:jane-doe`, R-PRIV-1);
- confusable entity ids (`entity:c0inbase`, R-ID-2) and invented quorum sources (R-QUORUM-2);
- non-canonical address spellings in claim subjects (R-SUBJ-1).

## Decision
1. Core defines a `Registry` trait: entity id -> `EntityKind`, source id -> reviewed `DataRights`,
   address namespace -> `AddressCanon`, known aggregates, known instrument namespaces.
2. `Claim::validate(&dyn Registry)` and `LabelSet::validate(&dyn Registry)` take it. Claim rights must
   equal the registered rights of the source. Subjects and label entities must be registered.
3. `EntityKind` moves to `onyx-core` (re-exported by `onyx-labels`). It still has no person variant.
4. Registry contents are reviewed data, changed by governance (docs/ops/governance.md), never by submitters.
   Adding an entity requires public evidence that it is an organisation.
5. Sector adapters provide `AddressCanon` implementations; core stays sector-agnostic.

## Alternatives
- Onchain registry from day one: needed later for permissionless operation; heavier now.
  The trait lets an onchain-backed implementation replace `StaticRegistry` without core changes.

## Consequences
- Every ingest path needs a registry handle. Tests build small registries.
- The registry becomes a governance and capture target; changes must be logged and reviewable.
