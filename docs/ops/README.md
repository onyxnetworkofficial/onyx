# Operations

Operations docs grow with the code. In M0 every file is a stub with scope,
current state, plan and open questions. Testnets only until the mainnet gate in
[upgrades.md](upgrades.md) is passed.

| Doc | Covers |
|---|---|
| [config.md](config.md) | Config by environment, env vars |
| [secrets.md](secrets.md) | Secret handling rules |
| [key-management.md](key-management.md) | Keys, roles, rotation |
| [hardware.md](hardware.md) | Hardware per role and chain (measured or linked), the `onyx-node doctor` preflight |
| [deployment.md](deployment.md) | Local compose, Base Sepolia deploys, address table |
| [remote-check.md](remote-check.md) | check-all on the remote build host (the build machine): limits, refusals, layout |
| [run-api.md](run-api.md) | Run the query API locally (free and x402 mock) |
| [demo-local.md](demo-local.md) | M8 rehearsal: one round on a local chain, served and checked |
| [local-handoff.md](local-handoff.md) | U5: one proposal from contributor to buyer on a local node; the preview bundle |
| [site-hosting.md](site-hosting.md) | The public website at onyx.cryptagraph.com: server, TLS, deploy and rollback |
| [demo-testnet.md](demo-testnet.md) | M8 on Base Sepolia: plan, fork rehearsal, the real run (self-settled x402) |
| [monitoring.md](monitoring.md) | Logs, metrics, alerts |
| [runbooks/health-watch.md](runbooks/health-watch.md) | Health watch: install, alerts and what each means, the notify hook |
| [incident-response.md](incident-response.md) | Severity levels and playbooks |
| [upgrades.md](upgrades.md) | Recipe, label set and contract versioning; mainnet gate |
| [launch-checklist.md](launch-checklist.md) | Public testnet launch: go/no-go gates, keys and owners, monitoring, incident response, rollback, owner decisions |
| [open-source-checklist.md](open-source-checklist.md) | What must be true before the repository is made public (secrets, history, label rights, contacts, licence files) |
| [data-retention.md](data-retention.md) | What we keep and for how long |
| [cost-model.md](cost-model.md) | Cost per query and per validation |
| [governance.md](governance.md) | Who adds recipes and label sources |
| [legal-data-rights.md](legal-data-rights.md) | Licensing, redistribution, entities only |
| [data-sources.md](data-sources.md) | Every current and candidate data source: terms, decision, proof level, ranking |
| [label-removal.md](label-removal.md) | Label correction and removal requests, end to end |
| [onboarding-sector.md](onboarding-sector.md) | Adding a chain or sector |
| [runbooks/](runbooks/README.md) | Step-by-step runbooks |
