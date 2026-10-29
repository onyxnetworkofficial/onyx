# Fresh-machine install: systemd unit and BuildKit build, 2026-10-15 (ROADMAP L7)

Branch `fresh-install` from main `bbfa29a`. Result: **the shipped systemd unit installs and runs
on a fresh machine by docs/ops alone once eight doc gaps and one server bug are fixed, and both
images build with BuildKit and pass `scripts/docker-smoke.sh`.** The Docker half (classic
builder) was done in A3 ([2026-10-13-docker.md](2026-10-13-docker.md)). Nothing was pushed, no
transaction reached a public network, and `.env` was never read or copied anywhere.

## Summary

| Item | Result |
|---|---|
| systemd unit, fresh Ubuntu 26.04, systemd 259 as PID 1 | start, `/v1/health` 200, row 200, restart, clean stop (`ExecMainStatus=0`, `Result=success`), SIGKILL restarts it (`NRestarts=1`), back after a container restart (`docker restart`) |
| `systemd-analyze security onyx-node` | **1.2 OK** (same with the `btc.conf` drop-in) |
| `onyx-node-btc` drop-in `btc.conf` | starts, serving check runs, row 200, clean stop |
| Credentials (`operator-key`, `rpc-url`) | **refused by the server** (L7-SD-1); fixed, then both start |
| Build prerequisites on a bare Ubuntu 26.04 | rustup + `gcc` + `libc6-dev`: `cargo build --release --locked -p onyx-node` in 4 m 40 s (4 CPUs) |
| BuildKit `onyx-node` | 282 s, 49.8 MB (13.4 MB content) |
| BuildKit `onyx-node-btc` | 196 s, 53.8 MB (14.1 MB content) |
| `docker-smoke.sh` on the BuildKit images | **DOCKER SMOKE PASSED**, 23 checks, 94 s |
| Gaps | 10 found, all fixed: 8 in the docs (gaps 1-8), 1 server bug (L7-SD-1), 1 unpinned BuildKit frontend (L7-BK-1) |

## Tools

| Tool | Version |
|---|---|
| Host | Ubuntu 26.04.1, glibc 2.43, Docker Engine 29.1.3 (no buildx plugin installed system-wide) |
| Fresh machine | `ubuntu:26.04` + `systemd systemd-sysv dbus curl ca-certificates` (`--no-install-recommends`), systemd 259 (259.5-0ubuntu3.4) |
| buildx | v0.37.1 (released 2026-09-11; sha256 `9447199c…` checked against the release's `checksums.txt`), installed only in a private `DOCKER_CONFIG` under `/tmp/claude-1000/onyx-l7`, so the user's `docker build` is unchanged |
| BuildKit | v0.33.1 (`moby/buildkit:buildx-stable-1`), `docker-container` driver capped at 8 GB, CPUs 0-3 (as A3's classic builds) |
| anvil | 1.8.5 (host binary) |

## (a) systemd unit on a fresh machine

### The machine

A container running systemd as PID 1. The harness, not the docs, needed these choices:

```sh
docker run -d --name onyx-l7-fresh --hostname fresh --cgroupns=host \
  -v /sys/fs/cgroup:/sys/fs/cgroup:rw --tmpfs /run --tmpfs /run/lock --network host \
  --memory 6g --cpuset-cpus 4-11 --cap-add SYS_ADMIN --security-opt apparmor=unconfined \
  onyx-l7-fresh:test
```

- `--cgroupns=private` without a writable cgroup: systemd exits at boot (`Failed to create
  /init.scope control group: Read-only file system`). `--cgroupns=host` with the cgroup tree
  mounted rw boots (`systemctl is-system-running`: `degraded`, only `tmp.mount` failed).
- Without `CAP_SYS_ADMIN` and with Docker's AppArmor profile, the unit's sandbox cannot be set
  up: `status=226/NAMESPACE` (`Failed at step NAMESPACE spawning /usr/local/bin/onyx-node:
  Operation not supported`). `--cap-add SYS_ADMIN --security-opt apparmor=unconfined` fixes it.
  `--privileged` was not needed. This is gap 7: a real host has these by default.
- `--network host`: the server takes an `http://` node URL to localhost only, and the local
  chain (`scripts/demo-local.sh`) listens on the host's loopback. The container has no
  `CAP_NET_ADMIN`, so it cannot change the host's network.

The checkout is a `git clone` of `bbfa29a` copied to `/opt/onyx` (the path the unit's
`Documentation=` and `btc.conf` name). The binary is `cargo build --release --locked -p onyx-node`
from the same commit on the host (same OS; the binary needs glibc 2.34), copied to
`/opt/onyx/target/release/`. The documented build prerequisites were checked separately on a bare
`ubuntu:26.04` (below).

### Inputs (deployment.md "Get the inputs", local chain)

```sh
ONYX_DEMO_RPC_PORT=28545 ONYX_DEMO_API_PORT=28403 ONYX_DEMO_DRIVER=target/release/examples/demo-local \
  scripts/demo-local.sh --serve --export-node /tmp/claude-1000/onyx-l7/export
```

```
  round      1, shard 0xacbcbfa203b99d9cba691cc1958b3c6563f04f8eab94db5a0402597b1ca34244
  chain      110 transactions, 453 blocks, 41 s
Serving on http://127.0.0.1:28403 until Ctrl-C.
```

The export (`catalog.json`, `deployment.json`, `cas/{blob,rows}`, `node.env`,
`node.service.env`, both env files 0600) was copied to `/root/export` on the machine.
`node.service.env` holds `ONYX_ENV=dev`, `ONYX_LISTEN=127.0.0.1:8080`, a commented
`# ONYX_PUBLIC_URL=…`, the `/etc/onyx` paths, `ONYX_ALLOW_TEST_LABELS=1`, `ONYX_INDEXER=on`,
`ONYX_PAYMENTS=off` and `ONYX_RPC_URL=http://127.0.0.1:28545`.

### The documented steps, as run

1. Literally, from the checkout (the steps' `target/` and `deploy/` paths are relative to it):

   ```
   + install -m 0600 node.service.env /etc/onyx/node.env
   install: cannot stat 'node.service.env': No such file or directory
   + install -m 0644 deployment.json catalog.json /etc/onyx/
   install: cannot stat 'deployment.json': No such file or directory
   ```
   Gap 1: three working directories in one block. Run again with `<export>/`.

2. Unit installed as shipped, `daemon-reload`, `systemctl start onyx-node` (payments off, no
   key file, so the docs' "if payments are on" step was skipped):

   ```
   Process: 116 ExecStartPre=/usr/local/bin/onyx-node --check-config (code=exited, status=243/CREDENTIALS)
   onyx-node.service: Failed to set up credentials: No such file or directory
   ```
   Gap 2: the table saying "payments off: delete both key lines" came after the steps.

3. Key lines deleted, start again (machine with the sandbox allowed):

   ```
   onyx-node: ONYX_PUBLIC_URL is required
   onyx-node.service: Control process exited, code=exited, status=2/INVALIDARGUMENT
   ```
   Gap 3: the systemd block never said to set `ONYX_PUBLIC_URL` (only the Docker block and "Get
   the inputs" did).

4. `ONYX_PUBLIC_URL=http://127.0.0.1:8080`, start:

   ```
   onyx-node: config ok: chain 31337, settlement 0xcf7e…0fc9, payments Off, indexer on from block 1, 1 held shard(s)
   onyx-node: listening on 127.0.0.1:8080 (http://127.0.0.1:8080); chain 31337 via 127.0.0.1:28545; …
   ```
   `/v1/health`: 200, `indexer.servable` 0 (no rows yet). `/var/lib/onyx` →
   `private/onyx` (owner the dynamic uid 63899, 0700) with `state/indexer.json`.

5. `cp -r <export>/cas/. /var/lib/private/onyx/cas/`, `systemctl enable --now onyx-node`. On the
   next poll, without a restart: `indexer: round 1 shard 0xacbc… is final on chain and its held
   rows hash to the root: serving it`; `/v1/health` `servable` 1;
   `GET /v1/metrics/btc.entity-flow?subject=entity:okx`: **200** (`asOf` 921082, proof, `license`).
   The copied `blob/` and `rows/` stay root-owned 0755/0644: readable, but the service cannot
   write there (onyx-node-btc writes spec params into `ONYX_CAS_DIR/blob` at start), and systemd
   does not chown them on a later start (checked: still `0:0` after a restart). Gap 4:
   `chown -R --reference=/var/lib/private/onyx/cas /var/lib/private/onyx/cas` added; row still
   200 after it.

6. Restart, stop, crash, container restart:

   ```
   pid 128 -> 198 (systemctl restart, 0.08 s)
   onyx-node: stopping: no new connections; draining in-flight requests and their settles (up to 32s)
   onyx-node: stopped (drained: requests and settles finished, no unknown outcomes)
   health after restart 200;  ONYX_LISTEN=127.0.0.1:8080 onyx-node healthcheck: exit 0
   systemctl stop:  ActiveState=inactive Result=success ExecMainCode=1 ExecMainStatus=0 NRestarts=0
   systemctl kill -s KILL:  active NRestarts=1 pid 238 -> 284
   docker restart (container restart):  active, health 200
   ```
   The process: uid 63899, `CapEff: 0000000000000000`, `NoNewPrivs: 1`, `Seccomp: 2`.
   Gap 6: the docs gave no command to check health or a clean stop under systemd.

7. `systemd-analyze security onyx-node`: **1.2 OK**. Exposed items: `RootDirectory=` (0.1),
   `AF_UNIX` (0.1), `AF_INET/AF_INET6` (0.3), `PrivateNetwork=` (0.5), `PrivateUsers=` (0.2),
   `DeviceAllow=` char-rtc (0.1), `IPAddressDeny=` (0.2); all others pass. The server needs the
   network, so none of these is a fix. `systemd-analyze verify /etc/systemd/system/onyx-node.service`:
   exit 0. On the file in the checkout it exits 1 (`Command /usr/local/bin/onyx-node-btc is not
   executable`), because it reads the `onyx-node.service.d/btc.conf` drop-in next to it (gap 8,
   a note in the docs).

8. `onyx-node-btc` (deployment.md "Packaging" and `btc.conf`'s header): `install … onyx-node-btc`,
   `install -D … btc.conf`, `daemon-reload`, `restart`: `config ok`, `serving check
   btc-entity-reserve-v1: 0 of 0 finalized shard(s) cleared` (the local export has no reserve
   shard: demo-local's step 8 needs the git-ignored tail files), row 200, stop `ExecMainStatus=0`,
   security still 1.2.

9. The `rpc-url` credential (deployment.md: uncomment the lines, remove `ONYX_RPC_URL`):

   ```
   onyx-node: ONYX_RPC_URL_FILE /run/credentials/onyx-node.service/rpc-url must not be readable by group or others (mode 440); chmod 600
   ```
   **L7-SD-1 (server bug, Medium).** systemd hands every `LoadCredential=` file as root-owned
   0440 with an ACL for the service's user (`-r--r-----+ 0 0`, directory `dr-xr-x---+`, a
   private read-only tmpfs). The 0600 rule refused it, and the operator key uses the same
   mechanism: the shipped unit could never start with `self-settle` or facilitator-with-reporter.
   A3 and the soak ran Docker or a bare process, so nothing had started the unit with a key.
   Fix (`crates/onyx-node/src/config.rs`, `check_private`): a regular file (not a symlink)
   directly in `$CREDENTIALS_DIRECTORY` may be group-readable when the file is root:root with
   no group write and nothing for others, and the directory is root:root and closed to others
   (`credential_allowed`, exactly what systemd writes); elsewhere 0600 as before. The first
   version checked only that others could not enter the directory; review found that
   `CREDENTIALS_DIRECTORY=/etc/onyx` (root:onyx 0750) would then admit a root:onyx 0440 key every
   `onyx` member can read, and the variable can be set outside systemd or from an
   `EnvironmentFile=`. Now refused, checked under systemd 259 (`must not be readable by group or
   others (mode 440)`), while a real credential still passes (the start reaches the next
   setting). Tests:
   `config::tests::credential_rule_accepts_only_root_owned_files_in_a_root_owned_closed_directory`
   (non-root file owner or group, non-root directory owner or group, 0750 directory with a
   non-root group, modes) and
   `config::tests::credential_candidates_are_regular_files_directly_in_the_directory` (a symlink
   in the directory pointing outside, a file elsewhere, 0600 anywhere). With
   the fixed binary: `rpc-url` credential → `config ok`, health 200; both credentials together
   (a random key, payments off: the key is still read and checked at start) → `config ok`,
   health 200; a credential holding `nothex` → `operator key file
   /run/credentials/onyx-node.service/operator-key must hold 64 hex digits` (the check still runs).
   Docker's 0640 key is still refused (smoke, below).

### Third pass: the corrected docs on a new machine

A new container, the same export, the fixed unit file, and deployment.md's new block run top to
bottom (`sudoedit` steps done with `sed`): first `systemctl start` exit 0, rows copied and
chowned, `enable --now`, then `config ok`, `listening on 127.0.0.1:8080`, `… serving it`;
`/v1/health` `"status":"ok"`, `"servable":1`; row 200; `onyx-node healthcheck` exit 0; stop
`ExecMainStatus=0`, `Result=success`; security 1.2 OK. No error on the way (sudo's `unable to
resolve host` warning comes from the harness's host network, not the docs).

### Build prerequisites (gap 5)

The docs said `cargo build --release --locked -p onyx-node` with no word on the toolchain.
Checked on a bare image:

```sh
docker run --rm --memory 10g --cpus 4 -v <clone>:/src:ro ubuntu:26.04 bash -c '
  apt-get install -y --no-install-recommends curl ca-certificates gcc libc6-dev
  curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
  . "$HOME/.cargo/env"; cp -r /src /build && cd /build
  cargo build --release --locked -p onyx-node'
```
```
    Finished `release` profile [optimized] target(s) in 4m 40s
rustc 1.98.0 (88d9e12ae 2026-08-18)        # fetched by rust-toolchain.toml
onyx-node: ONYX_ENV is required            # the binary runs
exit 0 in 471 s                            # apt + rustup + build
```

## (b) BuildKit build

From a clean clone of the worktree commit (`git clone <worktree> /tmp/claude-1000/onyx-l7/src`,
`bbfa29a`, no `.env`):

```sh
export DOCKER_CONFIG=/tmp/claude-1000/onyx-l7/dockercfg   # holds cli-plugins/docker-buildx only
docker buildx create --name onyx-l7 --driver docker-container \
  --driver-opt memory=8g --driver-opt memory-swap=8g --driver-opt cpuset-cpus=0-3
DOCKER_BUILDKIT=1 docker buildx build --builder onyx-l7 --load -t onyx-node:bk-l7 .
DOCKER_BUILDKIT=1 docker buildx build --builder onyx-l7 --load --target onyx-node-btc -t onyx-node-btc:bk-l7 .
```

| Image | BuildKit (4 CPUs, 8 GB) | Size | Content (`.Size`) | Classic builder (A3) |
|---|---|---|---|---|
| `onyx-node:bk-l7` | 282 s (cargo 258 s; includes pulling the rust image into the builder) | 49.8 MB | 13,350,952 B | 461-597 s, both binaries |
| `onyx-node-btc:bk-l7` | 196 s (cargo 3 m 12 s) | 53.8 MB | 14,121,209 B | 0 s (cache of the first build) |
| `onyx-node:bk-l7-pin` (pinned syntax line, gap L7-BK-1) | 235 s | 49.8 MB | | |

BuildKit builds only the stages the target needs, so the default build no longer compiles
`onyx-node-btc` (the classic builder did, A3). Both images: user `65532:65532`, the same
`HEALTHCHECK`. The log showed `resolve docker.io/docker/dockerfile:1@sha256:4edf897a…`: the
`# syntax=docker/dockerfile:1` line made BuildKit pull and run a frontend image by a moving tag,
outside the digest pins of OPS-7. **L7-BK-1 (Low)**: the line is now pinned to that digest
(rebuilt with it: `resolve docker.io/docker/dockerfile:1@sha256:4edf897a… done`, exit 0), and
`scripts/check-boundaries.sh` refuses an unpinned `# syntax=` (negative control: digest removed →
`UNPINNED SYNTAX FRONTEND in Dockerfile`, exit 1).

Smoke on the BuildKit images:

```sh
ONYX_DOCKER_TAG=bk-l7 ONYX_DOCKER_BUILD_FLAGS="" ONYX_RESERVE_DIR=<repo>/fixtures-live/tail \
  ONYX_DEMO_DRIVER=target/release/examples/demo-local \
  systemd-run --user --scope -q -p MemoryMax=12G scripts/docker-smoke.sh
```
```
PASS onyx-node:bk-l7: user 65532:65532, HEALTHCHECK [...], 13 MB, no .env, key, log or source
PASS onyx-node-btc ships the 3 committed fixtures, their SHA256SUMS lines and reserve-gate.json
PASS fork run settled round 1 and the reserve round in 80 s; export: both shards, OnyxRevenue 0x9674…3f1f
PASS --check-config exits 0 in both images (no network); a 0640 key is refused: … (mode 640); chmod 600
PASS node/btc: /v1/health 200; indexer servable 2; unpaid btc.entity-flow 402
PASS node: btc.entity-reserve 403 serving_check_unavailable (KNOWN_GATES)
PASS btc: serving check cleared the reserve shard; reserve OKX 402; Binance 403 not_cross_checked
PASS node/btc: docker health healthy after 5 s; state volume 65532:65532 700
PASS node/btc: docker stop -t 75: exit 0 in 0 s
DOCKER SMOKE PASSED (images onyx-node:bk-l7, onyx-node-btc:bk-l7)      # exit 0 in 94 s
```
With `ONYX_DOCKER_TAG` set the script skips its own build and its build-context canary check
(both ran in A3); the fork is an in-memory anvil reading Base Sepolia, nothing broadcast.

## Gaps found and fixed

| # | Gap | Fix |
|---|---|---|
| 1 | systemd steps mixed three working directories (`target/`, `deploy/` from the checkout; `node.service.env`, `deployment.json` bare; only `cas` had `<export>/`) | "run from a checkout"; every export file is `<export>/…` (deployment.md, unit header) |
| 2 | Shipped unit with payments off fails `243/CREDENTIALS`: the key-line table came after the steps | a `sudoedit` step before `daemon-reload` ("payments off: delete both key lines"); the error in a table of first-install errors; unit header says it |
| 3 | systemd block never said to set `ONYX_PUBLIC_URL` (commented out in `node.service.env`): `ONYX_PUBLIC_URL is required`, exit 2 | `sudoedit /etc/onyx/node.env` step |
| 4 | "`systemctl start` creates /var/lib/onyx/{state,cas}" hid that the server runs (0 shards) and that the rows copied after it stay root-owned, so the service cannot write under `cas/` | comment says what happens; `chown -R --reference=…/cas …/cas`; the indexer admits the rows on its next poll, no restart |
| 5 | No build prerequisites | rustup (`rust-toolchain.toml` fetches 1.98.0), `gcc`, `libc6-dev`, or copy a binary built on the same or newer glibc; checked on bare Ubuntu 26.04 |
| 6 | No health or clean-stop check for systemd | `curl -fsS …/v1/health`, `onyx-node healthcheck`, `systemctl show -p ExecMainStatus -p Result`, expected journal lines |
| 7 | The sandbox needs mount namespaces; an unprivileged container fails `226/NAMESPACE` | "Needs a VM or bare-metal host; in a container use the Docker image" |
| 8 | `systemd-analyze verify` on the checkout's unit reads the `btc.conf` drop-in next to it | note in deployment.md |
| L7-SD-1 | Server refused systemd credentials (0440): the unit could not start with a key or `rpc-url` | `config::check_private`; THREATS L7-SD-1, OPS-3 updated |
| L7-BK-1 | `# syntax=` frontend unpinned under BuildKit | pinned by digest; check-boundaries guard; THREATS L7-BK-1 |

## Not verified

- A real VM or bare-metal host: the machine was a container with `CAP_SYS_ADMIN`, AppArmor
  unconfined and the host's cgroup tree. Its systemd, unit sandbox and credentials are the real
  ones; boot order (`network-online.target`) and a real reboot are not.
- Payments under systemd: the export is chain 31337, which refuses `self-settle` and
  `facilitator`, so the key was read and checked but no payment or settle ran under the unit
  (the Docker smoke covers self-settle's start on the fork).
- The `onyx-node-btc` drop-in with a reserve shard (the local export has none) and with a
  read-only `cas` missing the params.
- `DOCKER_BUILDKIT=1 docker build` with the default `docker` driver (no resource caps there);
  `docker buildx build` with a capped `docker-container` builder was used instead. The smoke ran
  on the `bbfa29a` images first; see "Review follow-up" for the pinned-syntax images.

## Moved out of L7

THREATS L2R2-F2 had `TODO L7`: write the registry pin (`registry`, `registryCodeHash`) in the
`--export-node` manifest and assert the attested level in the Docker smoke. It is an export and
smoke change, not an install step, so it moved to the new ROADMAP item L16 rather than holding
L7 open.

## Review follow-up (after merging origin/main, 3233ca3)

- L7-SD-1 tightened: the credential exception now also needs file and directory owned
  root:root and refuses symlinks (see the L7-SD-1 step above); a real systemd 259 credential
  still passes, the review's `/etc/onyx` root:onyx case is refused.
- BuildKit again, same capped builder, from `git archive` of `3233ca3` (pinned `# syntax=`,
  `resolve docker.io/docker/dockerfile:1@sha256:4edf897a… done`): `onyx-node` 357 s / 49.9 MB,
  `onyx-node-btc` 528 s / 53.8 MB. The box was shared (load average about 22), so these times
  are not comparable with the 282 s / 196 s above.
- `docker-smoke.sh` with `ONYX_DOCKER_TAG` on those images: the first run stopped in the fork
  deploy (`MPP HTTP request to https://sepolia.base.org/ failed`, the fork's upstream read); the
  rerun passed: **DOCKER SMOKE PASSED**, 23 PASS lines, 154 s.
