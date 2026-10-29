#!/usr/bin/env bash
# M8 rehearsal on a local chain (docs/ops/demo-local.md): start anvil, deploy
# the settlement contracts, run one real round on chain, serve it with
# onyx-api reading roots from the chain, and check a row as a client.
# Local only: anvil's chain id 31337 and its public dev keys; never a public
# network. Foundry loads a `.env` from its working directory and project root
# (the repo's `.env` holds the testnet key), so every cast and forge call here
# runs inside a temp dir outside the repo, where there is none (M8-L55-F4).
#
# Env: ONYX_LIVE_DIR (node block files; default fixtures-live/node, falls back
# to the committed fixture), ONYX_RESERVE_DIR (reserve tail block files for
# step 8, btc.entity-reserve; default fixtures-live/tail, skipped without
# them), ONYX_DEMO_RPC_PORT (8545), ONYX_DEMO_API_PORT (8403; step 8 serves
# on that port + 2), FOUNDRY_BIN (~/.foundry/bin), ONYX_DEMO_DRIVER (a
# prebuilt driver).
# Pass --dispute to add the dispute round, --serve to keep the API running.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
RPC_PORT=${ONYX_DEMO_RPC_PORT:-8545}
API_PORT=${ONYX_DEMO_API_PORT:-8403}
LIVE_DIR=${ONYX_LIVE_DIR:-$REPO/fixtures-live/node}
RESERVE_DIR=${ONYX_RESERVE_DIR:-$REPO/fixtures-live/tail}
RPC=http://127.0.0.1:$RPC_PORT
export ONYX_CAST=$FOUNDRY_BIN/cast

work=$(mktemp -d)
anvil_pid=""
watch_pid=""
driver_pid=""
cleanup() {
  [ -n "$driver_pid" ] && kill "$driver_pid" 2>/dev/null || true
  [ -n "$anvil_pid" ] && kill "$anvil_pid" 2>/dev/null || true
  [ -n "$watch_pid" ] && kill "$watch_pid" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
# The driver runs in the background and the shell `wait`s on it, so INT and
# TERM are handled at once, even with --serve (M8-L56-F3).
trap 'exit 130' INT TERM

# cast and forge never run from the repo (see the header).
castw() { (cd "$work" && "$ONYX_CAST" "$@"); }
port_busy() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

if castw chain-id --rpc-url "$RPC" >/dev/null 2>&1 || port_busy "$RPC_PORT"; then
  echo "port $RPC_PORT is in use (a node?); set ONYX_DEMO_RPC_PORT" >&2
  exit 1
fi
if port_busy "$API_PORT" || port_busy "$((API_PORT + 2))"; then
  echo "port $API_PORT or $((API_PORT + 2)) is in use; set ONYX_DEMO_API_PORT" >&2
  exit 1
fi

if [ -n "${ONYX_DEMO_DRIVER:-}" ]; then
  driver=$ONYX_DEMO_DRIVER   # prebuilt (the gated test passes its own build)
else
  echo "Building the driver (release)..."
  cargo build --release --locked -p onyx-source-btc --example demo-local
  driver=${CARGO_TARGET_DIR:-$REPO/target}/release/examples/demo-local
fi

(cd "$work" && exec "$FOUNDRY_BIN/anvil" --port "$RPC_PORT" --accounts 20 --silent \
  --config-out "$work/anvil.json") &
anvil_pid=$!
# If this script is killed outright (SIGKILL skips the trap), the watchdog
# still stops anvil and the driver (its pid is in $work/driver.pid) and removes
# the temp dir once this shell is gone (M8-L55-F6, M8-L56-F3).
(
  trap '' INT TERM
  while kill -0 $$ 2>/dev/null; do sleep 1; done
  [ -f "$work/driver.pid" ] && kill "$(cat "$work/driver.pid")" 2>/dev/null
  kill "$anvil_pid" 2>/dev/null
  sleep 1
  rm -rf "$work"
) </dev/null >/dev/null 2>&1 &
# The watchdog must not hold this script's stdout or stderr: a caller reading
# them to EOF (the gated test) would wait for it, while it waits for this
# script, which stays a zombie until that caller reaps it (M8-L57-N7).
watch_pid=$!
for _ in $(seq 50); do
  castw chain-id --rpc-url "$RPC" >/dev/null 2>&1 && break
  sleep 0.2
done
[ "$(castw chain-id --rpc-url "$RPC")" = 31337 ] || { echo "anvil did not start" >&2; exit 1; }

echo "Deploying the contracts to anvil..."
# anvil's first dev key (public, holds nothing outside this node), only for
# this one command; the deploy script refuses any chain but 84532 and 31337.
key=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["private_keys"][0])' "$work/anvil.json")
# Build in place from the temp dir (`--root`), then deploy from a copy of
# contracts/ in the temp dir, so the broadcast record and forge's sensitive
# script cache land there, never in the repo. The copied compile cache names
# absolute paths; rewriting them keeps forge from recompiling (about 70 s).
(cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/build.log" 2>&1) \
  || { cat "$work/build.log" >&2; exit 1; }
cp -a "$REPO/contracts" "$work/contracts"
rm -rf "$work/contracts/broadcast" "$work/contracts/cache/Deploy.s.sol"
sed -i "s#$REPO/contracts#$work/contracts#g" "$work/contracts/cache/solidity-files-cache.json"
if ! (cd "$work/contracts" && BASE_SEPOLIA_PRIVATE_KEY=$key \
  "$FOUNDRY_BIN/forge" script script/Deploy.s.sol:Deploy --rpc-url "$RPC" --broadcast \
  >"$work/deploy.log" 2>&1); then
  cat "$work/deploy.log" >&2
  exit 1
fi
bc=$work/contracts/broadcast/Deploy.s.sol/31337/run-latest.json
settlement=$(python3 -I -c 'import json,sys
v=json.load(open(sys.argv[1]))
print(next(t["contractAddress"] for t in v["transactions"] if t["contractName"]=="OnyxSettlement" and t["transactionType"]=="CREATE"))' "$bc")
# The on-chain metric registry (ADR-0030). Its owner is anvil's last dev
# account, never the coordinator (REG-R1-F3; the driver uses accounts 0-18).
registry_owner=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["available_accounts"][19])' "$work/anvil.json")
if ! (cd "$work/contracts" && BASE_SEPOLIA_PRIVATE_KEY=$key ONYX_SETTLEMENT=$settlement \
  ONYX_REGISTRY_OWNER=$registry_owner \
  "$FOUNDRY_BIN/forge" script script/DeployRegistry.s.sol:DeployRegistry --rpc-url "$RPC" --broadcast \
  >"$work/deploy-registry.log" 2>&1); then
  cat "$work/deploy-registry.log" >&2
  exit 1
fi
unset key

"$driver" --rpc "$RPC" --accounts "$work/anvil.json" \
  --broadcast "$bc" \
  --registry-broadcast "$work/contracts/broadcast/DeployRegistry.s.sol/31337/run-latest.json" \
  --live-dir "$LIVE_DIR" --reserve-dir "$RESERVE_DIR" --port "$API_PORT" --state-dir "$work/api-state" "$@" &
driver_pid=$!
echo "$driver_pid" >"$work/driver.pid"
status=0
wait "$driver_pid" || status=$?
driver_pid=""
exit "$status"
