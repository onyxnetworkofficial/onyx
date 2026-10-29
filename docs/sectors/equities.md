# Sector: equities (stub)

## Scope

Facts about listed companies and funds: corporate filings (shares outstanding,
holdings disclosures), fund holdings, possibly end-of-day prices.

## Claim model fit

- Filings: `VerificationMethod::OfficialSourceMatch` against the regulator's
  publication, pinned as `Snapshot::Document { published_unix, content_hash }`.
  Anyone can fetch the same document and check the value.
- Prices: `CrossSourceQuorum` across independent sources with rights to redistribute,
  with a stated `tolerance` in smallest units (cents, scaled via `Value::Fixed`).
- Subjects: `Subject::Instrument("ticker:...")` or `Subject::Entity(...)` for issuers.
- Recipes may still parse a pinned document deterministically (for example extract a number from a filing), so `Recompute` can apply to document parsing.

## Rights issues

- Exchange market data is commonly licensed with redistribution limits. Do not ingest any price feed until its license is read and recorded. If it forbids redistribution, Onyx does not ingest it (`DataRights::check_ingest`).
- Regulator filings: check the terms of each regulator's site before ingesting.
- Time decay to open must be allowed by the license.

## Open questions

- Which first equities metric, and from which source with clear redistribution rights?
- Is a quorum of non-official price sources acceptable for settlement use?
