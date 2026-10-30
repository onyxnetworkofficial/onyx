# Configuration by environment

## Scope

How Onyx processes get configuration in each environment, and which settings exist.

## Environments

- `dev`: laptop or the build machine 7920. Anvil for chain work (chain 31337).
- `test`: CI and `cargo test`. No network, no secrets, fixtures only.
- `testnet`: Base Sepolia contracts (chain 84532), x402 testnet payments. Never mainnet.

There is no mainnet environment. Every binary refuses a mainnet chain id or payment network
unless started with `--allow-mainnet`, which this project never does.

## Developer tools (`.env`)

`.env` (never committed; `.gitignore` excludes `.env` and `.env.*`) holds the deployer key for
the deploy drivers. `.env.example` is the committed template.

| Variable | Meaning |
|---|---|
| `ONYX_ENV` | `dev`, `test` or `testnet` |
| `BASE_SEPOLIA_PRIVATE_KEY` | Testnet deployer key. Secret. See [secrets.md](secrets.md) |
| `BASE_SEPOLIA_RPC_URL` | Base Sepolia RPC endpoint |
| `DATABASE_URL` | Postgres connection string ([ADR-0007](../adr/0007-storage.md)) |
| `ONYX_CAS_DIR` | Content-addressed store root ([ADR-0007](../adr/0007-storage.md)) |

## The API service (`onyx-node`)

Read once at start and validated; a bad value stops the start with a message that names the
variable (and never echoes a secret). `onyx-node --check-config` runs the same checks without
any network call. Deployment steps: [deployment.md](deployment.md).

| Variable | Default | Meaning |
|---|---|---|
| `ONYX_ENV` | required | `dev` (anvil allowed) or `testnet` (Base Sepolia only) |
| `ONYX_LISTEN` | `127.0.0.1:8080` | Listen address (`0.0.0.0:8080` in the container image). Plain HTTP: an address that is not loopback is refused unless `ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1` (M7-7); put TLS in front ([deployment.md](deployment.md) "TLS in front") |
| `ONYX_ALLOW_PLAIN_HTTP_PUBLIC` | `0` | `1` accepts a non-loopback `ONYX_LISTEN` (with a warning at every start). Only where the port still cannot be reached from outside: a container published on `127.0.0.1`, a testnet lab on a private network. Never makes a public port safe |
| `ONYX_PUBLIC_URL` | required | Base URL clients use; part of every x402 `resource.url`. `https://`, or `http://` to localhost only |
| `ONYX_RPC_URL` | required (or `ONYX_RPC_URL_FILE`) | Chain node. `https://`, or `http://` to localhost only. Logged by host only. **May contain a node provider's API key**: keep env files 0600, or use `ONYX_RPC_URL_FILE` |
| `ONYX_RPC_URL_FILE` | none | File holding the node URL (one line, mode 0600, e.g. a systemd credential). Setting both this and `ONYX_RPC_URL` is refused |
| `ONYX_DEPLOYMENT` | required | Deployment manifest (JSON; [deployment.md](deployment.md)) |
| `ONYX_CATALOG` | required | Catalog file (JSON; `crates/onyx-node/src/catalog_file.rs`) |
| `ONYX_OBSERVATIONS` | none | Level-2 results store files, comma-separated (`onyx_api::observations::ObservationStore`; every round re-checked at start; run-api.md "Level-2 data") |
| `ONYX_OBSERVATION_TEMPLATES` | none (required with `ONYX_OBSERVATIONS`) | `metric=0x<template id>` pairs, comma-separated: the reviewed registration of each level-2 metric; a store with another template is refused at load, and a store naming a registry is re-checked against `OnyxRegistry.metrics` (L2API-R4-F1). That registry must be the deployment manifest's `registry`, whose code must hash to `registryCodeHash` (`eth_getCode`; AUD-2-F8); a pinned metric with no store is an error |
| `ONYX_SALTED_LABELS` | none | Comma-separated salted label set files (`{"set": "0x<borsh DatedLabelSet>", "secret": "0x<32 bytes>"}`) served by `GET /v1/labels/{set}/{address}` (ROADMAP D30). Only `test.*` sets load; any other name, an unsorted set or a bad file fails startup. Keep the files (they hold the salt secret) private to the operator. |
| `ONYX_SUBMISSIONS` | none | U2 intake journal path (ADR-0039). Unset disables all submission routes. Parent directory must exist on private local disk. Broken/locked persistence fails node startup. Review uses the local `submit-review` CLI while the node is stopped; restart to serve new decisions. Never put this journal in a public web directory. |
| `ONYX_SIGNED_REPORTS` | none | The level-3 coordinator's post log (`onyx_coordinator::signed_intake::PostLog`, ROADMAP C10), re-read when it changes. Needs the manifest's `signedBook`, `signedBookCodeHash` and `signedBookReviewer` (checked at start: code hash, `reviewer()`, never the settlement's coordinator); mounts `GET /v1/signed/{fact}`, served only against `settleable` at the finalized block (delivery.md "Signed facts") |
| `ONYX_BULK_DIR` | none | Publishers' large datasets (CL5, ADR-0059; [bulk-publish.md](bulk-publish.md)): manifests, the tiers file and hot shards. Needs the manifest's `signedBook` (manifests are anchored there; start refused without it). Mounts `GET /v1/bulk`, `/v1/bulk/{publisher}/{dataset}` and `.../rows` (free) and the two upload routes `POST /v1/bulk/manifests` and `PUT /v1/bulk/{publisher}/{dataset}/shards/{root}` (outside the 12 s request deadline: 300 s, two at a time). Every held manifest is checked again at start; a damaged one fails the start |
| `ONYX_BULK_WARM_DIR` | `<ONYX_BULK_DIR>/warm` | Warm shards (ADR-0052): a hot shard not read for `ONYX_BULK_WARM_AFTER_DAYS` moves here (copied and read back first) and back to hot when read. Bulk shards never go cold |
| `ONYX_BULK_QUOTA_GIB` | 16 | Bytes per publisher over its datasets' latest versions (413 `bulk_quota_exceeded`) |
| `ONYX_BULK_MAX_DATASETS` | 64 | Datasets per publisher |
| `ONYX_BULK_MAX_GIB` | 256 | Bulk bytes for all publishers together (503 `bulk_node_full`) |
| `ONYX_BULK_WARM_AFTER_DAYS` | 7 | Quiet days before a hot shard moves to warm (checked hourly) |
| `ONYX_TRACK_PUBLICATIONS` | none | The track-record coordinator's publication file (Borsh `onyx_track::publication::Publication`, written by `onyx_coordinator::track_chain::write_publication`, ROADMAP C20; version 1, or version 2 with chained logs and the registry, C21/C23). Read at start and again whenever it changes (a missing file publishes nothing yet); served at `/v1/track`, `/v1/track/log` (paged for chained logs), `/v1/track/registry` and `/v1/track/{source}/{metric}[/history]` only if it checks and starts with what was served before, else every track route answers 503 `track_store_unreadable` (the node itself still starts and serves everything else). The head it served (step count, latest commitment, row counts and a digest of those bytes) is kept in `ONYX_STATE_DIR/track-served-head.borsh`, so after a restart a file that does not start with those bytes, or no file, is refused too; a file whose check would pass 4,194,304 (`MAX_INSTALL_CHECK`: steps times rows for format-1 logs, rows plus steps for chained logs) is refused unchecked. Unset: nothing published (every record a 404 `no_track_record`), unless the state directory holds a served head: then every track route answers 503 (delete the head to start a new chain on purpose). With a file every ruled metric counts as label-derived under `ONYX_RIGHTS=fallback` (the node has no registry) |
| `ONYX_CAS_DIR` | required | Row batches |
| `ONYX_STATE_DIR` | required | Journals (receipts, pending settles, credits in flight). One process per directory, local disk only |
| `ONYX_PAYMENTS` | `off` | `off`, `self-settle` or `facilitator`. Anything else (including `mock`) is refused |
| `ONYX_OPERATOR_KEY_FILE` | none | Reporter key file (64 hex digits, mode 0600). Required for `self-settle`, and for `facilitator` unless `ONYX_REPORTER=off` |
| `ONYX_REPORTER` | `on` | `off` keeps facilitator-mode receipts on disk without posting them to `OnyxRevenue.credit` (contributors are not paid until a reporter posts them). Must be explicit (OPS-L1-F5) |
| `ONYX_X402_FACILITATOR` | x402.org | Facilitator URL in `facilitator` mode. `https://`; `http://` to localhost only with `ONYX_ENV=dev` (a local facilitator decides what is paid, OPS-L1-F3) |
| `ONYX_TRUSTED_PROXIES` | none | Comma-separated CIDRs of reverse proxies whose `X-Forwarded-For` is believed; a `/0` range is refused (it would trust every peer, AUD-2-F8). Behind `deploy/caddy/Caddyfile` on the same host: `127.0.0.1`. Unset, every client behind a proxy shares the proxy's rate limits |
| `ONYX_ALLOW_TEST_LABELS` | `0` | `1` serves shards pinned to a `test.*` label set (all current Bitcoin shards) |
| `ONYX_RIGHTS` | `cleared` with `ONYX_ENV=dev`; required with `testnet` (unset or empty is refused, review L14-R1; an `--export-node` of a testnet run writes `fallback`) | `fallback` serves no label-derived shard (entity flows, balances, reserves, so no OKX reserve sale): 403 `label_rights_withheld` before any 402, nothing charged. A shard is label-derived if it pins a label set, a known serving check covers it (`btc-entity-reserve-v1`), or a metric name segment starts with `entity`. Pool shares and level-2 observations (Treasury, SEC) are still served. Exact lowercase `cleared` or `fallback`, anything else refused. `/v1/health` `rights` shows the mode (ROADMAP L14, THREATS L8-5) |
| `ONYX_REPORTER_EVERY_SECS` | `30` | Reporter interval, 1-3600 |
| `ONYX_PREPAID` | none | An `OnyxPrepaid` address (ROADMAP S7, ADR-0046 amendment 3): paid routes also take `ONYX-PREPAID-VOUCHER`, the usage journal is `<ONYX_STATE_DIR>/prepaid-0x<contract>.journal` (an older `prepaid.journal` is still used; a journal whose vouchers are another contract's is refused at start, THREATS CL6-R1-1; owner-only, back it up with the state dir), and the reporter redeems and allocates every `ONYX_REPORTER_EVERY_SECS` (at most half the contract's exit delay). Needs `ONYX_PAYMENTS=facilitator` with the reporter on; at start the node refuses a contract whose `reporter()` is not the operator key or whose `token()` and `revenue()` are not the manifest's |
| `ONYX_OBSERVE` | none | Observe on the first paid query (ADR-0049, NORTH-STAR NS10): a JSON file `{command, offers}`. `command` is the operator's round command (absolute path, then fixed arguments); for each paid fact the node runs `command... <metric> <subject>`, which runs one level-2 round and prints the path of a results store holding it, and is killed at the hold's deadline. `offers` maps each metric to `{price (decimal string), deadlineSecs, store}`; `store` must be one of `ONYX_OBSERVATIONS` and writable (the served round is appended to it), the metric's template must be dated, and `deadlineSecs` at most half `OnyxPrepaid.exitDelay()`. Needs `ONYX_PREPAID`: a round is paid by a held voucher (`/v1/tickets`, `/v1/health` `observe`) |
| `ONYX_RELAY_KEY_FILE` | none | CL6 (ADR-0056): the relayer's own key file (64 hex digits, mode 0600). Turns on `POST /v1/relay`: self-signed observer posts and EIP-3009 deposits sent by this key, each paid by the actor's voucher adding the action's fee. Needs `ONYX_PREPAID` naming an `OnyxPrepaidV2` whose `relayer()` is this key; refused when it is the operator key, the prepaid or revenue owner, the registry owner or the settlement coordinator. Fund it with test ETH only (docs/ops/runbooks/deploy-credits.md) |
| `ONYX_RELAY_FEES` | `post_commit=1000,post_receipt=1000,post_sealed_reveal=3000,deposit=2000` | CL6: the fixed fee of each relayed action in the asset's smallest unit (1 to 1,000,000 each), published at `/v1/credits/schedule` with its hash |
| `ONYX_CREDITS_FROM_BLOCK` | the manifest's deploy block | CL6: the block `OnyxPrepaidV2` was deployed at; `/v1/credits` indexes its `Credited` events from there (10,000 blocks per `eth_getLogs`) |
| `ONYX_GATE_REFRESH_SECS` | `60` | Seconds between runs of the serving checks, 1-86400 (`onyx-node-btc`; review RESAPI-R1 F3): a listed shard that finalizes after the start is judged without a restart, and a failed root read is retried. A failed read keeps the shard's last answer (or, for a shard never judged, every unchanged answer) instead of refusing the metric |
| `ONYX_INDEXER` | `off` | `on`: the served shards come from `OnyxSettlement`'s `Finalized` events (ADR-0033). The catalog file then lists `held` shards only (no `shards`), and the manifest needs `deployBlock`. Each poll: chain id, the `finalized` block, `eth_getLogs` ranges, a `getFinalizedRoot` per held shard not yet final. Status on `/v1/health` `indexer` |
| `ONYX_INDEX_HEALTH` | `full` | `summary`: `/v1/health` `indexer` shows counts only (no shard ids, no error or conflict text; `failing` says whether the last poll failed). Use it on a node the public can reach (IDX-R1-5). Only with `ONYX_INDEXER=on` |
| `ONYX_INDEX_SPAN` | `200` | Blocks per `eth_getLogs`, 1-10000 (sepolia.base.org allows 200); a refused range is split. Only with `ONYX_INDEXER=on` |
| `ONYX_INDEX_OVERLAP` | `100` | Blocks before the cursor read again each poll, 0-10000 and at least `ONYX_INDEX_MAX_JUMP` (finds logs a lagging backend left out) |
| `ONYX_INDEX_EVERY_SECS` | `15` | Seconds between polls, 1-3600 |
| `ONYX_INDEX_RESCAN_POLLS` | `240` | Polls between held-shard rescans, counted from the end of the previous one, 0-1000000 (0: only after a start). A rescan reads the held shards' `Finalized` logs again from `deployBlock` (their ids as a topic filter, in the same capped ranges) and reads every confirmed end and every served shard's final view again, so a log, an end or an admission a lying node got wrong is put right once the node answers honestly (ADR-0033 Amendments 4 and 5). A start also runs one, from its third poll. Cost: deployment.md "Rescan cost" |
| `ONYX_INDEX_RESCAN_SECS` | unset | The same interval as a time, 0-31536000, rounded up to whole polls (e.g. `86400`: daily). Not with `ONYX_INDEX_RESCAN_POLLS` (IDX-R5-4) |
| `ONYX_INDEX_RESCAN_CALLS` | `100` | `eth_getLogs` calls per poll for the rescan, 1-2000, apart from the scan's own 2,000; the rescan continues on the next poll (IDX-R5-4) |
| `ONYX_INDEX_MAX_JUMP` | `100` | Blocks a poll may read past the `finalized` head this poll and the previous one both reported, 1-1000000 (IDX-R5-5). Must be at most `ONYX_INDEX_OVERLAP` (refused at start otherwise, IDX-R6-3), so the overlap reads again every block one lying head let pass; with the indexer on, an overlap of 0 is therefore refused |
| `ONYX_INDEX_STATE` | `$ONYX_STATE_DIR/indexer.json` | Cursor, events, a passed deploy-block check (SOAK-4) and a rescan in progress (a restart resumes it, IDX-R6-2), bound to the chain, contract and deploy block (another deployment's file is refused: delete it or point elsewhere) |
| `ONYX_HEADER_READ_TIMEOUT_SECS` | `10` | A connection must send its request headers within this time, 1-120 (slowloris, AUD-1-F5) |
| `ONYX_MAX_CONNECTIONS` | `1024` | Open connections, 1-65535. When all are taken, a new connection evicts the oldest one that has not sent a request; with none such, it is closed (AUD-1-F5, AUD-1R-F1) |
| `ONYX_MAX_CONNS_PER_IP` | `32` | Open connections per client (an IPv4 address or an IPv6 /64), 1-65535 and at most `ONYX_MAX_CONNECTIONS` (AUD-2-F8); a new one beyond is closed. An IPv6 /56 may hold 4 times this and a /48 16 times (`PREFIX_RATE_MULTIPLIER`, the rate limiter's scopes; AUD-2-F5). `ONYX_TRUSTED_PROXIES` are exempt (all their clients share the proxy's address); put the same kind of cap on the proxy (AUD-1R-F1) |
| `ONYX_MAX_FEE_GWEI` | `10` | Fee cap of every transaction the node signs (self-settle, reporter credits, registrations, fee-bump replacements), 1-10000 gwei. Base Sepolia fees run far below; raise it only during a fee spike (AUD-1R-F3) |
| `ONYX_MAX_GAS` | `15000000` | A send whose gas estimate is above this is refused, 21000-30000000 (AUD-1R-F3) |
| `ONYX_USDC_EIP712_NAME`, `ONYX_USDC_EIP712_VERSION` | `USDC`, `2` | EIP-712 domain of the payment token |
| `ONYX_RESERVE_GATE` | unset | `onyx-node-btc` only: a JSON file (`fixtures`, `scan`, `labels`, `crossChecks`, `startHeight`, `recipeId`, optional `secondScans`) naming the inputs of `btc.entity-reserve`'s serving check (ADR-0034 Amendment 1). Scan, labels and cross-checks are checked against the fixtures' `SHA256SUMS`. Unset, or plain `onyx-node`: every catalog entry with `servingCheck`, and every shard whose schema or recipe id is a reserve one (`onyx_api::gate::KNOWN_GATES`, whatever its metric name), is refused (403 `serving_check_unavailable`) |

The operator key is never an environment variable: only a path to a file the server checks is
private. Under systemd it is a credential (`LoadCredential=`); in a container, a read-only mount.
Env files are secrets too when they hold a node URL with an API key: install them 0600
(`--export-node` writes them 0600), or move the URL to `ONYX_RPC_URL_FILE`.

## Open questions

- Config file (TOML) in addition to env vars, or env vars only? (Env vars plus JSON files for
  the manifest and catalog, for now.)
