# Monitoring and alerts

## Scope

Logs, metrics and alerts for coordinator, miners, validators and the API ([ADR-0008](../adr/0008-observability.md)).

## Current state (M0)

- Decided: `tracing` with structured JSON logs; a Prometheus-format metrics endpoint per service.
- Today (2026-10-15): `/v1/health` (JSON: receipts, settle intents, reporter, indexer, serving checks) and `onyx-node healthcheck`; no metrics endpoint.
- Health watch (ROADMAP L12): `scripts/health-watch.py` polls `/v1/health`, the chain RPC, the deployer and OnyxRevenue balances and the public site every minute (systemd user timer), prints one JSON line per check, de-duplicates alerts and hands them to a file or a `--notify-cmd` hook. Thresholds: [deploy/health-watch/thresholds.json](../../deploy/health-watch/thresholds.json). Runbook: [runbooks/health-watch.md](runbooks/health-watch.md). What it reads: [launch-checklist.md](launch-checklist.md#3-monitoring) section 3. No alert channel is wired yet (LC-18).

## Plan

Logs: one JSON object per event, with `round`, `shard`, `recipe_id`, `label_set`, `actor` fields where they apply. Never log secrets ([secrets.md](secrets.md)) or full row payloads of restricted data.

Metrics (first set):

- `onyx_round_phase{round}` and seconds in current phase
- `onyx_reveals_total{result="match|mismatch"}`
- `onyx_canary_results_total{validator,result}`
- `onyx_recompute_mismatch_total{recipe}`
- `onyx_disputes_open`
- `onyx_api_requests_total{route,paid}` and x402 settlement failures

Alerts:

| Alert | Fires when |
|---|---|
| Round stuck | A round stays in one phase longer than its configured deadline |
| Canary failure rate | A validator fails canaries above a threshold (threshold set in M4) |
| Dispute opened | Any challenge is posted |
| Determinism mismatch | Two honest reruns of the same recipe + input give different bytes; page immediately, see [incident-response.md](incident-response.md) |
| Payment failures | x402 settlement errors above a threshold |

## Open questions

- Prometheus + Grafana self-hosted on the build machine, or a hosted service (paid, needs approval)?
- Alert routing: email, Telegram, or phone push?
