# Data sources of the keyed demo (a FIXTURE, not the project's terms table)

This file stands in for docs/ops/data-sources.md in the local keyed demo (contributor loop CL4,
ADR-0054): `fixtures/keyed-demo` is passed as `--repo`, so the manifest loader reads its terms
here. The provider is synthetic (a loopback fake on 127.0.0.1); nothing here clears a real host.

## 1. Current sources

| ID | Source | Where | Terms | Decision | Used for | Level | State |
|---|---|---|---|---|---|---|---|
| S90 | Synthetic keyed provider (local fixture, `keyed.example.org`) | fixtures/keyed-demo | Synthetic data, CC0; each holder's key only meters access | OK | The CL4 keyed demo | 2 | Fixture |
| S91 | Synthetic provider whose terms were not cleared for keyed use | fixtures/keyed-demo | Synthetic | OK | The terms-bypass test | 4 | Fixture |

## 6. Hosts cleared for source manifests

| Host | Row | Decision | Manifests |
|---|---|---|---|
| keyed.example.org | S90 | OK keyed | `sources/demo.keyed.widget_index` |
| plain.example.org | S91 | OK | none (public manifests only: a keyed manifest on this host does not load) |
