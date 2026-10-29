# ADR-0008: tracing and Prometheus metrics

## Status
Accepted, 2026-10-06 (M0). First wired in M3 (long-running services).

## Decision
- `tracing` with `tracing-subscriber` JSON output in services; human output in dev.
- Every log line about a round carries `round`, `shard`, `recipe_id`, `label_set` fields.
- Metrics via a Prometheus `/metrics` endpoint on each service (crate choice made when first used).
- Never log secrets, keys or full `.env` contents.

## Context
Disputes and slashes must be explainable from logs. Structured fields make evidence searchable.

## Consequences
- Alert list lives in docs/ops/monitoring.md.
