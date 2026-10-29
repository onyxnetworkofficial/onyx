# Site hosting (pre-public loop PP13)

The public website is the public-safe build (`ONYX_PUBLIC=1 ONYX_RIGHTS=fallback`: no label rows,
no label-derived totals) served as static files at **https://onyx.cryptagraph.com**.

| What | Where |
|---|---|
| Server | A Hetzner Cloud CX23 (2 vCPU, 4 GB) used for Onyx only, Ubuntu; ssh alias `onyx-web`, user `onyx` (key only; root and password logins off) |
| Firewall | The Hetzner firewall and `ufw`: TCP 22, 80 and 443 in, nothing else |
| TLS | Caddy, certificates from Let's Encrypt renewed by Caddy itself; no ACME e-mail; HTTP redirects to HTTPS |
| Files | `/srv/onyx/releases/<UTC time>-<commit>`; `/srv/onyx/current` points at the live one; the last 5 are kept |
| Logs | No access log (nothing about visitors is kept) |
| Updates | `unattended-upgrades` with an automatic reboot at 04:30 UTC when one is needed; `fail2ban` on SSH |
| DNS | An A record for `onyx` at the domain's registrar |

## Deploy

```sh
scripts/deploy-site.sh                 # build the public fallback site, then deploy
scripts/deploy-site.sh --skip-build    # deploy an existing public fallback build
```

The script refuses a build that is not a public fallback build or that contains the owner's or a
partner's name, copies it to a new release folder, switches `current` in one step and checks the
live page over HTTPS. Roll back: `ssh onyx-web 'ln -sfn /srv/onyx/releases/<older> /srv/onyx/current'`.

## API (NS17)

The query API (`onyx-node`, testnet) runs on the same server and answers under
**https://onyx.cryptagraph.com/v1/** (`/v1/health`, `/v1/openapi.json`, `/v1/settlement/...`, ...).

Live since 2026-10-27 (installed with the owner's OK): `/v1/health` answers 200 on Base Sepolia
(chain 84532, settlement `0xe4be…ae8f`, rights `fallback`, `acceptingPayments: false`) with 2
servable shards; `/` 200; `/v1/nope` and `POST /v1/health` 404; Caddy's admin endpoint
(127.0.0.1:2019) closed; `onyx-node` and `caddy` active; no `/etc/onyx/operator.key`; the site's
live daily check passed 11 of 11.

| What | Where |
|---|---|
| Process | `onyx-node`, the hardened unit `deploy/systemd/onyx-node.service` (dynamic user, private state, `MemoryMax=2G`, no link-local addresses), listening on `127.0.0.1:8080` only |
| Proxy | Caddy (2.6, the Ubuntu package: no `request_buffers`) routes the API's GET/HEAD paths to it (`deploy/caddy/onyx-web.Caddyfile`, installed as `/etc/caddy/Caddyfile`), and POST `/v1/submit` and `/v1/submit/check` (proposals, bodies cut at 64 KiB); every other `/v1/` path is 404. `/mcp` (POST, DELETE) goes to the agent connector on 127.0.0.1:8090 (bodies cut at 256 KiB). Caddy's admin endpoint is off (the script checks `127.0.0.1:2019` refuses before it starts the node): a config change needs `systemctl restart caddy`, not `reload`. HTTP/1.1 only, for the site too |
| Chain | Base Sepolia through the public `https://sepolia.base.org` (no provider key on the server); the contracts of the NS17 run ([deployment.md](deployment.md) "Deployed addresses") |
| Data | The run's held shards, admitted by the node's own chain indexer once final; `ONYX_RIGHTS=fallback`, so label-derived metrics (entity flows, reserves) answer 403 `label_rights_withheld` before any 402 until the label rights are cleared in writing (launch-checklist LC-1) |
| Payments | `off` (no key on the server): every shard served today is label-derived and withheld. `ONYX_API_PAYMENTS=1 scripts/deploy-api.sh ...` keeps the export's `self-settle` (x402 on Base Sepolia test USDC into `OnyxRevenue`) and installs the testnet operator key as a systemd credential; do that once something sellable is served (cleared label rights, or level 2 after ROADMAP B6) |
| Health | `/v1/health` in summary mode (`ONYX_INDEX_HEALTH=summary`: counts, no shard ids or error text) |
| Proposals | On (ROADMAP S5, 2026-10-28): `ONYX_SUBMISSIONS=/var/lib/onyx/state/submissions.journal` with the node's per-client, daily and global limits and a cap on what the journal holds (503 `intake_full` after it); nothing a proposal says is served before review. Review: stop the node, run `submit-review` on the journal ([submission-review.md](submission-review.md)), start it. `ONYX_API_INTAKE=0 scripts/deploy-api.sh ...` turns intake off |
| Agent connector | `apps/agent/src/http.mjs` (MCP over HTTP, NL21) in `/opt/onyx-mcp`, run by `deploy/systemd/onyx-mcp.service` when the host has Node 20 or later (else the script says it skipped it and `/mcp` answers 502). Read-only (`onyx_propose_dataset` off), no key, no `onyx-check` (answers say `not_checked`). `ONYX_API_MCP=0` leaves it out |

Deploy (after a testnet run with `--export-node`, docs/ops/demo-testnet.md):

```sh
CARGO_TARGET_DIR=~/.cache/onyx-api-target RUSTFLAGS="--remap-path-prefix=$HOME=/build" \
  cargo build --release --locked -p onyx-node
scripts/deploy-api.sh <export dir> ~/.cache/onyx-api-target/release/onyx-node
```

The script refuses a binary that holds the builder's home folder path and an export that is not
a testnet `fallback` export. It installs the Caddy file first when it changed (validated; the old
one kept as `/etc/caddy/Caddyfile.bak-<UTC time>` and put back if Caddy does not restart), then
the node, and waits until `/v1/health` shows a servable shard and the site and the API answer over
HTTPS. Stop the API: `ssh onyx-web sudo systemctl disable --now onyx-node` (the site keeps working;
the API's paths then answer 502, other `/v1/` paths 404).

Per-address connection cap (THREATS NS17-4, NL5-3; ROADMAP L19): Caddy is a trusted proxy, so the
node's own per-address cap does not apply to clients behind it. `deploy/nftables/onyx-conn-cap.nft`
resets a new connection on 443 from a source (IPv4 address or IPv6 /64) that already holds 32; it is
its own nftables table next to ufw, loaded by `deploy/systemd/onyx-conn-cap.service` (install steps
in the unit's header) and tested with `scripts/conn-cap-test.sh`. `scripts/deploy-api.sh` installs
and loads it (after `nft -c -f`) unless `ONYX_API_CONN_CAP=0`; on the server from the next deploy. The health watch alerts on the operator's gas once payments are on
(`balances.operator_eth`).

Track records (north-star loop NL4, ADR-0036 amendment 8): `scripts/track-nws-sepolia.sh <export>`
states the NWS forecast of the Denver high through the track-record intake on Base Sepolia and
writes `<export>/track/publication.borsh`; `scripts/deploy-api.sh` installs it as
`/etc/onyx/track/publication.borsh` (`ONYX_TRACK_PUBLICATIONS`). Each later run appends a
commitment (the node refuses a file that does not start with what it served). Not deployed yet.

Level-2 values (ROADMAP B6): the daily earthquake and temperature rounds ran live on Base Sepolia
on 2026-10-27 against this deployment's registry (`scripts/level2-sepolia.sh`, docs/ops/demo-testnet.md
"Level 2 on Base Sepolia"). Their results stores sit in the export's `observations/` folder with
their template pins; `scripts/deploy-api.sh` installs them under `/etc/onyx/observations/` and sets
`ONYX_OBSERVATIONS` and `ONYX_OBSERVATION_TEMPLATES`, and the node checks each store against its pin
and the registry on chain before it serves it. Checked on a local node with the same settings
(2026-10-27): both answer 200 and a client's own check passes off chain and on chain
(`cargo run -p onyx-source-usgs --example level2-check -- --url ... --subject ... --rpc ...
--registry ...`); another day answers 404. Live on the server since 2026-10-27 (owner's OK;
`scripts/deploy-api.sh` then `scripts/deploy-site.sh`, release `20261027T153436Z-fba600b`): both
answer 200 at https://onyx.cryptagraph.com/v1/observations/..., the same client check passes
against the hosted API and the registry on chain, another day answers 404. Treasury and SEC rounds still ran only on local test chains.

Release `2f01280`, live 2026-10-28 (owner's "Deploy"): `scripts/deploy-api.sh` exit 0, `/v1/health`
200; all six level-2 series answer 200 at level 2 (New York precipitation 40.1 mm and high
17.2 °C, Denver precipitation 0.0 mm and high 28.3 °C, world M5+ 5, Los Angeles M2.5+ 1); intake
on (`/v1/submit/check` answers a JSON 400 to an empty body); the 443 connection cap loaded (`nft`
table `inet onyx_conn_cap`); Caddy admin closed; payments off. The MCP connector at `/mcp` is not
running: the host has no Node, and installing it waits for the owner's OK. Site:
`scripts/deploy-site.sh --skip-build` from a clean worktree at `2f01280` after a public build whose
e2e passed, release `20261028T035536Z-2f01280`; the live daily check passed (with the NS5-8 and B6
checks), and the live Markets page at 1280 and 390 px reads market 1 and both insurance products
from Base Sepolia with no sideways scroll and no page errors.
