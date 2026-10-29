#!/usr/bin/env bash
# M8 on Base Sepolia (docs/ops/demo-testnet.md): deploy the settlement and
# revenue contracts, fund per-actor keys, run one real round, serve it with
# onyx-api settling x402 payments itself into OnyxRevenue (ADR-0026), and
# check a row as a client; then sell a reserve row (btc.entity-reserve,
# ADR-0034) from a second round behind its serving check.
#
#   scripts/demo-testnet.sh fork [--dispute]     rehearsal on an anvil fork of
#                                                Base Sepolia (nothing broadcast)
#   scripts/demo-testnet.sh fork --level2        also the level-2 Treasury round,
#                                                its fact's revenue book opened by
#                                                the reporter before it is sold
#                                                (THREATS L2API-4; fork only)
#   scripts/demo-testnet.sh testnet --plan       budget and balance check only
#                                                (--dry-run is the same)
#   scripts/demo-testnet.sh testnet --yes [--dispute] [--sweep]
#                                                the real run (Base Sepolia)
#
# Flags: --plan|--dry-run, --dispute, --level2 (fork only, ROADMAP B1), --serve, --export-node DIR (onyx-node
# inputs: manifest, catalog, rows, node.env), --sweep (return what the actor
# keys hold to the deployer afterwards), --yes (required for a real testnet
# run). Any other argument is refused, so a typo never starts a run
# (M8-L58-F1).
#
# Testnet only: the driver refuses any chain but Base Sepolia (84532), or an
# anvil fork of it. The deployer key is read from .env (BASE_SEPOLIA_PRIVATE_KEY)
# into the environment of forge and the driver only; it is never printed or put
# on a command line. Actor keys live in .onyx-testnet/keys (gitignored, 0600);
# run logs with BaseScan links in .onyx-testnet/runs. cast and forge run in a
# temp dir outside the repo, where Foundry finds no .env (M8-L55-F4).
#
# Env: ONYX_FORK_RPC (fork source, default https://sepolia.base.org),
# ONYX_DEMO_RPC_PORT (8546), ONYX_DEMO_API_PORT (8404; the reserve API uses
# port + 2), ONYX_LIVE_DIR, ONYX_RESERVE_DIR (reserve tail block files
# 971,192-971,199, default fixtures-live/tail; required, ADR-0034),
# FOUNDRY_BIN (~/.foundry/bin), ONYX_DEMO_DRIVER (a prebuilt driver),
# ONYX_DEMO_KEYS (fork only: the actor keys' directory, kept after the run;
# default a temp dir), --soak-rounds/--soak-every (fork, with --serve and
# --export-node: settle more rounds afterwards; scripts/soak.sh),
# ONYX_DEMO_ANVIL_CONFIG (fork only: an anvil fork already listening on
# ONYX_DEMO_RPC_PORT and the `--config-out` file it wrote; used instead of
# starting one and left running, so the fork outlives the script;
# scripts/docker-smoke.sh runs it in a container).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
mode=${1:-}
usage="usage: $0 fork|testnet [--plan|--dry-run] [--dispute] [--level2 (fork)] [--serve] [--sweep] [--export-node DIR] [--soak-rounds N --soak-every SECS] [--yes]"
case "$mode" in
  fork | testnet) shift ;;
  *) echo "$usage" >&2; exit 2 ;;
esac
plan=""
yes=""
flags=()
while [ $# -gt 0 ]; do
  a=$1
  shift
  case "$a" in
    --plan | --dry-run) plan=1 ;;
    --yes) yes=1 ;;
    --dispute | --serve | --sweep) flags+=("$a") ;;
    --level2)
      # ADR-0027 amendment 8 item 8: level 2 stays off Base Sepolia until
      # ROADMAP B2-B6; refused here, before the plan or the deploy sends anything.
      [ "$mode" = fork ] || { echo "--level2 is for fork mode: level 2 on Base Sepolia waits for ROADMAP B2-B6" >&2; exit 2; }
      flags+=("$a")
      ;;
    --export-node)
      [ $# -gt 0 ] || { echo "--export-node needs a directory" >&2; exit 2; }
      flags+=(--export-node "$1")
      shift
      ;;
    --soak-rounds | --soak-every)
      [ $# -gt 0 ] || { echo "$a needs a number" >&2; exit 2; }
      flags+=("$a" "$1")
      shift
      ;;
    *) echo "unknown argument: $a" >&2; echo "$usage" >&2; exit 2 ;;
  esac
done
if [ "$mode" = testnet ] && [ -z "$plan" ] && [ -z "$yes" ]; then
  echo "a testnet run sends Base Sepolia transactions from the deployer:" >&2
  echo "run '$0 testnet --plan' first, then add --yes" >&2
  exit 2
fi
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
FORK_SOURCE=${ONYX_FORK_RPC:-https://sepolia.base.org}
RPC_PORT=${ONYX_DEMO_RPC_PORT:-8546}
API_PORT=${ONYX_DEMO_API_PORT:-8404}
LIVE_DIR=${ONYX_LIVE_DIR:-$REPO/fixtures-live/node}
RESERVE_DIR=${ONYX_RESERVE_DIR:-$REPO/fixtures-live/tail}
# The reserve round (step 5b) needs these blocks; refuse here, before the plan and the
# deploy send anything (review A2-R1-1). Fetch them with
#   scripts/fetch-btc-node.sh <host> 971192 971199 fixtures-live/tail
for h in $(seq 971192 971199); do
  for f in "$RESERVE_DIR/$h.raw.hex" "$RESERVE_DIR/$h.v3.json.gz"; do
    [ -s "$f" ] || { echo "missing reserve tail file $f (see docs/ops/demo-testnet.md); nothing sent" >&2; exit 1; }
  done
done
USDC=0x036CbD53842c5426634e7929541eC2318f3dCF7e
export ONYX_CAST=$FOUNDRY_BIN/cast

work=$(mktemp -d)
anvil_pid=""
driver_pid=""
cleanup() {
  [ -n "$driver_pid" ] && kill "$driver_pid" 2>/dev/null || true
  [ -n "$anvil_pid" ] && kill "$anvil_pid" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
castw() { (cd "$work" && "$ONYX_CAST" "$@"); }
# A node URL's path or query can hold an API key: show scheme://host only,
# and pass the URL to cast, forge and the driver in the environment, never
# on a command line (M8-L58-F7).
redact() { REDACT_URL=$1 python3 -I -c 'import os,urllib.parse as u; p=u.urlsplit(os.environ["REDACT_URL"]); print(f"{p.scheme}://{p.hostname or str()}" + ("/..." if (p.path not in ("","/") or p.query) else ""))'; }
port_busy() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

# A value from .env, without printing it (only the named key is read).
env_value() {
  python3 -I - "$REPO/.env" "$1" <<'PY'
import sys
path, name = sys.argv[1], sys.argv[2]
try:
    lines = open(path).read().splitlines()
except FileNotFoundError:
    sys.exit(0)
for line in lines:
    k, sep, v = line.partition("=")
    if sep and k.strip() == name:
        print(v.strip().strip('"').strip("'"))
        break
PY
}

for p in "$API_PORT" "$((API_PORT + 2))"; do
  if port_busy "$p"; then
    echo "port $p is in use; set ONYX_DEMO_API_PORT" >&2
    exit 1
  fi
done

if [ "$mode" = fork ]; then
  ANVIL_CONFIG=${ONYX_DEMO_ANVIL_CONFIG:-}
  if [ -z "$ANVIL_CONFIG" ] && port_busy "$RPC_PORT"; then
    echo "port $RPC_PORT is in use; set ONYX_DEMO_RPC_PORT" >&2
    exit 1
  fi
  RPC=http://127.0.0.1:$RPC_PORT
  # ONYX_DEMO_KEYS: keep the actor keys (fork only; scripts/soak.sh pays
  # from the customer key and runs onyx-node with the operator key).
  keys=${ONYX_DEMO_KEYS:-$work/keys}
  state=$work/api-state
  if [ -n "$ANVIL_CONFIG" ]; then
    [ -f "$ANVIL_CONFIG" ] || { echo "ONYX_DEMO_ANVIL_CONFIG: no file $ANVIL_CONFIG" >&2; exit 1; }
    cp "$ANVIL_CONFIG" "$work/anvil.json"
  else
    # anvil forks Base Sepolia in memory: nothing is broadcast to the network.
    (cd "$work" && exec "$FOUNDRY_BIN/anvil" --fork-url "$FORK_SOURCE" --port "$RPC_PORT" \
      --accounts 1 --silent --config-out "$work/anvil.json") &
    anvil_pid=$!
  fi
  for _ in $(seq 100); do
    castw chain-id --rpc-url "$RPC" >/dev/null 2>&1 && break
    sleep 0.3
  done
  [ "$(castw chain-id --rpc-url "$RPC")" = 84532 ] || { echo "the fork did not start" >&2; exit 1; }
  # anvil's first dev key (public, holds nothing outside this fork).
  key=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["private_keys"][0])' "$work/anvil.json")
  accounts=(--accounts "$work/anvil.json")
else
  RPC=$(env_value BASE_SEPOLIA_RPC_URL)
  RPC=${RPC:-https://sepolia.base.org}
  key=$(env_value BASE_SEPOLIA_PRIVATE_KEY)
  [ -n "$key" ] || { echo "BASE_SEPOLIA_PRIVATE_KEY is missing from .env" >&2; exit 1; }
  [ "$(cd "$work" && ETH_RPC_URL=$RPC "$ONYX_CAST" chain-id)" = 84532 ] \
    || { echo "$(redact "$RPC") is not Base Sepolia" >&2; exit 1; }
  keys=$REPO/.onyx-testnet/keys
  # The API's receipt log and pending settles outlive the run (REV-11).
  state=$REPO/.onyx-testnet/api-state
  accounts=(--work "$work")
fi
if [ -n "${ONYX_DEMO_DRIVER:-}" ]; then
  driver=$ONYX_DEMO_DRIVER
else
  echo "Building the driver (release)..."
  cargo build --release --locked -p onyx-source-btc --example demo-local
  driver=${CARGO_TARGET_DIR:-$REPO/target}/release/examples/demo-local
fi
common=(--mode "$mode" "${accounts[@]}" --keys "$keys" --port "$API_PORT" --state-dir "$state")
export ONYX_DEMO_RPC=$RPC
[ -n "$yes" ] && common+=(--yes)

# Budget first: print it, check the deployer's balance, stop on --plan.
ONYX_DEPLOYER_KEY=$key "$driver" "${common[@]}" --broadcast /dev/null --plan
[ -n "$plan" ] && exit 0

echo "Deploying the contracts ($mode)..."
(cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/build.log" 2>&1) \
  || { cat "$work/build.log" >&2; exit 1; }
cp -a "$REPO/contracts" "$work/contracts"
rm -rf "$work/contracts/broadcast" "$work/contracts/cache/Deploy.s.sol" \
  "$work/contracts/cache/DeployRevenue.s.sol" "$work/contracts/cache/DeployRegistry.s.sol"
sed -i "s#$REPO/contracts#$work/contracts#g" "$work/contracts/cache/solidity-files-cache.json"
# forge script takes its node from an endpoint alias that reads the URL from
# the environment, so the URL never appears on a command line (M8-L58-F7).
printf '\n[rpc_endpoints]\nonyx_demo = "${ONYX_DEMO_RPC}"\n' >>"$work/contracts/foundry.toml"
# A load-balanced public node (sepolia.base.org) can answer the nonce read
# that forge signs with from a node a block or two behind, right after the
# previous script's last transaction. forge then stops before sending
# ("EOA nonce changed unexpectedly ... Expected 9 got 10"), and --resume reuses
# the same stale nonces. So on testnet let the nodes catch up before each
# script, and retry a failed script from a fresh simulation.
deploy() {
  local try
  for try in 1 2 3 4; do
    [ "$mode" = testnet ] && sleep 20
    (cd "$work/contracts" && BASE_SEPOLIA_PRIVATE_KEY=$key \
      "$FOUNDRY_BIN/forge" script "$1" --rpc-url onyx_demo --broadcast --slow >>"$work/$2.log" 2>&1) && return
    echo "  $2 stopped early (try $try of 4)" >&2
  done
  cat "$work/$2.log" >&2
  exit 1
}
deploy script/Deploy.s.sol:Deploy deploy
bc=$work/contracts/broadcast/Deploy.s.sol/84532/run-latest.json
settlement=$(python3 -I -c 'import json,sys
v=json.load(open(sys.argv[1]))
print(next(t["contractAddress"] for t in v["transactions"] if t["contractName"]=="OnyxSettlement" and t["transactionType"]=="CREATE"))' "$bc")
ONYX_SETTLEMENT=$settlement ONYX_USDC=$USDC deploy script/DeployRevenue.s.sol:DeployRevenue deploy-revenue
rb=$work/contracts/broadcast/DeployRevenue.s.sol/84532/run-latest.json
# The registry's owner is a key of its own, never the coordinator (REG-R1-F3):
# the driver makes it in the keys folder and prints only its address.
registry_owner=$("$driver" --key-address registry-owner --keys "$keys")
ONYX_SETTLEMENT=$settlement ONYX_REGISTRY_OWNER=$registry_owner \
  deploy script/DeployRegistry.s.sol:DeployRegistry deploy-registry
gb=$work/contracts/broadcast/DeployRegistry.s.sol/84532/run-latest.json

log=()
if [ "$mode" = testnet ]; then
  mkdir -p "$REPO/.onyx-testnet/runs"
  log=(--log "$REPO/.onyx-testnet/runs/run-$(date -u +%Y%m%dT%H%M%SZ).log")
fi
ONYX_DEPLOYER_KEY=$key "$driver" "${common[@]}" --broadcast "$bc" --revenue-broadcast "$rb" --registry-broadcast "$gb" \
  --live-dir "$LIVE_DIR" --reserve-dir "$RESERVE_DIR" "${log[@]}" "${flags[@]}" &
driver_pid=$!
unset key
status=0
wait "$driver_pid" || status=$?
driver_pid=""
exit "$status"
