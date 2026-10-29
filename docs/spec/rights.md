# Spec: data rights

## Scope
The rights carried by every claim and source, the ingest gate, serve-time duties and the
entities-only rule. Types: `crates/onyx-core/src/rights.rs`. Legal notes: docs/ops/legal-data-rights.md.

## Interfaces
- `DataRights { license, redistribution: Allowed | Restricted | Forbidden, source, attribution, terms }`.
- `DataRights::check_ingest()`: refuses `Forbidden`; license ids that are not visible ASCII;
  `Restricted` without a terms id; `Allowed` with terms (they would be silently ignored).
- `Claim::validate` also requires `rights.source == source.source`.
- `Commitment.rights_hash` binds the rights of every row in a batch (ADR-0011).
- `DataRights::public_chain(source)`: rights for public blockchain data.

## Rules
1. Never ingest data whose license forbids redistribution (hard limit). Enforced in `Claim::validate`.
2. Labels cover entities only. The `Subject` and `EntityKind` types have no person variant.
3. Attribution and terms travel with the data to the customer.
4. An entity can ask to correct a label; the request is logged and resolved by evidence.

## Registry (loop 3, ADR-0012)
Rights come from the reviewed source registry, not from the submitter: a claim's rights must equal
the registered rights of its source, and unregistered sources are rejected. Forbidden sources cannot
be registered at all. Attribution is display-only visible ASCII and never carries terms (R-RIGHTS-3).

## Reviewed offchain sources
| Source id | License id | Evidence | Status |
|---|---|---|---|
| `us.treasury.fiscaldata` | `onyx:us-treasury-fiscal-data-open` | fiscaldata.treasury.gov/about-us "Licensing": data "available to copy, adapt, redistribute, or otherwise use for non-commercial and commercial purposes" (read 2026-10-11); no account | `Allowed`; adapter `adapters/source-fiscaldata`, provenance in its `fixtures/PROVENANCE.md` |
| `sec.edgar` | `onyx:sec-gov-public-information` | www.sec.gov/privacy: information on sec.gov "is considered public information and may be copied or further distributed" without permission (read 2026-10-11); access policy: a declared User-Agent, at most 10 requests a second; no account | `Allowed`; adapter `adapters/source-edgar`, provenance in its `fixtures/PROVENANCE.md`; attribution line shown with the data |
| mining-pools list (a recipe input, not a claim source) | `MIT` | github.com/bitcoin-data/mining-pools, branch `generated`, `pool-list.json`; the full MIT notice ships in `fixtures/pools/LICENSE-mining-pools.txt`, as MIT requires | `Allowed` with the notice; pool names and tags only (no person's coinbase handle: POOL-R1/R2 scan), provenance in `fixtures/pools/PROVENANCE.md` |

## Assumptions
- Public chain data is freely redistributable (`onyx:public-chain-data`).
- Each offchain source gets a rights review before its adapter is merged.

## Open questions
- License ids: SPDX where possible; we need a registry of custom ids.
- How to handle a source that changes its license later (already-finalized rows).
