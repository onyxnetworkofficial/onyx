#!/usr/bin/env bash
# Docker smoke test (ROADMAP A3, the Docker half of L7): build the onyx-node
# and onyx-node-btc images from a clean checkout of HEAD, then run them the
# way docs/ops/deployment.md "Run with Docker" says, against a local chain:
#   1. build both images (`git archive HEAD`, the documented commands) and
#      check them: non-root user, HEALTHCHECK, no .env, keys, logs or state in
#      the build context or the image;
#   2. an anvil fork of Base Sepolia (chain 84532, in memory: nothing is
#      broadcast; the fork reads state from ONYX_FORK_RPC) in a container on
#      its own Docker network; scripts/demo-testnet.sh fork
#      (ONYX_DEMO_ANVIL_CONFIG) deploys the contracts and OnyxRevenue,
#      settles round 1 and the reserve round on it and exports the node
#      inputs (--export-node). A fork, not demo-local's chain 31337: onyx-node
#      takes payments on Base Sepolia only, and the 402 needs payments on;
#   3. both images run read-only, as uid 65532, with the documented flags and
#      the operator key mounted 0600 for uid 65532 (self-settle), in the
#      fork's network namespace (the server takes http:// node URLs to
#      localhost only), and are checked: a group-readable key is refused,
#      /v1/health, Docker health "healthy", the indexer admits both finalized
#      shards, unpaid 402, onyx-node refuses the reserve shard (KNOWN_GATES),
#      onyx-node-btc clears it and refuses Binance (403 not_cross_checked), the
#      state volume is written by uid 65532, and `docker stop -t 75` ends each
#      with exit 0. No payment is made, so the servers send no transaction.
#   4. the registry pin (ROADMAP L16, THREATS L2R2-F2): the export's manifest
#      names OnyxRegistry with its code hash and the registry's ledger() is
#      the settlement; both servers start pinned and serve both shards at
#      their attested level (/v1/health shardLevels; a labelled shard from a
#      catalog file is level 5 without the pin); a manifest with a wrong registryCodeHash, or
#      another contract (the settlement, with its own code hash) as the
#      registry, does not start (fail closed).
#   5. behind Caddy in Docker (ROADMAP L18, THREATS TLS-7): a third onyx-node
#      container behind a loopback Caddy (deploy/caddy/Caddyfile on `tls
#      internal`) trusts the Docker network's gateway, where Docker's port
#      proxy connects from: two clients through Caddy get separate request
#      limits, and a client's own X-Forwarded-For changes nothing. The control
#      runs the host value ONYX_TRUSTED_PROXIES=127.0.0.1: the forwarded header
#      is then ignored and both clients share one bucket (the bug TLS-7 names).
# Nothing is pushed anywhere; anvil's dev key is public and holds nothing
# outside the fork.
# Exits non-zero on the first failed check. Slow (two release builds): opt-in
# in check-all with ONYX_DOCKER=1. Report: docs/audit/2026-10-13-docker.md.
#
# Env: ONYX_DOCKER_TAG (use prebuilt images onyx-node:<tag> and
# onyx-node-btc:<tag>; unset: build smoke-<HEAD sha>), ONYX_DOCKER_BUILD_FLAGS
# (default "--memory 8g --memory-swap 8g --cpuset-cpus 0-3", for the classic
# builder; set "" under BuildKit), ONYX_RESERVE_DIR (reserve tail block files,
# default fixtures-live/tail; required), ONYX_LIVE_DIR (as demo-testnet.sh),
# ONYX_FORK_RPC (fork source, default https://sepolia.base.org, read only),
# ONYX_SMOKE_RPC_PORT (18545), ONYX_SMOKE_API_PORT (18080; onyx-node-btc on
# +1), ONYX_SMOKE_DEMO_PORT (18403, the driver's own servers, +0..+2),
# ONYX_SMOKE_ANVIL_IMAGE (ubuntu:24.04; runs the host's anvil binary),
# ONYX_SMOKE_CA_BUNDLE (/etc/ssl/certs/ca-certificates.crt, mounted for it),
# FOUNDRY_BIN (~/.foundry/bin), ONYX_DEMO_DRIVER (a prebuilt driver),
# ONYX_SMOKE_LOCK (the machine-wide lock file, default
# /run/user/UID/onyx-docker-smoke.lock, else /tmp/onyx-docker-smoke-UID.lock), ONYX_SMOKE_LOCK_TIMEOUT (whole
# seconds to wait for it, default 3600; then the run fails), ONYX_CADDY (the Caddy
# binary for step 5, default `caddy` on PATH; required), ONYX_SMOKE_SKIP_PROXY=1 (skip
# step 5, saying so). Step 5 uses ONYX_SMOKE_API_PORT +2 (the third container),
# +3 and +4 (Caddy's HTTP and HTTPS ports, on 127.0.0.1).
#
# Concurrent runs (scripts/smoke-lock.sh): the host ports above are fixed, so every run takes
# the lock file before its first port-binding container and holds it until its containers,
# volumes and network are removed (the EXIT trap, also on a failure or Ctrl-C). The lock is
# taken even when the ports are overridden (simplest; one smoke at a time per lock file). Two
# runs on other ports can overlap by giving each its own ONYX_SMOKE_LOCK.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
RPC_PORT=${ONYX_SMOKE_RPC_PORT:-18545}
API_PORT=${ONYX_SMOKE_API_PORT:-18080}
BTC_PORT=$((API_PORT + 1))
PROXIED_PORT=$((API_PORT + 2))
CADDY_HTTP_PORT=$((API_PORT + 3))
CADDY_TLS_PORT=$((API_PORT + 4))
DEMO_PORT=${ONYX_SMOKE_DEMO_PORT:-18403}
RESERVE_DIR=${ONYX_RESERVE_DIR:-$REPO/fixtures-live/tail}
ANVIL_IMAGE=${ONYX_SMOKE_ANVIL_IMAGE:-ubuntu:24.04}
# The fork reads Base Sepolia over HTTPS; the anvil image has no CA store.
CA_BUNDLE=${ONYX_SMOKE_CA_BUNDLE:-/etc/ssl/certs/ca-certificates.crt}
BUILD_FLAGS=${ONYX_DOCKER_BUILD_FLAGS---memory 8g --memory-swap 8g --cpuset-cpus 0-3}
FORK_SOURCE=${ONYX_FORK_RPC:-https://sepolia.base.org}
RPC=http://127.0.0.1:$RPC_PORT
# shellcheck source=scripts/smoke-lock.sh
. scripts/smoke-lock.sh
LOCK_FILE=${ONYX_SMOKE_LOCK:-$(smoke_lock_default_path)}
LOCK_TIMEOUT=${ONYX_SMOKE_LOCK_TIMEOUT:-3600}
id=onyx-smoke-$$
work=$(mktemp -d)
caddy_pid=

cleanup() {
  # A second Ctrl-C or TERM must not cut this short (its `exit 130` would end the trap): the
  # lock would then be free while containers still hold the ports. Children inherit the ignore.
  trap '' INT TERM HUP
  # Caddy by the PID this run started it with, never by name. `:-`: scripts/smoke-lock-tests.sh
  # runs this function on its own, under set -u, where caddy_pid was never set.
  if [ -n "${caddy_pid:-}" ]; then
    kill "$caddy_pid" 2>/dev/null || true
    wait "$caddy_pid" 2>/dev/null || true
  fi
  docker rm -f "$id-node" "$id-btc" "$id-proxied" "$id-anvil" "$id-badhash" "$id-badreg" \
    >/dev/null 2>&1 || true
  docker volume rm "$id-state-node" "$id-state-btc" "$id-state-proxied" "$id-state-badhash" \
    "$id-state-badreg" >/dev/null 2>&1 || true
  docker network rm "$id-net" >/dev/null 2>&1 || true
  rm -rf "$work" 2>/dev/null || true
  # Last: the next run may bind the ports only once this run's containers are gone.
  smoke_unlock
}
trap cleanup EXIT
trap 'exit 130' INT TERM

pass() { echo "PASS $*"; }
fail() { echo "FAIL $*" >&2; exit 1; }
# cast and forge never run from the repo: Foundry loads a .env from its
# working directory and project root (demo-testnet.sh, M8-L55-F4).
castw() { (cd "$work" && "$FOUNDRY_BIN/cast" "$@"); }
json() { python3 -I -c "import json,sys; v=json.load(sys.stdin); print(eval(sys.argv[1], {'v': v}))" "$1"; }
# GET a URL: prints the status code; the body goes to $work/body.
get() { curl -s -o "$work/body" -w '%{http_code}' --max-time 15 "$1" || true; }
health_status() { docker inspect --format '{{.State.Health.Status}}' "$1" 2>/dev/null || echo none; }

[ -d "$RESERVE_DIR" ] || fail "no reserve tail block files in $RESERVE_DIR (set ONYX_RESERVE_DIR)"
command -v docker >/dev/null || fail "docker not found"
command -v flock >/dev/null || fail "flock not found (util-linux; the machine-wide lock)"
# Checked before the build, so a bad value does not cost two release builds first.
if [ "${ONYX_SMOKE_SKIP_PROXY:-}" != 1 ]; then
  CADDY=${ONYX_CADDY:-$(command -v caddy || true)}
  [ -n "$CADDY" ] && [ -x "$CADDY" ] \
    || fail "no Caddy for step 5 (set ONYX_CADDY, or ONYX_SMOKE_SKIP_PROXY=1 to skip it)"
fi
smoke_lock_timeout_ok "$LOCK_TIMEOUT" || fail "ONYX_SMOKE_LOCK_TIMEOUT '$LOCK_TIMEOUT' is not a whole number of seconds"

# ------------------------------------------------------------------ 1. build
if [ -z "${ONYX_DOCKER_TAG:-}" ]; then
  TAG=smoke-$(git rev-parse --short HEAD)
  [ -z "$(git status --porcelain -- Dockerfile .dockerignore Cargo.toml Cargo.lock crates adapters recipes apps/web/verify fixtures deploy)" ] \
    || echo "note: uncommitted changes are not in the images (built from HEAD)"
  mkdir "$work/ctx"
  git archive HEAD | tar -x -C "$work/ctx"
  # Canaries: none of these may enter the build context (.dockerignore).
  for f in .env .env.local crates/.env node.key crates/onyx-node/operator.key \
    fixtures-live/node/x.hex .onyx-testnet/keys/x state/receipts.jsonl run.log \
    crates/onyx-node/target/x; do
    mkdir -p "$work/ctx/$(dirname "$f")"
    echo "canary, not a secret" >"$work/ctx/$f"
  done
  printf 'FROM alpine:3\nCOPY . /ctx\nCMD ["find", "/ctx", "-type", "f"]\n' |
    docker build -q -t "$id-ctx" -f - "$work/ctx" >/dev/null
  ctx_list=$(docker run --rm --network none "$id-ctx" | sort)
  docker image rm "$id-ctx" >/dev/null 2>&1 || true
  leaked=$(grep -E '\.env|\.key$|fixtures-live|\.onyx-testnet|/state/|\.log$|/target/' <<<"$ctx_list" || true)
  [ -z "$leaked" ] || fail "build context holds: $leaked"
  pass "build context: $(wc -l <<<"$ctx_list") files, no .env, key, log, state or live fixture (canaries refused)"
  rm -rf "$work/ctx/.env" "$work/ctx/.env.local" "$work/ctx/crates/.env" "$work/ctx/node.key" \
    "$work/ctx/crates/onyx-node/operator.key" "$work/ctx/fixtures-live" "$work/ctx/.onyx-testnet" \
    "$work/ctx/state" "$work/ctx/run.log" "$work/ctx/crates/onyx-node/target"
  t0=$(date +%s)
  # shellcheck disable=SC2086
  docker build $BUILD_FLAGS -t "onyx-node:$TAG" "$work/ctx" >"$work/build-node.log" 2>&1 \
    || { tail -30 "$work/build-node.log" >&2; fail "docker build onyx-node"; }
  t1=$(date +%s)
  # shellcheck disable=SC2086
  docker build $BUILD_FLAGS --target onyx-node-btc -t "onyx-node-btc:$TAG" "$work/ctx" \
    >"$work/build-btc.log" 2>&1 || { tail -30 "$work/build-btc.log" >&2; fail "docker build onyx-node-btc"; }
  t2=$(date +%s)
  pass "built onyx-node:$TAG in $((t1 - t0)) s, onyx-node-btc:$TAG in $((t2 - t1)) s"
else
  TAG=$ONYX_DOCKER_TAG
fi
for img in "onyx-node:$TAG" "onyx-node-btc:$TAG"; do
  docker image inspect "$img" >/dev/null || fail "no image $img"
  user=$(docker image inspect --format '{{.Config.User}}' "$img")
  [ "$user" = 65532:65532 ] || fail "$img runs as '$user', want 65532:65532"
  hc=$(docker image inspect --format '{{json .Config.Healthcheck.Test}}' "$img")
  grep -q healthcheck <<<"$hc" || fail "$img has no HEALTHCHECK ($hc)"
  size=$(docker image inspect --format '{{.Size}}' "$img")
  c=$(docker create "$img")
  files=$(docker export "$c" | tar -t)
  docker rm "$c" >/dev/null
  leaked=$(grep -E '(^|/)\.env|\.key$|fixtures-live|\.onyx-testnet|\.log$|/src/.|\.rs$|Cargo\.toml' <<<"$files" || true)
  [ -z "$leaked" ] || fail "$img holds: $leaked"
  pass "$img: user 65532:65532, HEALTHCHECK $hc, $((size / 1000000)) MB, no .env, key, log or source"
done
shipped=$(docker create "onyx-node-btc:$TAG")
docker export "$shipped" | tar -t | grep '^usr/share/onyx/' | grep -v '/$' >"$work/shipped"
docker rm "$shipped" >/dev/null
[ "$(wc -l <"$work/shipped")" = 6 ] || { cat "$work/shipped" >&2; fail "onyx-node-btc ships $(wc -l <"$work/shipped") files under /usr/share/onyx, want 6"; }
pass "onyx-node-btc ships the 4 committed fixtures, their SHA256SUMS lines and reserve-gate.json"
# The systemd drop-in runs the same binary the image does.
grep -q '^ExecStart=/usr/local/bin/onyx-node-btc$' deploy/systemd/onyx-node.service.d/btc.conf \
  && grep -q '^ExecStartPre=/usr/local/bin/onyx-node-btc --check-config$' deploy/systemd/onyx-node.service.d/btc.conf \
  && [ "$(docker image inspect --format '{{json .Config.Entrypoint}}' "onyx-node-btc:$TAG")" = '["/usr/local/bin/onyx-node-btc"]' ] \
  || fail "deploy/systemd/onyx-node.service.d/btc.conf and the image's entrypoint differ"
pass "systemd drop-in btc.conf and the image both run /usr/local/bin/onyx-node-btc"

# ------------------------------------------------------------ the lock
# Taken here, after the builds and image checks: they publish no port (the build-context
# container runs with --network none; the image checks only create containers, never start
# them), and every name they use carries this run's PID, so two runs can build at once and a
# second run does not sit idle through its own builds. From here on the run binds the fixed
# host ports, so only one run per lock file goes past this point. The lock is held until the
# EXIT trap has removed the containers, volumes and network (cleanup ends with smoke_unlock).
t=$(date +%s)
smoke_lock "$LOCK_FILE" "$LOCK_TIMEOUT" \
  "pid $$ (containers $id-*), worktree $(basename "$REPO"), since $(date -u +%FT%TZ)" \
  || fail "no machine-wide smoke lock ($LOCK_FILE; ONYX_SMOKE_LOCK_TIMEOUT=$LOCK_TIMEOUT s)"
pass "machine-wide smoke lock $LOCK_FILE taken after $(($(date +%s) - t)) s"
# A run that does not take the lock (a checkout from before it; seen on the first run of this
# change) or one killed with SIGKILL can still hold the ports: wait for them within the same
# timeout, naming whoever has them, instead of failing with Docker's exit 125.
smoke_wait_ports "$((t + LOCK_TIMEOUT))" "$RPC_PORT" "$API_PORT" "$BTC_PORT" \
  "$PROXIED_PORT" "$CADDY_HTTP_PORT" "$CADDY_TLS_PORT" \
  "$DEMO_PORT" $((DEMO_PORT + 1)) $((DEMO_PORT + 2)) \
  || fail "ports still in use after ONYX_SMOKE_LOCK_TIMEOUT=$LOCK_TIMEOUT s"

# ------------------------------------------------------- 2. chain and inputs
docker network create "$id-net" >/dev/null
mkdir -m 0700 "$work/anvil"
# The fork and both servers share one network namespace (the anvil
# container's): onyx-node takes an http:// node URL to localhost only.
docker run -d --name "$id-anvil" --network "$id-net" --user "$(id -u):$(id -g)" \
  --read-only --tmpfs /tmp -e HOME=/tmp -e FORK_SOURCE="$FORK_SOURCE" \
  -v "$FOUNDRY_BIN/anvil:/usr/local/bin/anvil:ro" -v "$work/anvil:/anvil" \
  -v "$CA_BUNDLE:/etc/ssl/certs/ca-certificates.crt:ro" \
  -p "127.0.0.1:$RPC_PORT:$RPC_PORT" -p "127.0.0.1:$API_PORT:8080" -p "127.0.0.1:$BTC_PORT:8081" \
  -p "127.0.0.1:$PROXIED_PORT:8082" \
  "$ANVIL_IMAGE" sh -c 'exec anvil --fork-url "$FORK_SOURCE" --host 0.0.0.0 --port '"$RPC_PORT"' \
  --accounts 1 --silent --config-out /anvil/anvil.json' >/dev/null
for _ in $(seq 150); do
  [ -s "$work/anvil/anvil.json" ] && [ "$(castw chain-id --rpc-url "$RPC" 2>/dev/null)" = 84532 ] && break
  sleep 0.2
done
[ "$(castw chain-id --rpc-url "$RPC" 2>/dev/null)" = 84532 ] \
  || { docker logs "$id-anvil" 2>&1 | tail -5 >&2; fail "the anvil fork did not start"; }
pass "anvil fork of Base Sepolia (chain 84532) in container $id-anvil on network $id-net"

echo "Deploying and settling round 1 and the reserve round on the fork (scripts/demo-testnet.sh fork)..."
t=$(date +%s)
ONYX_DEMO_ANVIL_CONFIG=$work/anvil/anvil.json ONYX_DEMO_RPC_PORT=$RPC_PORT \
  ONYX_DEMO_KEYS=$work/keys ONYX_DEMO_API_PORT=$DEMO_PORT ONYX_RESERVE_DIR=$RESERVE_DIR \
  smoke_without_lock_fd scripts/demo-testnet.sh fork --export-node "$work/export" >"$work/demo.log" 2>&1 \
  || { tail -40 "$work/demo.log" >&2; fail "scripts/demo-testnet.sh fork"; }
grep -q '^8\. reserve shard' "$work/demo.log" || { tail -20 "$work/demo.log" >&2; fail "the fork run had no reserve step"; }
held=$(json 'len(v["held"])' <"$work/export/catalog.json")
[ "$held" = 2 ] || fail "exported catalog holds $held shards, want 2"
revenue=$(json 'v["revenue"]' <"$work/export/deployment.json")
pass "fork run settled round 1 and the reserve round in $(($(date +%s) - t)) s; export: both shards, OnyxRevenue $revenue"

# L16: the export pins the registry, and the chain holds an attested level
# for the market's shard and the reserve shard (the driver attested both
# before their rounds opened; both pin a label set, whose level onyx-node
# cannot know from the catalog file: without the pin it serves them at 5).
lower() { tr '[:upper:]' '[:lower:]'; }
registry=$(json 'v.get("registry") or ""' <"$work/export/deployment.json" | lower)
registry_hash=$(json 'v.get("registryCodeHash") or ""' <"$work/export/deployment.json" | lower)
settlement=$(json 'v["settlement"]' <"$work/export/deployment.json" | lower)
[ -n "$registry" ] && [ -n "$registry_hash" ] \
  || fail "the exported manifest does not pin the registry (registry, registryCodeHash)"
[ "$(castw codehash "$registry" --rpc-url "$RPC" | lower)" = "$registry_hash" ] \
  || fail "registryCodeHash is not the code hash of $registry"
[ "$(castw call "$registry" 'ledger()(address)' --rpc-url "$RPC" | lower)" = "$settlement" ] \
  || fail "the pinned registry's ledger() is not the settlement $settlement"
m_round=$(json '[m for m in v["markets"] if m["id"] == "okx-net-flow-demo"][0]["round"]' <"$work/export/catalog.json")
m_shard=$(json '[m for m in v["markets"] if m["id"] == "okx-net-flow-demo"][0]["shard"]' <"$work/export/catalog.json")
attested=$(castw call "$registry" 'levelOf(uint64,bytes32)(uint8)' "$m_round" "$m_shard" --rpc-url "$RPC")
case "$attested" in
  1 | 2 | 3 | 4) ;;
  *) fail "the registry's level for the market shard (round $m_round) is '$attested', want an attested level 1-4" ;;
esac
grep -q "^8\. reserve shard .*admitted at level $attested" "$work/demo.log" \
  || fail "the reserve shard was not attested at level $attested (demo log step 8)"
pass "export pins registry $registry (code hash matches the chain, ledger() is the settlement); market shard attested at level $attested"

# /etc/onyx as deployment.md lays it out: manifest and catalog 0644, the row
# store readable by uid 65532, node.env 0600 (read by the docker CLI only).
etc=$work/etc
mkdir "$etc"
cp "$work/export/deployment.json" "$etc/"
# The shards' data is days to a year old, past the exported 24 h fresh window
# (rows are free after it, priced from the data's own time): stretch it so
# an unpaid request must pay (402).
python3 -I - "$work/export/catalog.json" "$etc/catalog.json" <<'EOF'
import json, sys
c = json.load(open(sys.argv[1]))
for p in c["pricing"].values():
    p["freshWindowSecs"] = 10 * 365 * 86400
open(sys.argv[2], "w").write(json.dumps(c, indent=2) + "\n")
EOF
cp -r "$work/export/cas" "$etc/cas"
# The image's gate config, with the start height of this one-shard chain.
python3 -I - "$REPO/deploy/reserve-gate.json" "$etc/reserve-gate.json" <<'EOF'
import json, sys
g = json.load(open(sys.argv[1]))
g.update(fixtures="/usr/share/onyx/fixtures", startHeight=971191)
open(sys.argv[2], "w").write(json.dumps(g, indent=2) + "\n")
EOF
# The export says ONYX_RIGHTS=fallback for a testnet node (review L14-R1); this smoke test sells
# the reserve row of the test label set on a fork, so it runs cleared.
grep -q '^ONYX_RIGHTS=fallback$' "$work/export/node.service.env" || fail "the fork export does not default to ONYX_RIGHTS=fallback"
grep -v '^# ONYX_PUBLIC_URL' "$work/export/node.service.env" | grep -v '^ONYX_RIGHTS=' >"$etc/node.env"
cat >>"$etc/node.env" <<EOF
ONYX_RIGHTS=cleared
ONYX_PUBLIC_URL=http://127.0.0.1:$API_PORT
ONYX_INDEX_EVERY_SECS=2
EOF
grep -q '^ONYX_PAYMENTS=self-settle$' "$etc/node.env" || fail "the fork export is not self-settle"
chmod 0600 "$etc/node.env"
chmod 0755 "$etc"
chmod 0644 "$etc/deployment.json" "$etc/catalog.json" "$etc/reserve-gate.json"
chmod -R a+rX "$etc/cas"
# The operator key (self-settle): owned by uid 65532, mode 0600, in the
# read-only mount (deployment.md). Here the run's own operator key (a fresh
# fork-only key the driver made OnyxRevenue's reporter; onyx-node refuses any
# other since L15; THREATS L16-R7); a copy that group can read must be refused. Written by a
# root helper (chown).
[ -s "$work/keys/operator.key" ] || fail "the fork run left no operator key in $work/keys"
tr -d '\n' <"$work/keys/operator.key" |
  docker run --rm -i --network none -v "$etc:/e" alpine:3 sh -c \
  'cat >/e/operator.key && cp /e/operator.key /e/operator-open.key &&
   chown 65532:65532 /e/operator.key /e/operator-open.key &&
   chmod 0600 /e/operator.key && chmod 0640 /e/operator-open.key'

# The documented flags (deployment.md "Run with Docker"), with the network
# namespace instead of -p. $1 name, $2 image, $3 listen port, rest: extra -e.
# Inside the container the server must listen on 0.0.0.0, which onyx-node
# refuses without ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1 (M7-7): here the namespace is
# the anvil container's, whose ports are published on 127.0.0.1 only.
run_node() {
  local name=$1 img=$2 port=$3
  shift 3
  docker volume create "$id-state-$name" >/dev/null 2>&1 || true
  docker run -d --name "$id-$name" --read-only --stop-timeout 75 \
    --memory 2g --cap-drop ALL --security-opt no-new-privileges \
    --network "container:$id-anvil" \
    --env-file "$etc/node.env" \
    -e "ONYX_LISTEN=0.0.0.0:$port" -e ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1 \
    -e ONYX_DEPLOYMENT=/etc/onyx/deployment.json -e ONYX_CATALOG=/etc/onyx/catalog.json \
    -e ONYX_CAS_DIR=/etc/onyx/cas -e ONYX_STATE_DIR=/var/lib/onyx/state \
    -e ONYX_OPERATOR_KEY_FILE=/etc/onyx/operator.key \
    "$@" \
    -v "$etc:/etc/onyx:ro" -v "$id-state-$name:/var/lib/onyx/state" \
    "$img" >/dev/null
}

# L16 fail-closed controls: onyx-node started from a manifest whose registry
# pin does not hold. $1 name, $2 a python statement editing the manifest `v`.
# Started in the background (the start tries each node read 8 times, about
# 35 s) and judged after the servers' checks.
bad_start() {
  local name=$1
  python3 -I -c 'import json, sys
v = json.load(open(sys.argv[1]))
exec(sys.argv[3])
open(sys.argv[2], "w").write(json.dumps(v, indent=2) + "\n")' \
    "$etc/deployment.json" "$etc/deployment-$name.json" "$2"
  chmod 0644 "$etc/deployment-$name.json"
  docker volume create "$id-state-$name" >/dev/null 2>&1 || true
  docker run -d --name "$id-$name" --read-only --memory 1g --cap-drop ALL \
    --security-opt no-new-privileges --network "container:$id-anvil" \
    --env-file "$etc/node.env" \
    -e ONYX_LISTEN=0.0.0.0:8090 -e ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1 \
    -e "ONYX_DEPLOYMENT=/etc/onyx/deployment-$name.json" -e ONYX_CATALOG=/etc/onyx/catalog.json \
    -e ONYX_CAS_DIR=/etc/onyx/cas -e ONYX_STATE_DIR=/var/lib/onyx/state \
    -e ONYX_OPERATOR_KEY_FILE=/etc/onyx/operator.key \
    -v "$etc:/etc/onyx:ro" -v "$id-state-$name:/var/lib/onyx/state" \
    "onyx-node:$TAG" >/dev/null
}
# $1 name, $2 text the refusal must name.
bad_start_refused() {
  local name=$1 want=$2 code
  code=$(timeout 150 docker wait "$id-$name" 2>/dev/null || echo running)
  docker logs "$id-$name" >"$work/$name.log" 2>&1 || true
  [ "$code" != running ] || fail "$name: onyx-node still running after 150 s with a broken registry pin"
  [ "$code" != 0 ] || fail "$name: onyx-node exited 0 with a broken registry pin"
  grep -q "$want" "$work/$name.log" || { tail -5 "$work/$name.log" >&2; fail "$name: the refusal does not name '$want'"; }
  if grep -q 'pinned (code hash' "$work/$name.log"; then
    fail "$name: onyx-node pinned a broken registry"
  fi
  pass "$name: onyx-node refused to start (exit $code): $(grep -o 'registry 0x[0-9a-f]*: [^(]*' "$work/$name.log" | tail -1)"
}

# ------------------------------------------------------------ 3. the servers
check_config() {
  local img=$1 key=$2
  docker run --rm --read-only --network none --env-file "$etc/node.env" \
    -e ONYX_DEPLOYMENT=/etc/onyx/deployment.json -e ONYX_CATALOG=/etc/onyx/catalog.json \
    -e ONYX_CAS_DIR=/etc/onyx/cas -e ONYX_STATE_DIR=/var/lib/onyx/state \
    -e ONYX_OPERATOR_KEY_FILE="/etc/onyx/$key" -e ONYX_RESERVE_GATE=/etc/onyx/reserve-gate.json \
    -v "$etc:/etc/onyx:ro" "$img" --check-config >"$work/check.log" 2>&1
}
for img in "onyx-node:$TAG" "onyx-node-btc:$TAG"; do
  check_config "$img" operator.key || { cat "$work/check.log" >&2; fail "$img --check-config"; }
  if check_config "$img" operator-open.key; then
    fail "$img accepted an operator key that group can read"
  fi
  grep -qi 'key' "$work/check.log" || { cat "$work/check.log" >&2; fail "$img: unexpected refusal"; }
done
pass "--check-config exits 0 in both images (no network); a 0640 key is refused: $(tail -1 "$work/check.log")"
# M7-7: the image's own ONYX_LISTEN (0.0.0.0:8080) without the opt-in is refused.
for img in "onyx-node:$TAG" "onyx-node-btc:$TAG"; do
  if docker run --rm --read-only --network none --env-file "$etc/node.env" \
    -e ONYX_LISTEN=0.0.0.0:8080 \
    -e ONYX_DEPLOYMENT=/etc/onyx/deployment.json -e ONYX_CATALOG=/etc/onyx/catalog.json \
    -e ONYX_CAS_DIR=/etc/onyx/cas -e ONYX_STATE_DIR=/var/lib/onyx/state \
    -e ONYX_OPERATOR_KEY_FILE=/etc/onyx/operator.key -e ONYX_RESERVE_GATE=/etc/onyx/reserve-gate.json \
    -v "$etc:/etc/onyx:ro" "$img" --check-config >"$work/check.log" 2>&1; then
    fail "$img accepted ONYX_LISTEN=0.0.0.0 without ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1"
  fi
  grep -q 'ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1' "$work/check.log" || { cat "$work/check.log" >&2; fail "$img: unexpected refusal"; }
done
pass "a 0.0.0.0 listen address without ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1 is refused in both images (M7-7)"

run_node node "onyx-node:$TAG" 8080
run_node btc "onyx-node-btc:$TAG" 8081 -e "ONYX_PUBLIC_URL=http://127.0.0.1:$BTC_PORT" \
  -e ONYX_RESERVE_GATE=/etc/onyx/reserve-gate.json
started=$(date +%s)
logs() { docker logs "$id-$1" 2>&1 | tail -20 >&2; }
# A wrong code hash; another contract (the settlement) as the registry, with
# its own code hash, so only the ledger() binding can refuse it.
bad_start badhash 'v["registryCodeHash"] = "0x" + "00" * 32'
bad_start badreg 'v["registry"] = v["settlement"]; v["registryCodeHash"] = v["settlementCodeHash"]'


for n in node btc; do
  port=$([ $n = node ] && echo "$API_PORT" || echo "$BTC_PORT")
  code=000
  for _ in $(seq 60); do
    code=$(get "http://127.0.0.1:$port/v1/health")
    [ "$code" = 200 ] && break
    [ "$(docker inspect --format '{{.State.Running}}' "$id-$n")" = true ] || break
    sleep 1
  done
  [ "$code" = 200 ] || { logs $n; fail "$n: /v1/health answered $code"; }
  pass "$n: /v1/health 200"
done

# The chain indexer admits the finalized shards (ADR-0033).
for n in node btc; do
  port=$([ $n = node ] && echo "$API_PORT" || echo "$BTC_PORT")
  servable=0
  for _ in $(seq 60); do
    get "http://127.0.0.1:$port/v1/health" >/dev/null
    servable=$(json 'v.get("indexer", {}).get("servable", 0)' <"$work/body")
    [ "$servable" -ge 2 ] && break
    sleep 1
  done
  [ "$servable" -ge 2 ] || { cat "$work/body" >&2; logs $n; fail "$n: indexer servable $servable, want 2"; }
  pass "$n: indexer admitted both finalized shards (servable $servable)"
done

# Unpaid: 402 with a PAYMENT-REQUIRED offer.
for n in node btc; do
  port=$([ $n = node ] && echo "$API_PORT" || echo "$BTC_PORT")
  code=$(get "http://127.0.0.1:$port/v1/metrics/btc.entity-flow?subject=entity:okx")
  [ "$code" = 402 ] || { cat "$work/body" >&2; fail "$n: unpaid btc.entity-flow answered $code, want 402"; }
  pass "$n: unpaid btc.entity-flow (entity:okx) 402"
done

# L16: both servers started with the registry pinned, and both shards keep
# their attested level (not the fail-closed 5 of a label set the catalog
# file cannot rate, nor anything stronger than the chain's).
for n in node btc; do
  port=$([ $n = node ] && echo "$API_PORT" || echo "$BTC_PORT")
  # Read the whole log first: under pipefail, `docker logs | grep -q` fails when grep exits at
  # the first match and docker logs is killed by SIGPIPE (seen in review C20-R2).
  node_log=$(docker logs "$id-$n" 2>&1)
  grep -qF "registry $registry pinned (code hash and ledger checked)" <<<"$node_log" \
    || { logs $n; fail "$n: started without the registry pin"; }
  get "http://127.0.0.1:$port/v1/health" >/dev/null
  levels=$(json '" ".join(f"{k}:{n}" for k, n in sorted((v.get("shardLevels") or {}).items()))' <"$work/body")
  [ "$levels" = "$attested:2" ] \
    || { cat "$work/body" >&2; fail "$n: shardLevels $levels, want both shards at the attested level $attested"; }
  pass "$n: registry pinned at start; both shards served at their attested level $attested (shardLevels $levels)"
done

# Plain onyx-node refuses the reserve shard whatever its catalog says.
code=$(get "http://127.0.0.1:$API_PORT/v1/metrics/btc.entity-reserve?subject=entity:okx")
{ [ "$code" = 403 ] && grep -q serving_check_unavailable "$work/body"; } \
  || { cat "$work/body" >&2; fail "node: reserve answered $code, want 403 serving_check_unavailable"; }
pass "node: btc.entity-reserve 403 serving_check_unavailable (KNOWN_GATES)"

# onyx-node-btc clears it, sells OKX, refuses Binance.
cleared=0
for _ in $(seq 60); do
  get "http://127.0.0.1:$BTC_PORT/v1/health" >/dev/null
  cleared=$(json 'sum(r.get("cleared", 0) for r in v.get("servingChecks", []))' <"$work/body")
  [ "$cleared" -ge 1 ] && break
  sleep 1
done
[ "$cleared" -ge 1 ] || { cat "$work/body" >&2; logs btc; fail "btc: serving check cleared $cleared shards"; }
pass "btc: serving check btc-entity-reserve-v1 cleared the reserve shard"
code=$(get "http://127.0.0.1:$BTC_PORT/v1/metrics/btc.entity-reserve?subject=entity:okx")
[ "$code" = 402 ] || { cat "$work/body" >&2; fail "btc: unpaid reserve OKX answered $code, want 402"; }
pass "btc: unpaid btc.entity-reserve (entity:okx) 402"
code=$(get "http://127.0.0.1:$BTC_PORT/v1/metrics/btc.entity-reserve?subject=entity:binance")
{ [ "$code" = 403 ] && grep -q not_cross_checked "$work/body"; } \
  || { cat "$work/body" >&2; fail "btc: Binance answered $code, want 403 not_cross_checked"; }
pass "btc: btc.entity-reserve (entity:binance) 403 not_cross_checked"

# Docker's own health check (the image's HEALTHCHECK, every 30 s).
for n in node btc; do
  st=none
  for _ in $(seq 100); do
    st=$(health_status "$id-$n")
    [ "$st" = healthy ] && break
    [ "$st" = unhealthy ] && break
    sleep 1
  done
  [ "$st" = healthy ] || { logs $n; fail "$n: docker health $st"; }
  pass "$n: docker health healthy after $(($(date +%s) - started)) s"
done

# The state volume is written by the image's user.
own=$(docker run --rm --network none -v "$id-state-node:/s:ro" alpine:3 stat -c '%u:%g %a' /s)
[ "$own" = "65532:65532 700" ] || fail "state volume is $own, want 65532:65532 700"
docker run --rm --network none -v "$id-state-node:/s:ro" alpine:3 test -s /s/indexer.json \
  || fail "no indexer.json in the state volume"
pass "state volume: 65532:65532 mode 700, indexer.json written"

# L16: the broken pins never served.
bad_start_refused badhash registryCodeHash
bad_start_refused badreg 'not bound to the manifest.s settlement\|registry ledger() failed'

# A clean stop within the 75 s the docs give it.
for n in node btc; do
  t=$(date +%s)
  docker stop -t 75 "$id-$n" >/dev/null
  code=$(docker inspect --format '{{.State.ExitCode}}' "$id-$n")
  [ "$code" = 0 ] || { logs $n; fail "$n: exit code $code after docker stop"; }
  pass "$n: docker stop -t 75: exit 0 in $(($(date +%s) - t)) s"
done

# ------------------------------------------- 5. behind Caddy in Docker (TLS-7)
# deployment.md "TLS in front", "In Docker": the node is published on 127.0.0.1 and Caddy on the
# host proxies to it, but Docker's port proxy connects to the container from the network's
# gateway, not from 127.0.0.1. So the node trusts the gateway; each client is then keyed by the
# address Caddy forwards. ONYX_READS_PER_MINUTE=5 makes a limit quick to reach. This script's
# readiness probes through Caddy come from 127.0.0.4, the clients are 127.0.0.2 (A) and 127.0.0.3 (B).
if [ "${ONYX_SMOKE_SKIP_PROXY:-}" = 1 ]; then
  echo "SKIP step 5 (behind Caddy in Docker, TLS-7): ONYX_SMOKE_SKIP_PROXY=1"
else
  gw=$(docker network inspect "$id-net" --format '{{(index .IPAM.Config 0).Gateway}}')
  [[ $gw =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "network $id-net has no IPv4 gateway ('$gw')"
  mkdir -m 0700 "$work/caddy-data" "$work/caddy-config"
  # The shipped file; only the environment differs from production (local CA, loopback ports).
  XDG_DATA_HOME=$work/caddy-data XDG_CONFIG_HOME=$work/caddy-config \
    ONYX_CADDY_DOMAIN=localhost ONYX_CADDY_TLS=internal \
    ONYX_CADDY_UPSTREAM=127.0.0.1:$PROXIED_PORT ONYX_CADDY_BIND=127.0.0.1 \
    ONYX_CADDY_HTTP_PORT=$CADDY_HTTP_PORT ONYX_CADDY_HTTPS_PORT=$CADDY_TLS_PORT \
    "$CADDY" run --config "$REPO/deploy/caddy/Caddyfile" --adapter caddyfile \
    >"$work/caddy.log" 2>&1 &
  caddy_pid=$!
  CA=$work/caddy-data/caddy/pki/authorities/local/root.crt
  # $1 the client's address, rest: curl options. Prints the status through Caddy; body in $work/body.
  via() {
    local from=$1
    shift
    curl -s -o "$work/body" -w '%{http_code}' --max-time 15 --cacert "$CA" \
      --resolve "localhost:$CADDY_TLS_PORT:127.0.0.1" --interface "$from" "$@" \
      "https://localhost:$CADDY_TLS_PORT/v1/health" || true
  }
  # $1 the client's address. Requests until the first 429 (at most 12); prints the 200s before it.
  until_limited() {
    local n=0 code
    for _ in $(seq 12); do
      code=$(via "$1")
      [ "$code" = 429 ] && { echo "$n"; return 0; }
      [ "$code" = 200 ] || { echo "status $code"; return 0; }
      n=$((n + 1))
    done
    echo "no 429 after $n"
  }
  # $1 ONYX_TRUSTED_PROXIES: (re)starts the third container and waits until Caddy reaches it.
  proxied() {
    docker rm -f "$id-proxied" >/dev/null 2>&1 || true
    run_node proxied "onyx-node:$TAG" 8082 -e "ONYX_TRUSTED_PROXIES=$1" \
      -e ONYX_READS_PER_MINUTE=5 -e "ONYX_PUBLIC_URL=https://localhost:$CADDY_TLS_PORT"
    local code=000
    for _ in $(seq 60); do
      kill -0 "$caddy_pid" 2>/dev/null || { cat "$work/caddy.log" >&2; fail "Caddy exited"; }
      [ -s "$CA" ] && code=$(via 127.0.0.4)
      [ "$code" = 200 ] && break
      [ "$(docker inspect --format '{{.State.Running}}' "$id-proxied")" = true ] || break
      sleep 1
    done
    [ "$code" = 200 ] || { logs proxied; tail -5 "$work/caddy.log" >&2; fail "proxied: /v1/health through Caddy answered $code"; }
  }

  proxied "$gw/32"
  a=$(until_limited 127.0.0.2)
  [ "$a" = 5 ] || { cat "$work/body" >&2; fail "trusted gateway: client A got '$a' answers before its 429, want its own 5"; }
  grep -q '"rate_limited"' "$work/body" || { cat "$work/body" >&2; fail "client A's 429 is not rate_limited"; }
  code=$(via 127.0.0.2 -H 'X-Forwarded-For: 203.0.113.9')
  [ "$code" = 429 ] || fail "client A with its own X-Forwarded-For answered $code, want 429 (its key cannot be spoofed through Caddy)"
  code=$(via 127.0.0.3)
  [ "$code" = 200 ] || { cat "$work/body" >&2; fail "trusted gateway: client B answered $code after A's limit, want 200"; }
  pass "ONYX_TRUSTED_PROXIES=$gw/32 (network $id-net's gateway) behind Caddy: client A 5 x 200 then 429 (also with its own X-Forwarded-For), client B 200: separate limits"

  # The control: the host value. Every request reaches the node from the gateway, so
  # the forwarded header is ignored and A, B and the readiness probes share one bucket.
  proxied 127.0.0.1
  a=$(until_limited 127.0.0.2)
  [[ $a =~ ^[0-5]$ ]] || { cat "$work/body" >&2; fail "control: client A got '$a' before its 429"; }
  code=$(via 127.0.0.3)
  [ "$code" = 429 ] || { cat "$work/body" >&2; fail "control (ONYX_TRUSTED_PROXIES=127.0.0.1): client B answered $code after A's limit, want 429 (one shared bucket)"; }
  pass "control ONYX_TRUSTED_PROXIES=127.0.0.1: client A $a x 200 then 429, client B 429 at once: one shared bucket (the TLS-7 bug)"

  t=$(date +%s)
  docker stop -t 75 "$id-proxied" >/dev/null
  code=$(docker inspect --format '{{.State.ExitCode}}' "$id-proxied")
  [ "$code" = 0 ] || { logs proxied; fail "proxied: exit code $code after docker stop"; }
  kill "$caddy_pid" 2>/dev/null || true
  wait "$caddy_pid" 2>/dev/null || true
  caddy_pid=
  pass "proxied: docker stop -t 75: exit 0 in $(($(date +%s) - t)) s; Caddy stopped"
fi

echo "DOCKER SMOKE PASSED (images onyx-node:$TAG, onyx-node-btc:$TAG)"
