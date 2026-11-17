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

## Amendment 1 (2026-11-16): no access log; usage counts instead

### Context
The hosted node and the proxy in front of it keep no access log (docs/ops/site-hosting.md: nothing
about visitors is kept). That left one question unanswered that an operator does need: which routes
and which datasets are used at all (READINESS A9). An access log would answer it and a hundred
things nobody should know.

### Decision
- No access log, at the proxy or in the node. This is now a rule, not only the state of one server.
- `onyx-node` counts requests itself (`ONYX_USAGE=on`, off by default): per UTC day, per route
  template and status class, and per dataset it holds for the bulk dataset routes. Counts only.
- No address, user agent, referrer, cookie, header, query text, path value, id, time within the day
  or duration is read for the counts or stored. Every key is a string the server chose (a route
  template, a held dataset's name), never one a client sent.
- Stored as one small JSON file per day in the state directory, 90 days, bounded: at most 2,000
  dataset names a day, the rest in "other".
- A dataset number counts opens (the dataset's info, or the first page of its rows), not every
  request. A dataset under 5 opens for a day is never shown by name, and is folded for good when the
  day is over. A count is a number of requests, never of people.
- No route gives the counts out. The operator reads the day files on the host with
  `onyx-node usage` (review USE-R1: a first build's loopback-only route rested on what the node
  cannot check, and was removed). Backups keep finished, folded days only.
- The exact schema, the bounds and the reasons are docs/spec/usage-counts.md.

### Consequences
- The operator learns which routes and datasets are used and how often they fail, per day. Not who,
  not from where, not when in the day, not how many different people.
- Abuse cannot be traced to a client from these counts. The rate limits work on addresses in memory
  and keep none; that stays as it is.
- The Prometheus `/metrics` endpoint of the decision above is still not built for `onyx-node`; if it
  is, it follows the same rule: no label may carry anything a client sent.
- Threats and tests: THREATS.md `USE-1` to `USE-7` and review `USE-R1`.
