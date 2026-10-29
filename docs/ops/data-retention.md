# Data retention

## Scope

What Onyx stores, where, and for how long.

## Current state (M0)

Nothing stored yet. Storage design in [ADR-0007](../adr/0007-storage.md): Postgres for metadata and ledgers, a content-addressed store (CAS) for snapshots and recipe artifacts.

## Plan

| Data | Where | Kept |
|---|---|---|
| Recipe WASM artifacts | CAS (by recipe id) | Forever. Needed to recompute any old root |
| Source snapshots and fixtures | CAS (by content hash) | Forever while any finalized root depends on them |
| Label set versions | CAS | Forever, except a version holding a label removed on request: restricted until every root pinning it is past its challenge window, then deleted ([label-removal.md](label-removal.md), R-REMOVE-1) |
| Finalized rows | CAS + Postgres index | Forever; served under time-decay pricing |
| Evidence (signatures, filings) | CAS | Forever, unless a valid removal request applies |
| Ledgers (rewards, slashes, bonds) | Postgres | Forever |
| Level-2 round book (`--book`) and results store (`--store`) | Durable, backed-up disk; never ephemeral storage | Forever. A book lost after its round's receipts are on chain voids that fact (THREATS REVLOG-R5-1d, REVLOG-R9-F3); one runner per book |
| Service logs | Local disk | 30 days |
| API request logs | Local disk | 30 days, no request bodies |

Rules:

- No personal data. Labels cover entities only ([legal-data-rights.md](legal-data-rights.md)); the type system has no individual subject.
- Payer addresses from x402 are kept only as long as the ledger needs them.
- Content addressing means removal deletes the blob but the hash in a finalized root remains. The API then returns "removed" for that row.

## Open questions

- Storage growth per chain per year: measure in M1 from fixtures; no estimate yet.
- Off-site backup target and cost (needs approval if paid).
