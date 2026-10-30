# Deployment

## Scope

How the Onyx query API runs as a long-lived service (`onyx-node`), and how contracts reach Base
Sepolia. Mainnet is out of scope until the gate in [upgrades.md](upgrades.md): the service
refuses a mainnet chain or payment network unless started with `--allow-mainnet`, which this
project never does.

## What runs

`onyx-node` (crate `crates/onyx-node`) is the onyx-api server as a service. It reads:

| Input | Where | What |
|---|---|---|
| Settings | `ONYX_*` environment variables ([config.md](config.md)) | Treat as secret: `ONYX_RPC_URL` often carries a node provider's API key. Keep env files 0600, or pass the URL as a file (`ONYX_RPC_URL_FILE`). |
| Deployment manifest | `ONYX_DEPLOYMENT` (JSON) | Chain id, settlement address and runtime code hash; with payments, OnyxRevenue, its code hash and the USDC address. |
| Catalog | `ONYX_CATALOG` (JSON) | Shards (Borsh `ShardSpec` + `DataRights`), markets, prices. The server derives shard ids, `asOf` and bindings from the specs. A block shard carries its end block's header (`endHeader`, hex): the node checks it hashes to the snapshot's tip and takes the data time from it (AUD-2R-F2). With a pinned registry each shard is floored at its on-chain level (AUD-2R-F3): the node checks the registry's code hash and that its `ledger()` is the manifest's settlement before it reads a level, and does not start otherwise (L16). |
| Rows | `ONYX_CAS_DIR` | Row batches, `<dir>/rows/<merkle root>` (the miners' published batches). |
| State | `ONYX_STATE_DIR` | Receipt journals, pending self-settles, credits in flight (REV-11, REV-L4-F1). Back it up. |
| Operator key | file at `ONYX_OPERATOR_KEY_FILE` (mode 0600) | OnyxRevenue's reporter key: gas only, owns nothing (M8-L58-F5). Self-settle needs it; the facilitator mode uses it to post receipts. |

At start it checks the manifest's code hashes on its own node (settlement through
`ChainRoots::connect`; OnyxRevenue through the same `PayToPin` check a client runs), and stops
if either differs. Payments are `off`, `self-settle` (the server submits each payment into
`OnyxRevenue.settle`, ADR-0026) or `facilitator` (an HTTPS x402 facilitator, default
x402.org on Base Sepolia). No mock or dev facilitator exists in this binary.

Commands:

```sh
onyx-node                  # serve
onyx-node --check-config   # read settings, manifest, catalog, key file; no network; exit 0 if fine
onyx-node healthcheck      # GET /v1/health on ONYX_LISTEN; exit 0 on 200 (container health check)
onyx-node doctor --role node   # hardware preflight on ONYX_STATE_DIR: PASS/WARN/FAIL per check (hardware.md)
```

Serving and `--check-config` both print the `node` row of the hardware preflight first
([hardware.md](hardware.md) section 5): one `PASS` line, or each line that is not `PASS`. Only a
state directory with less free space than the soak's largest one (542,874 bytes) stops the start;
CPU, RAM and disk type only warn.

`onyx-node-btc` (crate `adapters/reserve-serve`) is the same server with the Bitcoin serving
check, which `btc.entity-reserve` needs (ADR-0034 Amendment 1; `ONYX_RESERVE_GATE`, run-api.md
"Reserve balances"). Plain `onyx-node` (and the default Docker image, which builds it) refuses
every shard whose catalog entry names a serving check, and every shard whose schema or recipe id
is a reserve one (`onyx_api::gate::KNOWN_GATES`), whatever its catalog entry says.

Packaging `onyx-node-btc` (review RESAPI-R1 F4):
- Docker: `docker build --target onyx-node-btc -t onyx-node-btc:local .` builds the binary and
  ships the committed scan, label set, cross-checks and header chain (ADR-0037) under
  `/usr/share/onyx/fixtures` (with only
  their `SHA256SUMS` lines) and `deploy/reserve-gate.json` as `ONYX_RESERVE_GATE`; run it like the
  default image. Mount another config and set `ONYX_RESERVE_GATE` to change the start height or
  add second scans.
- systemd: install the binary as `/usr/local/bin/onyx-node-btc` and the drop-in
  `deploy/systemd/onyx-node.service.d/btc.conf` (it swaps `ExecStart`/`ExecStartPre` and sets
  `ONYX_RESERVE_GATE=/opt/onyx/deploy/reserve-gate.json`, a checkout at `/opt/onyx`); the unit's
  hardening stays in one file.
- The shipped label set is `test.onyx.entities.btc.v2`: serving it needs
  `ONYX_ALLOW_TEST_LABELS=1`.

On SIGTERM (or Ctrl-C) it stops accepting connections, lets requests in flight finish (up to
the 12 s request timeout), then waits for the settles those requests started (up to 50 s:
with a facilitator, two 16 s settles, two 8 s authorization checks and a 2 s wait,
`settle_task_budget`; with self-settle, its own 50 s budget; the facilitator's HTTP client waits
16 s per settle, not the 4 s verify timeout, OPS-L2-F1), stops the reporter and the intent resolver, and exits. A settle
never queues for a facilitator slot: the slot taken at verify is held through the settle, so
these bounds hold (OPS-L2-F2). The whole stop takes at most about 66 s; the server prints its
bound at start. **Give the supervisor at least 75 s**: `TimeoutStopSec=75s` in the unit,
`--stop-timeout 75` for Docker (whose default of 10 s is too short).

A clean stop is a convenience, not what keeps fees safe (OPS-L2-F1, OPS-L3): every paid settle is
first written to `settle-intents.jsonl` in the state dir. A settle that succeeds removes its intent
only once its receipt is durable on disk (or, self-settled, once it credited the shard on chain).
Any other ending (a facilitator timeout or error, a crash, a SIGKILL, a stop cut short, a receipt
that could not be written) leaves the intent, and the intent resolver (in the same process, every
`ONYX_REPORTER_EVERY`) reads the chain:

- if the USDC token's logs show this authorization's own `Transfer` (the log right after its
  `AuthorizationUsed`) of the exact amount into OnyxRevenue, the intent becomes a receipt, and is
  removed once that receipt is on disk;
- if the payer cancelled the authorization (`AuthorizationCanceled`), the intent is dropped;
- if, at a block past `validBefore` + 10 minutes by the chain's own clock, the token says the
  authorization is unused, or its use is found sending the value elsewhere, or (used) two complete
  reads of its whole chain-time span, plus one read extended back, find no payment of ours, the
  intent is dropped;
- otherwise (a node error, a node that is behind, a read not yet complete) it stays.

Every intent therefore ends within `validBefore` + 10 minutes + a bounded resolver delay, whatever
the payer does (OPS-L4): the caps below cannot be held full by stuck intents.

While the receipt log cannot write, the resolver removes nothing; restart (after draining, see
below) and it resumes. What this does not cover: losing or editing the state directory. Payments
are refused with 503 while a journal is failing or the receipt log is full, with 429 while one
payer already has 4 unsettled intents (settles in flight do not count), with 409 when the same
authorization is already being settled for another request, with 503 `clock_skew` while this
server's clock is more than 300 s off the chain's (keep NTP running), and with 402 when an
authorization is valid for longer than `maxTimeoutSeconds` + 300 s or expires before a settle can
be mined (x402 clients sign `now + maxTimeoutSeconds`). Unknown outcomes are
logged with payer, nonce, round, shard and amount, counted on `/v1/health` (`settles`), and the
stop line reports them. The resolver needs only read access to the node, so it runs with
`ONYX_REPORTER=off` too. It reads at most 200 blocks per `eth_getLogs`, at most 400 windows and
200 intents per pass, earliest expiry first (a long search resumes on the next pass), and never early history, which the
public Base Sepolia node refuses (OPS-L2-N1).

Smoke test a running node with one paid request: `ONYX_SMOKE_ADDR=127.0.0.1:8080
ONYX_SMOKE_PATH=/v1/proofs/<round>/<shard>/<index> ONYX_SMOKE_KEY_FILE=payer.key cargo test -p
onyx-node --test smoke_client -- --ignored --nocapture`. Use a fresh key (`cast wallet new`) funded
with test USDC, never a well-known one: some carry an EIP-7702 delegation on Base Sepolia, and USDC
then refuses their signatures (OPS-L2-N2).

## Get the inputs

After a deploy, the demo driver writes everything except the rows' long-term home:

```sh
# Local chain (anvil): settlement only, payments off.
scripts/demo-local.sh --serve --export-node /tmp/onyx-node
# Base Sepolia (after funding; docs/ops/demo-testnet.md): adds OnyxRevenue and self-settle.
scripts/demo-testnet.sh testnet --dispute --yes --export-node "$PWD/.onyx-testnet/node"
```

`--export-node <dir>` writes `deployment.json` (with the settlement's `deployBlock` and the
registry pin `registry`/`registryCodeHash`: the export first checks the registry's runtime code is
the checkout's `OnyxRegistry` build, immutables aside, and that its `ledger()` is the settlement; L16),
`catalog.json` (the shard under `held`: the node's chain indexer serves it once it finds the
`Finalized` event and the rows hash to the contract's root, ADR-0033; with the reserve tail files
also the `btc.entity-reserve` shard, which only `onyx-node-btc` serves), `cas/` (the row batches
and each spec's params)
and two settings files (both with `ONYX_INDEXER=on`), both mode 0600 (a remote node URL is never written, since it can hold
an API key):

- `node.env`: run `onyx-node` by hand from that directory (paths point into it).
- `node.service.env`: install as `/etc/onyx/node.env` for systemd or Docker. It has no
  `ONYX_STATE_DIR`, `ONYX_CAS_DIR` or `ONYX_OPERATOR_KEY_FILE`, which the unit and the docker
  command set (a value in the env file would override the unit's, OPS-L1-F5). Fill in
  `ONYX_PUBLIC_URL` and the node URL.

A testnet export writes `ONYX_RIGHTS=fallback` in both files: a testnet node refuses to start
without `ONYX_RIGHTS` (review L14-R1), and the label-set rights are not cleared, so the export
serves no label-derived metric (no OKX reserve sale, 403 `label_rights_withheld`). Change it to
`cleared` only after launch-checklist LC-1 path A (rights cleared in writing).

## Run with Docker

The image is built from an allowlisted context (`.dockerignore`: only `Cargo.*`, `crates/`,
`adapters/`, `recipes/`, `apps/web/verify/`, and for `onyx-node-btc` the three committed reserve
fixtures, `fixtures/SHA256SUMS` and `deploy/reserve-gate.json`; never `.env`, `*.key`, `target/`,
`fixtures-live/`, `.onyx-testnet/`, logs or run state). Base images are pinned by digest. The
runtime image is distroless (no shell), runs as uid 65532 (`USER 65532:65532`), has a
`HEALTHCHECK` (`onyx-node healthcheck`: GET `/v1/health` on `ONYX_LISTEN`, every 30 s, with a
60 s start period during which Docker 25+ probes every 5 s; `healthy` about 6 s after start in
the smoke run) and keeps state on a volume. Without BuildKit (the classic builder) `docker build`
of the default target also builds the `onyx-node-btc` stages, so the first build compiles both
(597 s on 4 cores) and the `--target onyx-node-btc` build after it is all cache. BuildKit
(`docker buildx build`, or `DOCKER_BUILDKIT=1 docker build` with the buildx plugin) builds only
the target's stages: 282 s for `onyx-node` and 196 s for `onyx-node-btc` on 4 cores, with the
same image sizes (49.8 MB and 53.8 MB) (docs/audit/2026-10-15-fresh-install.md). With a `docker-container` builder add
`--load` so the image reaches `docker images`. The Dockerfile's `# syntax=` frontend is pinned by
digest too (BuildKit pulls and runs it; L7-BK-1). `scripts/docker-smoke.sh` builds
both images and runs every step below against a local chain (docs/audit/2026-10-13-docker.md).

The smoke publishes fixed host ports (`127.0.0.1:18545`, `18080`, `18081`; the fork driver's own
servers on `18403`-`18405`), so two runs on one machine (several worktrees) take turns: after its
image builds, which bind no port and may overlap, each run takes an exclusive `flock` on
`/run/user/<uid>/onyx-docker-smoke.lock` (else `/tmp/onyx-docker-smoke-<uid>.lock`; by user id, not
`XDG_RUNTIME_DIR`, which cron, `env -i` and some sandboxes unset or move; `ONYX_SMOKE_LOCK` to
change it; mode 0600, a symbolic link or another user's file in `/tmp` is refused) and holds it
until its containers, volumes and network are removed, also on a failure, Ctrl-C (a second one
during cleanup is ignored) or `SIGTERM`. A waiting run prints one line naming the holder (its PID,
its `onyx-smoke-<pid>-*` containers, its worktree directory name and start time; printable
characters only) and fails after `ONYX_SMOKE_LOCK_TIMEOUT` seconds (default 3,600) with
`timed out after ... s waiting for the lock`. The lock is taken even when
`ONYX_SMOKE_RPC_PORT` and `ONYX_SMOKE_API_PORT` are overridden; runs on other ports overlap only
with their own `ONYX_SMOKE_LOCK` (and `ONYX_SMOKE_DEMO_PORT`). A run killed with `SIGKILL` frees
the lock (the demo driver, which `demo-testnet.sh` starts in the background, never gets the lock's
file descriptor; a command in flight, at most `docker wait`'s 150 s, holds it until it ends) but
leaves its containers: remove `onyx-smoke-<pid>-*` by hand once that PID is gone.
With the lock taken, a run still waits (within the same timeout, naming the container or host
process; `docker ps` bounded by 10 s) while any of those ports is in use, for example by a
checkout from before the lock. The default lock file is per user; runs under other users need a
shared `ONYX_SMOKE_LOCK` outside `/tmp`. `scripts/smoke-lock-tests.sh` (in check-all, no Docker) tests the lock.

Lay out `/etc/onyx` from the `--export-node` directory. The container user (uid 65532) must be
able to read everything it mounts; `node.env` stays root-only, since only the docker CLI reads it:

```sh
sudo install -d -m 0755 /etc/onyx
sudo install -m 0600 <export>/node.service.env /etc/onyx/node.env   # then fill in ONYX_PUBLIC_URL, the node URL
sudo install -m 0644 <export>/deployment.json <export>/catalog.json /etc/onyx/
sudo cp -r <export>/cas /etc/onyx/cas && sudo chmod -R a+rX /etc/onyx/cas   # row batches and spec params
```

```sh
docker build -t onyx-node:local .
docker volume create onyx-state
docker run -d --name onyx-node --read-only --restart unless-stopped --stop-timeout 75 \
  --memory 2g --cap-drop ALL --security-opt no-new-privileges \
  -p 127.0.0.1:8080:8080 \
  --env-file /etc/onyx/node.env \
  -e ONYX_LISTEN=0.0.0.0:8080 -e ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1 \
  -e ONYX_DEPLOYMENT=/etc/onyx/deployment.json -e ONYX_CATALOG=/etc/onyx/catalog.json \
  -e ONYX_CAS_DIR=/etc/onyx/cas -e ONYX_STATE_DIR=/var/lib/onyx/state \
  -v /etc/onyx:/etc/onyx:ro -v onyx-state:/var/lib/onyx/state \
  onyx-node:local
docker logs onyx-node
docker inspect --format '{{.State.Health.Status}}' onyx-node
```

`-e` values override the same names in `--env-file` (here `ONYX_LISTEN`, which
`node.service.env` sets to `127.0.0.1:8080` for systemd: inside a container the server must
listen on `0.0.0.0` for `-p` to reach it). onyx-node refuses a listen address that is not
loopback unless `ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1` (M7-7, "TLS in front" below): set it here
because `-p 127.0.0.1:8080:8080` keeps the port on the host's loopback. Never pair the opt-in
with `-p 8080:8080` (all interfaces). `--memory 2g`, `--cap-drop ALL` and
`no-new-privileges` match the systemd unit's `MemoryMax=2G`, empty `CapabilityBoundingSet=` and
`NoNewPrivileges=yes`; the server needs no capability (it listens above port 1024). A fresh
named volume takes the image's `/var/lib/onyx/state` (uid 65532, mode 0700); a bind mount
instead must be owned by uid 65532 (`sudo chown 65532:65532 <dir> && sudo chmod 0700 <dir>`).
Check the settings first with the same flags and `--check-config` appended (no network).

`--stop-timeout 75` gives the server time to finish settles that already reached the chain
(see "What runs"); Docker's default of 10 s would kill them. `--env-file` values show in
`docker inspect`: for a node URL with an API key, mount it as a file instead (owned by uid
65532, mode 0600) and set `-e ONYX_RPC_URL_FILE=/etc/onyx/rpc-url` with no `ONYX_RPC_URL`.

The node URL is `https://`, or `http://` to localhost only. Inside a container, localhost is the
container itself: a dev chain on the host's loopback (anvil from `scripts/demo-local.sh`) is not
reachable. Run the chain in a container and start the server in its network namespace
(`--network container:<anvil container>` instead of `-p`; publish the ports on the anvil
container), as `scripts/docker-smoke.sh` does. Payments need the Base Sepolia network: on
demo-local's chain 31337 the server refuses `self-settle` and `facilitator` (`network
eip155:31337 is not a testnet this project accepts`), so a dev chain runs with
`ONYX_PAYMENTS=off` (rows answer 200, free); for a paid rehearsal use the fork
(`scripts/demo-testnet.sh fork --export-node <dir>`, chain 84532, nothing broadcast). Rows are
free once their data is older than the catalog's `freshWindowSecs` (24 h in the export, counted
from the data's own time), so the committed fixtures answer 200 even with payments on.

`onyx-node-btc` runs with the same flags (`docker build --target onyx-node-btc -t
onyx-node-btc:local .`, image `onyx-node-btc:local`). Its default `ONYX_RESERVE_GATE` is the
shipped `/usr/share/onyx/deploy/reserve-gate.json`. A config of your own goes in the `/etc/onyx`
mount with `-e ONYX_RESERVE_GATE=/etc/onyx/reserve-gate.json`, and its `fixtures` must then be
the absolute `/usr/share/onyx/fixtures` (a relative path is read from the config's directory).
At start it writes every catalog spec's params into `ONYX_CAS_DIR` for the serving check; with
the read-only mount above they must already be there (`--export-node` writes them under
`cas/blob/`), or the start stops with `ONYX_CAS_DIR (spec params): ...`.

The operator key (self-settle) goes in a read-only mount: put it at `/etc/onyx/operator.key`
owned by uid 65532, mode 0600, and set `-e ONYX_OPERATOR_KEY_FILE=/etc/onyx/operator.key`.
The server refuses a key file that group or others can read. Publish the port on loopback and
put the TLS proxy in front ("TLS in front" below); then set `ONYX_PUBLIC_URL` to the https URL and
`ONYX_TRUSTED_PROXIES` to the proxy's address (run-api.md, "Behind a reverse proxy").

Images are never pushed to a registry by this project.

## Run with systemd

`deploy/systemd/onyx-node.service` runs the binary with a dynamic user, a private state
directory and a strict sandbox (`systemd-analyze security` rates it 1.2, "OK", on systemd 259).
The operator key is passed as a systemd credential, readable only by the service. Tested on a
fresh Ubuntu 26.04 with systemd as PID 1 (docs/audit/2026-10-15-fresh-install.md).

Needs a VM or bare-metal host. The sandbox uses mount namespaces, which an unprivileged
container does not allow: under Docker's defaults the start fails with `status=226/NAMESPACE`.
In a container, use the Docker image instead.

To build on the machine: Rust through [rustup](https://rustup.rs) (`rust-toolchain.toml` makes
cargo fetch 1.98.0 on the first build) and a C compiler and libc headers (`gcc`, `libc6-dev` on
Debian and Ubuntu). Or build on another machine with the same or newer glibc and copy the binary
over. Run the commands from a checkout of the repo; `<export>` is the `--export-node` directory
copied to this machine.

```sh
cargo build --release --locked -p onyx-node
sudo install -m 0755 target/release/onyx-node /usr/local/bin/onyx-node
sudo install -d -m 0755 /etc/onyx
sudo install -m 0600 <export>/node.service.env /etc/onyx/node.env
sudoedit /etc/onyx/node.env      # set ONYX_PUBLIC_URL (uncomment it) and check ONYX_RPC_URL
sudo install -m 0644 <export>/deployment.json <export>/catalog.json /etc/onyx/
sudo install -m 0600 operator.key /etc/onyx/operator.key        # if payments are on
sudo install -m 0644 deploy/systemd/onyx-node.service /etc/systemd/system/
sudoedit /etc/systemd/system/onyx-node.service   # payments off: delete both key lines (table below)
sudo systemctl daemon-reload
sudo systemctl start onyx-node   # creates /var/lib/onyx/{state,cas}; serves 0 shards until the rows are in
sudo cp -r <export>/cas/. /var/lib/private/onyx/cas/   # the row batches and spec params
sudo chown -R --reference=/var/lib/private/onyx/cas /var/lib/private/onyx/cas   # owned by the service's user
sudo systemctl enable --now onyx-node   # starts it again at boot
journalctl -u onyx-node -f
```

What a good start looks like, and how to check it:

- `journalctl -u onyx-node` shows `config ok: chain …`, then `listening on 127.0.0.1:8080`,
  and once the indexer finds the round's `Finalized` event and the copied rows hash to its
  root, `indexer: round 1 shard 0x… is final on chain and its held rows hash to the root:
  serving it` (no restart needed after copying the rows; it happens on the next poll,
  `ONYX_INDEX_EVERY_SECS`).
- `curl -fsS http://127.0.0.1:8080/v1/health` answers 200 with `"status":"ok"` and
  `indexer.servable` at the number of held shards; `onyx-node healthcheck` does the same GET on
  `ONYX_LISTEN` (default `127.0.0.1:8080`) and exits 0 on 200.
- `sudo systemctl stop onyx-node`, then `systemctl show onyx-node -p ExecMainStatus -p Result`:
  `ExecMainStatus=0`, `Result=success` (the log says `stopped (drained: …)`).
- `systemd-analyze security onyx-node`: `1.2 OK`. (`systemd-analyze verify` on the unit in the
  checkout also reads the `onyx-node.service.d/btc.conf` drop-in next to it, and reports the
  missing `/usr/local/bin/onyx-node-btc` unless that is installed; verify the installed unit.)

Errors seen on a first install, and their cause:

| Journal | Cause |
|---|---|
| `status=243/CREDENTIALS`, `Failed to set up credentials: No such file or directory` | The key lines are in the unit but `/etc/onyx/operator.key` does not exist: payments off needs both lines deleted |
| `ONYX_PUBLIC_URL is required` (`status=2/INVALIDARGUMENT`) | `node.service.env` leaves it commented out |
| `status=226/NAMESPACE` | The host does not allow the sandbox's mount namespaces (an unprivileged container) |

The state directory exists from the first start even when the start fails. The unit's
`Restart=on-failure` restarts a crashed server after 5 s (a SIGKILL test restarted it once,
`NRestarts=1`).

`/etc/onyx/node.env` must not set `ONYX_STATE_DIR`, `ONYX_CAS_DIR` or `ONYX_OPERATOR_KEY_FILE`:
the unit sets them, and a value in the env file would override the unit's (`node.service.env`
leaves them out). The operator key lines come as a pair, `LoadCredential=operator-key:…` and
`Environment=ONYX_OPERATOR_KEY_FILE=%d/operator-key`:

| Payments | Key lines |
|---|---|
| `self-settle` | keep both |
| `facilitator` (receipts posted by the reporter) | keep both |
| `facilitator` with `ONYX_REPORTER=off` (receipts kept on disk, unposted) | delete both |
| `off` | delete both |

The server refuses a key path it cannot read, and facilitator mode without a key unless
`ONYX_REPORTER=off` is set. The node URL can also be a credential: put it in `/etc/onyx/rpc-url`
(one line, mode 0600), uncomment the `rpc-url` lines in the unit and remove `ONYX_RPC_URL` from
`node.env` (setting both is refused). systemd hands each credential to the service as a
root:root 0440 file in its own root:root credentials directory that others cannot enter; the
server accepts exactly that there (a regular file, not a symlink, owner and group root) and
keeps the 0600 rule everywhere else (L7-SD-1: before, it refused every credential, so the unit could not start with a
key or `rpc-url`).

## TLS in front (ROADMAP L11, THREATS.md M7-7)

**Rule: a public `ONYX_LISTEN` is never plain HTTP.** onyx-node has no TLS of its own. It
listens on loopback (`127.0.0.1:8080`, the default and what `node.service.env` sets) and a TLS
reverse proxy on the same host is the only thing the network reaches. The server enforces the
loopback half: an `ONYX_LISTEN` that is not loopback (127.0.0.0/8, `::1`) stops the start and
`--check-config` with exit 2 and `ONYX_LISTEN ... is not a loopback address: this server speaks
plain HTTP ...`, unless `ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1` is set
(`config::tests::m7_7_a_public_listen_address_needs_an_explicit_opt_in`). The opt-in is for a
place where the port still cannot be reached from outside: a container published on
`127.0.0.1` ("Run with Docker"; `scripts/docker-smoke.sh` sets it for that reason) or a testnet
lab on a private network. With it the server prints `onyx-node: warning: ONYX_LISTEN ... is not
loopback and serves plain HTTP` at every start. It never makes a public port safe.

The proxy is Caddy (choice and sources: RESEARCH.md "TLS in front of onyx-node"), config
`deploy/caddy/Caddyfile`:

| What | Setting |
|---|---|
| Certificate | automatic over ACME (Let's Encrypt or ZeroSSL), renewed by Caddy; `ONYX_CADDY_TLS` is the ACME contact (a role mailbox) |
| Plain HTTP | port 80 answers `308` to `https://` (an explicit redirect block, so the timeouts and header cap apply there too; ACME HTTP challenges still work); nothing is proxied over it |
| HSTS | `Strict-Transport-Security: max-age=31536000` (this host only), on Caddy's own errors (413, 502) too (`handle_errors`) |
| Protocol | HTTP/1.1 only (no HTTP/2 or 3): one request at a time per connection, so a per-address connection cap also caps requests in flight |
| Proxied | only `GET`/`HEAD` on the onyx-api routes (`/v1/health`, `/v1/proofs/*`, `/v1/metrics/*`, `/v1/export/*`, `/v1/settlement/*`, `/v1/served/*`, `/v1/observations/*`); anything else `404` at the proxy |
| Request body | at most 1 KiB (`413`): no onyx-api route reads a body. `request_buffers 4KiB` makes Caddy read the body before dialing; without it a 2 KiB body passed, since onyx-node answered before Caddy read past the cap |
| Headers | at most 32 KiB (`431`); a payment header alone may be 16 KiB |
| Timeouts | headers 10 s, body 10 s, whole response 75 s (12 s request + 60 s send), idle 60 s; upstream dial 3 s, answer 15 s |
| Client address | Caddy drops the client's `X-Forwarded-For` and sends the client's own address |

`tests/caddyfile.rs` (onyx-node) checks the file against the server: every router path proxied
and nothing else, the header cap above a full payment header, the write timeout above the
server's deadlines, no `trusted_proxies` and no `X-Forwarded-For` rewrite in the file.

Set up (the domain and host are the operator's; the box needs ports 80 and 443 open and the
domain's A/AAAA records pointing at it):

```sh
# onyx-node under systemd: /etc/onyx/node.env
ONYX_LISTEN=127.0.0.1:8080          # not [::1]: Caddy would connect from ::1, which 127.0.0.1 does not trust
ONYX_TRUSTED_PROXIES=127.0.0.1
ONYX_PUBLIC_URL=https://api.example.org
ONYX_INDEX_HEALTH=summary
# Caddy (the release binary from github.com/caddyserver/caddy, checked against its checksums file)
ONYX_CADDY_DOMAIN=api.example.org ONYX_CADDY_TLS=ops@example.org \
  caddy run --config deploy/caddy/Caddyfile --adapter caddyfile
```

Set both variables. With `ONYX_CADDY_TLS` set but `ONYX_CADDY_DOMAIN` unset, Caddy adapts the file
to catch-all `https://` and `http://` sites with no host: it serves no certificate (safe, but
silent), so check `caddy adapt` names your domain before going live.

**In Docker** (`-p 127.0.0.1:8080:8080`, "Run with Docker") the server keeps `ONYX_LISTEN=0.0.0.0:8080`
with the opt-in, and Caddy's connections do not arrive from 127.0.0.1: Docker's port proxy
connects from the bridge gateway (`docker network inspect bridge --format
'{{(index .IPAM.Config 0).Gateway}}'`, usually `172.17.0.1`). Set
`-e ONYX_TRUSTED_PROXIES=172.17.0.1` (that address, /32). With `127.0.0.1` the forwarded header
is ignored and every client shares one rate-limit bucket and the proxy's connection cap (review
finding TLS-7). Check it once: two clients, as in the audit's section 6, must get separate limits.

Under systemd, run Caddy as its own unit with those two variables in an `EnvironmentFile`
(the Caddy project ships one, `caddy.service`, in its dist repository). Ports 80 and 443 need
`CAP_NET_BIND_SERVICE` for a non-root Caddy. The file has no admin endpoint (`admin off`):
change it and restart Caddy.

Not in the file, so the host has to provide it:
- **Per-client connection cap.** onyx-node exempts its trusted proxy from `ONYX_MAX_CONNS_PER_IP`
  (AUD-1R-F1), and stock Caddy has no per-client connection limit. Cap new connections per
  source address in the host firewall, or accept that one client can hold many proxy
  connections (THREATS TLS-2). The ready file is `deploy/nftables/onyx-conn-cap.nft` (32 open
  connections per IPv4 address or IPv6 /64 on 443, its own table, loaded by
  `deploy/systemd/onyx-conn-cap.service`; tested with `scripts/conn-cap-test.sh`). The file serves
  HTTP/1.1 only, so that cap also bounds the client's requests in flight; onyx-api still has one
  global cap of 256 in flight (503 `overloaded`) that 8 such clients could fill.
- **Rate limits** stay in onyx-node, keyed on the forwarded client (payments 30/min, free
  exports 6/min, level-2 attempts 30/min).
- **`includeSubDomains` / preload** for HSTS: only if every subdomain of the domain is HTTPS.

## Backups

- **State dir** (`/var/lib/onyx/state` or the `onyx-state` volume): receipts not yet credited,
  settle intents not yet resolved (`settle-intents.jsonl`, OPS-L2-F1) and credits in flight. Losing it loses fees that were paid but not yet attributed (they stay
  unallocated in OnyxRevenue). Back it up while the server is stopped, or copy it while running
  and accept a torn last line (the journal drops it on open). Restore only to the same
  deployment: journals are bound to one chain and contract and refuse others (REV-L3-F4).
- **Rows** (`cas/`): re-creatable from the miners' published batches; back up for speed.
- **Manifest, catalog, node.env**: keep in the private ops store (never the key; `node.env`
  only if its node URL has no API key, else store it with the secrets).
- **Operator key**: keep an offline copy; on loss, the owner rotates the reporter
  (`OnyxRevenue.setReporter`) to a new key.

## Upgrade and rollback

1. Drain receipts first: wait for `receiptsPending == 0` in `/v1/health` (or accept that the
   new process reposts them; credits are idempotent on chain).
2. Docker: `docker build -t onyx-node:<git sha> .`, then `docker stop -t 75 onyx-node` (SIGTERM;
   the stop drains requests and their settles, up to about 66 s), `docker rm onyx-node`, and run
   the new tag with the same volume and `--stop-timeout 75`.
   systemd: install the new binary and `systemctl restart onyx-node`.
3. Check `onyx-node --check-config` before the restart and `/v1/health` after.
4. Rollback: run the previous tag or binary with the same state volume. The journal format is
   versioned by its header; a downgrade that cannot read it refuses to start instead of losing
   entries.

Only one process may own a state directory (lock file, REV-L3-F4); run a second replica with
its own state dir. Use a local disk, not NFS (REV-L4-F7).

## Chain indexer (ADR-0033)

With `ONYX_INDEXER=on` the node lists only the shards whose `Finalized` events it found and
whose held rows hash to the contract's root. At start it refuses a manifest `deployBlock` that is
not where the settlement's code appeared (`eth_getCode` empty at `deployBlock - 1`, non-empty at
`deployBlock`; an old deployment needs a node that keeps old state, IDX-R1-4). Node errors (429,
timeouts) are retried for about 6 s; the error then says which it is: the node failing (retry
or change node), its head below `deployBlock` (wrong network or still syncing), no old state
(use an archive node), or the manifest's block is wrong (IDX-R2-2). The check runs until it
passes once: a passed check is recorded in the indexer state file (`deployChecked`, bound to the
chain, contract and deploy block), and later starts with that file skip it, so a restart needs
no old state (SOAK-4: before, every restart of a deployment older than the node's state window
failed). A new or deleted state file, or one written before this change, is checked again.

Every node read the start makes (the settlement's and OnyxRevenue's code, the registry floors,
level-2 pins, the deploy block) is tried up to 8 times, waiting 0.5 s and doubling to 8 s
(about 31 s per read; `onyx_node::app::STARTUP_TRIES`, SOAK-2): a node that fails some calls
(429, 5xx, timeouts) no longer stops the start, and each failed try is logged
(`onyx-node: start: ...`). A failure that lasts (wrong chain, wrong code hash, node down) still
stops it after the tries, with exit code 1; the systemd unit (`Restart=on-failure`) and the
documented `docker run --restart unless-stopped` start it again.

- **Use an RPC node you trust** (your own, or a provider you trust). The indexer and the
  per-request root read (`ChainRoots`) both use `ONYX_RPC_URL`, so the per-request read adds
  nothing against a lying node: a node that lies can make this server list and serve a wrong
  or unfinal root (ADR-0033 Amendment 5, IDX-R5-3). Correctness against a lying operator node
  rests on clients that check roots on their own node (the default since M7 loop 3:
  `client::ClientNode::verify`, or `verify_response` with a chain answer; only the explicit
  `verify_response_unchecked` returns `chain_checked: false`, see delivery.md "Verification"). A
  lying node can also delay serving a shard you hold until it stops lying and a held-shard
  rescan or a restart runs. The rescan reads the held shards' logs again from `deployBlock`
  from the third poll after every start and then `ONYX_INDEX_RESCAN_POLLS` polls (default 240,
  one an hour) or `ONYX_INDEX_RESCAN_SECS` after the previous one ended; it reads every
  confirmed end and every served shard's final view again (a served shard two reads on two
  polls refute leaves the catalog, `removed` on `/v1/health`; a read from a backend below the
  head the shard was admitted at is ignored, IDX-R6-1). A rescan in progress is saved in the
  state file, so a restart resumes it instead of starting again from `deployBlock` (IDX-R6-2).
- **Rescan cost** (IDX-R5-4). A rescan costs one `eth_getLogs` per `ONYX_INDEX_SPAN` blocks
  since `deployBlock` per 64 held shards, at most `ONYX_INDEX_RESCAN_CALLS` (default 100) per
  poll, resumed on the next poll; the scan of new blocks keeps its own budget. At Base's 2 s
  blocks (43,200 a day) and the defaults (15 s polls, span 200), per 64 held shards:

  | Deployment age | Calls per rescan | Rescan lasts | Calls/day, hourly (default) | Calls/day, daily |
  |---|---|---|---|---|
  | 1 month | 6,480 | 65 polls (16 min) | ~122,000 | 6,480 |
  | 1 year | 78,840 | 789 polls (3.3 h) | ~441,000 | 78,840 |
  | 1 month, span 10,000 | 130 | 2 polls | ~3,100 | 130 |
  | 1 year, span 10,000 | 1,577 | 16 polls | ~35,500 | 1,577 |

  "Hourly" is 240 polls after each rescan ends; "daily" is `ONYX_INDEX_RESCAN_SECS=86400`.
  Restarts no longer reset a long rescan (it resumes from the saved cursor, IDX-R6-2). On
  top, each rescan reads one `getFinalizedRoot` per served shard, and the scan costs about one
  `eth_getLogs` per poll (5,760 a day). To shrink it: use a node
  whose `eth_getLogs` accepts wider ranges (check its documented limit; sepolia.base.org allows
  200) and raise `ONYX_INDEX_SPAN` to match (up to 10,000), and on an older deployment set a
  daily `ONYX_INDEX_RESCAN_SECS`. A longer interval only delays recovery from a lying node.
- **Cursor jumps** (IDX-R5-5). A poll reads at most `ONYX_INDEX_MAX_JUMP` blocks (default 100,
  the overlap) past the `finalized` head this poll and the previous one both reported, so one
  lying answer far ahead moves the cursor that far at most and the overlap reads those blocks
  again. A start therefore reads up to the head on its second poll. It must be at most
  `ONYX_INDEX_OVERLAP` (a start refuses it otherwise, IDX-R6-3).
- **Health.** `/v1/health` `indexer` shows the cursor (`nextBlock`, `finalizedHead`), counts
  (`servable`, `pending`, `notHeld`, `ended`, `unservable`), `conflicts` (re-reads that gave a
  different log at a known position; the contract's view decides, IDX-R1-1),
  `candidatesDropped` and `candidatesEvicted` (a position held 4 logs, which an honest node
  never causes: change providers), `partialBlocks` (blocks read for the held shards only,
  IDX-R1-2), `rescans` and `rescanning` (held-shard rescans finished since start, and one
  running) and `stateWriteErrors`. By default it also
  lists up to 64 shard ids with their status and the last error's text: that tells anyone who
  can reach the node which shards this operator holds and which node errors it sees. On a node
  the public can reach, set `ONYX_INDEX_HEALTH=summary` (counts only, and `failing` instead of
  the error text; the text stays in the node's log).
- **Reset the cursor** (not needed for conflicting, forged or withheld logs: after a change of
  provider, a restart's held-shard rescan finds a held shard's log again; needed for a state
  file from another deployment or a hand edit): stop the node, move
  `$ONYX_STATE_DIR/indexer.json` (or `ONYX_INDEX_STATE`) aside, start it. The scan reads again
  from `deployBlock` (about one `eth_getLogs` per 200 blocks) and serves the same shards once
  their events are found again.

## Contracts on Base Sepolia

`scripts/demo-testnet.sh testnet --plan`, then `--dispute --yes` (docs/ops/demo-testnet.md).
It deploys the settlement contracts and OnyxRevenue, refuses any chain but 84532, and records
the addresses and code hashes in the manifest above.

### Deployed addresses (Base Sepolia)

Each live run deploys a fresh set; `apps/web/static/run/base-sepolia-*.json` lists the runs the
site shows. The latest (NS17, run-20261027T024839Z, 2026-10-27; REHEARSAL PASSED; the hosted API
node at `https://onyx.cryptagraph.com/v1/` indexes this set, [site-hosting.md](site-hosting.md)):

| Contract | Address |
|---|---|
| OnyxSettlement (+ Ext) | `0xe4be87932d29ecb85c332ef7057e12a7a83aae8f` |
| OnyxDisputes | `0x43dc8ca60fa7b4b99284d60aa909d6f54b552c5d` |
| OnyxRevenue (x402 `payTo`) | `0xa32d520380362378d22a7a51993678c7fbbfe44d` |
| OnyxRegistry | `0x497389e04f7d58e07a79ac8d395c3927aecbdf2b` |
| USDC (Circle's Base Sepolia test token) | `0x036cbd53842c5426634e7929541ec2318f3dcf7e` |
| Deployer (coordinator, OnyxRevenue owner and treasury) | `0x80c91f45b8353ff5c2b1e4c127a2a21746b37972` |
| API operator (OnyxRevenue reporter, self-settles x402 payments) | `0xf1b27c3f48e9972eb8050e6dff8594e05e3e488c` |
| Level-2 receipt signer (ROADMAP B3, B6: a key of its own, set by the first level-2 round) | `0x7420f8cba077db54848bb9a1cbb19fc5e12ad6b3` |
| OnyxSignedBook (level-3 book and bulk manifests, CL5; `scripts/deploy-signed-book.sh`, 2026-10-30, tx `0x485057a75ea1bba625da407a00a98cf9da2159ad4de0f64c3a653d416a6e6144`; code hash `0x0843e5a59d2ee1d64aefa6f0487c6c1289b32c8c120ddf0550950cbf57f55d20`) | `0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5` |
| Signed book reviewer (a test key of its own, `.onyx-testnet/keys/signed-book-reviewer.key`; registers publisher keys with `cargo run -p onyx-level2 --example book-reviewer`) | `0x98afa79b24e067ddac2478468d3e1c81d4810d30` |
| Bulk demo publisher `synthetic-demo` (synthetic rows only; domain onyx.cryptagraph.com, keys document at `/.well-known/onyx-source-keys.json`) | `0x39c543ec14a6aff3e38c82f132f3ff014f671269` |

Markets next to this registry (NORTH-STAR NS11, ADR-0042 amendment 2; `scripts/markets-sepolia.sh
deploy`, 2026-10-27, block 51006420, 8 transactions, 0.000351 test ETH; the deployer mints tONYX,
owns the futures and lists facts). The file `<export>/markets.json` names them and every market:

| Markets contract | Address |
|---|---|
| tONYX test token (no value) | `0x1cb5f5816812991ac3b52c63c3fcadcc9f544579` |
| OnyxReobserve | `0xa300e10eceabc74933312bfff7dcdbce1db268e5` |
| OnyxPanelOracle (the stakes' fallback, the futures' arbiter) | `0x3c3368823af04292be0cc6d396e5b958ad7c4002` |
| OnyxStakes | `0xff67f3ec5da643719d1b839584c2b4efd0a4ac98` |
| OnyxObservedFacts (registry facts for the futures) | `0xe13b89565d037c6ea91d2bc040de78829eac473c` |
| OnyxFutures | `0xd0252e271cc218fabe591895f23a3e6e9e1180ff` |

The second markets deployment (docs/north-star-loop.md NL14 and NL18, ADR-0042 amendment 3;
`scripts/insurance-sepolia.sh deploy`, 2026-10-27, block 51070747, 3 transactions, 0.000027 test
ETH; rehearsed first on an anvil fork of Base Sepolia). It reuses tONYX, `OnyxReobserve` and
`OnyxPanelOracle` above; listings carry `closeBy` and name insiders, the futures are guarded, and
`OnyxParametric` sells test policies. Market 1 stays on the first futures (NL15). The file
`<export>/insurance.json` names them and every product; the market tool opens new markets only
there (`ONYX_MARKETS_FILE=<export>/insurance.json scripts/markets-sepolia.sh market <export> open ...`):

| Second deployment contract | Address |
|---|---|
| OnyxObservedFacts (guarded listings: `closeBy`, `isInsider`) | `0x8d10fc8f37875f15f4d180dbcaa6c2915334bb76` |
| OnyxFutures (guarded: no insider bets, close by `closeBy`) | `0x07e89ae47579f9297c28ccab782acf9443354a65` |
| OnyxParametric (test policies) | `0xfc585f41e5e615b774a24404fd5a857042f63639` |

Market 2 on the page (north-star loop NL23) is market 1 of the guarded futures: U.S. total public
debt for record date 2026-11-02, buckets under $40.60 T, $40.60-40.70 T and above $40.70 T,
trading closes 2026-11-01 23:00 UTC, observable from 2026-11-04 00:00 UTC, refunds if no value by
2026-11-11 00:00 UTC; opened 2026-10-29 with four test bets (30, 45, 20, 25 tONYX from miner-0,
miner-1, validator-0, customer) after a fork rehearsal; the deployer was refused as a bettor.

The `customer` test key underwrote 1,000 tONYX into `OnyxParametric` on 2026-10-28 (block
51534119; `scripts/markets-sepolia.sh market ... underwrite`), so the live products have cover for
sale.

The live `OnyxReobserve` and `OnyxPanelOracle` above predate THREATS LC16-1 and LC16-2: their
observer changes are instant and their seats open to any address. The code now delays observer
changes and gates seats on the settlement's staked validators (ADR-0042 amendment 3 items 7-8);
the final release redeploys both (and the stakes bound to them).

The A2 run of 2026-10-14 (settlement `0x4edc6a6329ce71adb7d65fb987b77ed460c85640`, revenue
`0x904157faf2c709032cdd0a17816ea57c2d6fd7dc`) stays on chain; nothing serves it.

The health watch reads its balance checks from these rows (copied into
[deploy/health-watch/thresholds.json](../../deploy/health-watch/thresholds.json) `addresses`; a test
in `scripts/health-watch-tests.py` fails when the two differ). After a redeploy update both.

## Website builds (AUD-2-F1)
The label sets are test fixtures whose sharing rights are not cleared
(fixtures/labels/PROVENANCE.md), so the website has two builds:

| Build | Command | Ships | Shows |
|---|---|---|---|
| Private preview (default) | `scripts/build-web.sh` | the recipe inputs, with the 9,850 test label rows | a "Private preview · test labels, sharing rights not cleared" banner on every page; the browser reruns the recipe over the labels |
| Public | `ONYX_RIGHTS=cleared ONYX_PUBLIC=1 scripts/build-web.sh` (only once the label-set rights are cleared, LC-1 path A; without `ONYX_RIGHTS` a public build is refused, review L14-R1) | no label rows: no recipe input, `demo.json` marked `public` with `runs[].input = null` | the label set's name, version, count and hash, checked against the hash pinned in the verifier; run rows checked against the pinned root instead of a rerun |
| Public, rights fallback (ADR-0035) | `ONYX_RIGHTS=fallback ONYX_PUBLIC=1 scripts/build-web.sh` | as Public, and no label-derived total: no run, live, per-block or series rows, no `history/week/`, no `reserves-week.json`, no OKX figure in the run records or the bundle | "Company totals are held back until data rights are cleared." on every page and where each total would be; blocks, pools, Treasury and SEC as in Public. Refused if any label-derived total is found (`public_build::a_fallback_build_carries_no_label_derived_totals`). Inventory: [audit](../audit/2026-10-16-rights-fallback.md) |

The public build fails closed (AUD-2R-F1): every shipped file must be on an exact path allowlist
(a `.gz` or `.br` copy, or any new file, is refused until reviewed), and it is refused if any
address of any committed label set appears in any file in any form the guard decodes: text,
base64 (any alphabet, padding or alignment), hex, `%XX`/`\xXX`/`\uXXXX` escapes, `data:` URIs,
gzip, zlib or Brotli streams, nested up to four layers. In the same forms, every checksum-valid
Bitcoin address must be a known one (test-key proof vectors, the demo attestation's test key, the
reviewed pool list), so addresses from lists never committed (a full PoR file, the excluded wBTC
and cbBTC lists, a person's address) are refused too (L8-F1). `adapters/source-btc/tests/public_build.rs`,
run by the script. Residual: an encoding the guard does not decode (e.g. encryption, a custom
cipher in a script) is not caught; the allowlist and code review are the backstop. Both builds
also run the coinbase-handle scan over `apps/web/build` and `apps/web/static/data` (AUD-2-F7).
Only a public build may be shared beyond the owner. The current artifact is the private preview.

A public deployment publishes a role contact for label corrections and removal (a team mailbox,
never a person's own address) next to the site (README "Corrections and removal").

## Open questions

- Where the API runs for the public demo (the dev box vs a hosted box). Hosted costs money: ask first.
- The public domain and host for the API (the project owner): `ONYX_CADDY_DOMAIN` and the box Caddy runs
  on (launch checklist LC-8).
