# Legal and data rights

## Scope

Licensing of ingested data, redistribution, privacy (entities only), and removal requests. Not legal advice; a lawyer reviews before mainnet ([upgrades.md](upgrades.md) gate).

## Current state (M0)

- Every claim carries `DataRights { license, redistribution, source, attribution, terms }` (`crates/onyx-core/src/rights.rs`).
- `DataRights::check_ingest` is the gate on every ingest path:
  - `Forbidden` → refused. Onyx never ingests data whose license forbids redistribution.
  - `Restricted` → allowed only with explicit `terms`; delivery must honour them.
  - Empty license id → refused.
- Public chain data uses `onyx:public-chain-data`, redistribution allowed.
- The code is Apache-2.0 ([LICENSE](../../LICENSE)); data in the repository is not covered by it. Each committed data set and its terms: [DATA-LICENSE.md](../../DATA-LICENSE.md). Before the repository goes public: [open-source-checklist.md](open-source-checklist.md).
- Subjects are `Entity`, `Address`, `Instrument` or `Aggregate`. There is no individual subject type, and `EntityKind` (`onyx-labels`) lists only organisations.

## Plan

- Labels cover entities only: exchanges, ETFs, funds, protocols, custodians, issuers, mining pools, treasuries. Never individuals. Bounties or tasks that target a person are rejected by policy.
- Removal requests: an entity or person may ask for a label to be removed. End-to-end process in [label-removal.md](label-removal.md) (intake, triage, removal from every copy, reply, record); removal = new label set version without the label; old blobs deleted from CAS once no root under challenge pins them (R-REMOVE-1). Contact: a GitHub issue, or the private vulnerability reporting form for a request that names a person (THREATS L8-4).
- Offchain licensed data (equities, macro, commodities): ingest only with a written rights basis recorded in the source's `DataRights`. Check each source and each series individually; vendor feeds are often non-redistributable.
- Time decay: data that becomes free after the window must have rights that allow free redistribution. `Restricted` data may never decay to open unless its terms allow it.
- Attribution strings are shipped with every response that needs them.

## Open questions

- Jurisdiction and entity for Onyx (affects terms of service and removal obligations).
- Do first-party attestations need entity consent terms before we publish them?
