# ADR-0007: Postgres for ledgers, content-addressed store for blobs

## Status
Accepted, 2026-10-06 (M0). Amended 2026-10-25: Postgres was never used and is removed (amendment 1).

## Context
We store rounds, commitments, rewards and slashes (relational, transactional), plus snapshots,
recipe WASM and row batches (large, immutable).

## Decision
- Postgres 16 via Docker Compose for local dev (`docker-compose.yml`).
- Content-addressed store (CAS): files at `$ONYX_CAS_DIR/<first 2 hex>/<keccak hex>`. Write once,
  verify hash on read. An S3-compatible backend can implement the same trait later.
- Unit tests use an in-memory CAS and need no database; integration tests that need Postgres are marked and run in CI with a service container (from M3).

## Consequences
- Blobs are never mutated or deleted by normal operation; retention rules in docs/ops/data-retention.md.
- The CAS proves integrity, not availability. Availability (others can fetch the data) is an open question in PLAN.md.

## Amendment 1 (2026-10-25, pre-public loop PP7): no Postgres
Postgres was never used. Rounds, ledgers, receipts, journals and stores became append-only
journals and content-addressed files (`onyx_api::journal`, the CAS above, ADR-0039), each with its
own lock, replay checks and caps, so no code ever connected to a database. `docker-compose.yml`
(a Postgres 16 service for local development) is removed. A database needs a new decision
(PRODUCT-PLAN.md: "Do not add a database just to duplicate the journal").
