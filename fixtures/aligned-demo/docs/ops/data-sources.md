# Data sources of the aligned demo (a FIXTURE, not the project's terms table)

This file stands in for docs/ops/data-sources.md in the local aligned demo (contributor loop CL8,
ADR-0058): `fixtures/aligned-demo` is passed as `--repo`, so the manifest loader reads its terms
here. Every venue is synthetic (a loopback fake on 127.0.0.1); nothing here clears a real host or
a real venue. The real venues' terms review is docs/ops/market-venues.md (none cleared).

## 1. Current sources

| ID | Source | Where | Terms | Decision | Used for | Level | State |
|---|---|---|---|---|---|---|---|
| S80 | Synthetic venue A (local fixture, `venue-a.example.org`) | fixtures/aligned-demo | Synthetic data, CC0 | OK | The CL8 aligned demo | 2 | Fixture |
| S81 | Synthetic venue B (local fixture, `venue-b.example.org`) | fixtures/aligned-demo | Synthetic data, CC0 | OK | The CL8 aligned demo | 2 | Fixture |
| S82 | Synthetic venue C (local fixture, `venue-c.example.org`) | fixtures/aligned-demo | Synthetic data, CC0 | OK | The CL8 aligned demo | 2 | Fixture |
| S83 | Synthetic venue D (local fixture, `venue-d.example.org`) | fixtures/aligned-demo | Synthetic data, CC0 | OK | The CL8 aligned demo | 2 | Fixture |
| S84 | Synthetic venue E (local fixture, `venue-e.example.org`) | fixtures/aligned-demo | Synthetic data, CC0 | OK | The CL8 aligned demo | 2 | Fixture |
| S85 | Synthetic venue F (local fixture, `venue-f.example.org`) | fixtures/aligned-demo | Synthetic data, CC0 | OK | The CL8 aligned demo | 2 | Fixture |
| S89 | Synthetic venue whose terms forbid passing data on (rights-bypass test) | fixtures/aligned-demo | Synthetic: redistribution forbidden | blocked | Nothing | none | Fixture |

## 6. Hosts cleared for source manifests

| Host | Row | Decision | Manifests |
|---|---|---|---|
| venue-a.example.org | S80 | OK | `sources/demo.venue_a.aaa_usd.1h` |
| venue-b.example.org | S81 | OK | `sources/demo.venue_b.aaa_usd.1h` |
| venue-c.example.org | S82 | OK | `sources/demo.venue_c.aaa_usd.1h` |
| venue-d.example.org | S83 | OK | `sources/demo.venue_d.aaa_usd.1h` |
| venue-e.example.org | S84 | OK | `sources/demo.venue_e.aaa_usd.1h` |
| venue-f.example.org | S85 | OK | `sources/demo.venue_f.aaa_usd.1h` |
| venue-g.example.org | S89 | blocked | none (THREATS CL8-12: a manifest on this host does not load) |
