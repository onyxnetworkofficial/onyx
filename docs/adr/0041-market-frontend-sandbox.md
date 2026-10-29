# ADR-0041: Market frontend is an explicit sandbox

## Status

Accepted for U6A frontend scope, 2026-10-22.

## Context

The October 20 product call discussed (a) incentives or staking to check submitted data and (b)
markets that forecast a future data observation. It did not settle a prediction-market launch.
It raised manipulation and duplicate-reward risks for validity staking, and described the broader
futures system as future work. The current Onyx network is built around submitted data and independent
verification; it has no market registry, order book, collateral, prediction oracle, positions API,
or settlement contract.

## Decision

U6A may ship a complete navigable **local concept UI** for explore, market detail, user-entered
arithmetic preview, positions, evidence/resolution/redemption states, and unsigned term drafting.
The UI must:

- distinguish future-outcome forecasting from staking on the correctness of existing data;
- call out that the call deferred a full futures system and leave validity staking unresolved;
- show no invented quotes, liquidity, activity, balances, positions, or evidence;
- make every order calculation local, hypothetical, user-priced, and non-persistent;
- keep redemption disabled and explicitly state that market/oracle/settlement services are absent;
- label the source example as an incomplete sketch until model ID, exact report/field/location,
  observation window, and revision policy are actually specified; never claim it represents global
  usage.

U6A does not authorize or implement trading, token movement, market creation transactions, a market
oracle, or production/testnet market deployment. Any backed market is a separate U6B design and
implementation milestone requiring its own threat review, frozen terms, collateral and payout rules,
resolution/finality and void behavior, and contract/backend tests.

## Consequences

Users can inspect a complete workflow and discuss product fit without mistaking a mock screen for a
live market. The prototype intentionally cannot validate market demand or actual trading UX. U6B
remains open; the data-contribution and verification product remains the core direction.
