# Sector: commodities (stub)

## Scope

Commodity prices, inventories and reserves (for example exchange warehouse stocks, government reserve reports).

## Claim model fit

- Official reports: `OfficialSourceMatch` with `Snapshot::Document`.
- Prices: `CrossSourceQuorum` with a tolerance, only across sources whose rights allow redistribution.
- Tokenised commodities (gold tokens) can join the crypto lane: onchain supply via `Recompute`, custodian reserves via `FirstPartyAttestation` plus documents.

## Rights issues

Benchmark prices are often licensed. Check each source before ingest; refuse `Forbidden`.

## Open questions

- Is there a commodity data source with clear redistribution rights to start with?
