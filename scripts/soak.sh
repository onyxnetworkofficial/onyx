#!/usr/bin/env bash
# Soak and chaos test (ROADMAP L6; report docs/audit/2026-10-14-soak.md):
# onyx-node and onyx-node-btc for hours against a local anvil fork of Base
# Sepolia, behind a fault-injecting JSON-RPC proxy, under mixed paid and
# unpaid load, with new rounds finalizing and the nodes restarted mid-traffic
# (SIGTERM and SIGKILL). Samples every minute and checks the invariants
# (scripts/soak/soak.py, scripts/soak/report.py).
#
#   scripts/soak.sh [--minutes N] [--rounds N] [--round-every SECS]
#                   [--cycle-minutes N] [--leak-window-minutes N] [--clients N]
#
# Local only: the fork reads Base Sepolia state (default
# https://sepolia.base.org, ONYX_FORK_RPC) and keeps every transaction in
# memory; nothing is broadcast. Keys: anvil's public dev key (deployer), the
# rehearsal's actor keys (made in the soak dir) and payer test keys derived
# from a public label (soak-load). cast and forge run in the soak dir, outside
# the repo (Foundry loads a .env from its working directory, M8-L55-F4).
#
# Env: ONYX_SOAK_DIR (artifacts; default a new temp dir, kept), ONYX_SOAK_PORT
# (base port, default 18750: anvil +0, proxy +1, onyx-node +2, onyx-node-btc
# +3, the rehearsal's own servers +10..+12), ONYX_SOAK_PREBUILT=1 (skip the
# build), ONYX_FORK_RPC, ONYX_LIVE_DIR, ONYX_RESERVE_DIR, FOUNDRY_BIN.
# Builds use CARGO_TARGET_DIR (default target/). Exit 0 only if every
# invariant held (summary.json "pass").
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
BASE=${ONYX_SOAK_PORT:-18750}
ANVIL_PORT=$BASE
PROXY_PORT=$((BASE + 1))
NODE_PORT=$((BASE + 2))
BTC_PORT=$((BASE + 3))
DEMO_PORT=$((BASE + 10))
FORK_SOURCE=${ONYX_FORK_RPC:-https://sepolia.base.org}
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
minutes=150
rounds=16
round_every=480
cycle=30
leak=120
clients=8
while [ $# -gt 0 ]; do
  case "$1" in
    --minutes) minutes=$2; shift 2 ;;
    --rounds) rounds=$2; shift 2 ;;
    --round-every) round_every=$2; shift 2 ;;
    --cycle-minutes) cycle=$2; shift 2 ;;
    --leak-window-minutes) leak=$2; shift 2 ;;
    --clients) clients=$2; shift 2 ;;
    *) echo "unknown argument $1 (see the header of $0)" >&2; exit 2 ;;
  esac
done
work=${ONYX_SOAK_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/onyx-soak-XXXXXX")}
mkdir -p "$work"
case "$work" in "$REPO"/*) echo "ONYX_SOAK_DIR must be outside the repo (Foundry .env)" >&2; exit 2 ;; esac
echo "soak dir: $work"
port_busy() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
for p in $ANVIL_PORT $PROXY_PORT $NODE_PORT $BTC_PORT $DEMO_PORT $((DEMO_PORT + 1)) $((DEMO_PORT + 2)); do
  if port_busy "$p"; then echo "port $p is in use; set ONYX_SOAK_PORT" >&2; exit 1; fi
done

if [ -z "${ONYX_SOAK_PREBUILT:-}" ]; then
  echo "Building (release)..."
  cargo build --release --locked -p onyx-node -p onyx-reserve-serve -p onyx-source-btc \
    --bin onyx-node --bin onyx-node-btc --example demo-local --example soak-load
fi
for b in release/onyx-node release/onyx-node-btc release/examples/demo-local release/examples/soak-load; do
  [ -x "$TARGET/$b" ] || { echo "missing $TARGET/$b (build first)" >&2; exit 1; }
done

anvil_pid=""
driver_pid=""
cleanup() {
  [ -n "$driver_pid" ] && kill "$driver_pid" 2>/dev/null || true
  [ -n "$anvil_pid" ] && kill "$anvil_pid" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT TERM
castw() { (cd "$work" && "$FOUNDRY_BIN/cast" "$@"); }
RPC=http://127.0.0.1:$ANVIL_PORT

# 1. The chain: an anvil fork of Base Sepolia, automine while the rehearsal
# deploys and settles (interval mining afterwards).
(cd "$work" && exec "$FOUNDRY_BIN/anvil" --fork-url "$FORK_SOURCE" --port "$ANVIL_PORT" \
  --accounts 1 --silent --config-out "$work/anvil.json") >"$work/anvil.log" 2>&1 &
anvil_pid=$!
for _ in $(seq 150); do
  [ -s "$work/anvil.json" ] && [ "$(castw chain-id --rpc-url "$RPC" 2>/dev/null)" = 84532 ] && break
  sleep 0.2
done
[ "$(castw chain-id --rpc-url "$RPC" 2>/dev/null)" = 84532 ] || { echo "the fork did not start" >&2; exit 1; }

# 2. The rehearsal (scripts/demo-testnet.sh fork): deploy, settle round 1 and
# the reserve round, export the node inputs, then keep settling soak rounds.
echo "Deploying and settling the rehearsal on the fork..."
ONYX_DEMO_DRIVER=$TARGET/release/examples/demo-local ONYX_DEMO_ANVIL_CONFIG=$work/anvil.json \
  ONYX_DEMO_RPC_PORT=$ANVIL_PORT ONYX_DEMO_API_PORT=$DEMO_PORT ONYX_DEMO_KEYS=$work/keys \
  scripts/demo-testnet.sh fork --serve --export-node "$work/export" \
  --soak-rounds "$rounds" --soak-every "$round_every" >"$work/driver.log" 2>&1 &
driver_pid=$!
for _ in $(seq 900); do
  grep -q '^SOAK READY' "$work/driver.log" && break
  kill -0 "$driver_pid" 2>/dev/null || { tail -40 "$work/driver.log" >&2; echo "the rehearsal stopped" >&2; exit 1; }
  sleep 1
done
grep -q '^SOAK READY' "$work/driver.log" || { tail -20 "$work/driver.log" >&2; echo "no SOAK READY" >&2; exit 1; }
echo "  $(grep '^SOAK READY' "$work/driver.log")"

# 3. Gas for the operator, test USDC for the payers (minted by the token's
# master minter, impersonated), then interval mining (2 s blocks, like Base).
operator=$("$TARGET/release/examples/demo-local" --key-address operator --keys "$work/keys")
castw rpc anvil_setBalance "$operator" 0x3635c9adc5dea00000 --rpc-url "$RPC" >/dev/null
revenue=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["revenue"])' "$work/export/deployment.json")
usdc=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["usdc"])' "$work/export/deployment.json")
minter=$(castw call "$usdc" 'masterMinter()(address)' --rpc-url "$RPC")
funder=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["available_accounts"][0])' "$work/anvil.json")
castw rpc anvil_impersonateAccount "$minter" --rpc-url "$RPC" >/dev/null
castw rpc anvil_setBalance "$minter" 0xde0b6b3a7640000 --rpc-url "$RPC" >/dev/null
castw send --unlocked --from "$minter" "$usdc" 'configureMinter(address,uint256)' "$funder" 1000000000000 \
  --rpc-url "$RPC" >/dev/null
castw rpc anvil_stopImpersonatingAccount "$minter" --rpc-url "$RPC" >/dev/null
"$TARGET/release/examples/soak-load" addresses --clients "$clients" >"$work/payers.txt"
while read -r a; do
  castw send --unlocked --from "$funder" "$usdc" 'mint(address,uint256)' "$a" 500000000 \
    --rpc-url "$RPC" >/dev/null
done <"$work/payers.txt"
echo "  funded the operator $operator (gas) and $(wc -l <"$work/payers.txt") payers (500 test USDC each)"
castw rpc evm_setIntervalMining 2 --rpc-url "$RPC" >/dev/null

# 4. Node inputs: the export, with the fresh window stretched so every row is
# priced (the data is days old), and the reserve gate on this one-shard chain.
etc=$work/etc
mkdir -p "$etc"
cp "$work/export/deployment.json" "$etc/"
python3 -I - "$work/export/catalog.json" "$etc/catalog.json" <<'EOF'
import json, sys
c = json.load(open(sys.argv[1]))
for p in c["pricing"].values():
    p["freshWindowSecs"] = 10 * 365 * 86400
open(sys.argv[2], "w").write(json.dumps(c, indent=2) + "\n")
EOF
python3 -I - "$REPO/deploy/reserve-gate.json" "$etc/reserve-gate.json" "$REPO/fixtures" <<'EOF'
import json, sys
g = json.load(open(sys.argv[1]))
g.update(fixtures=sys.argv[3], startHeight=971191)
open(sys.argv[2], "w").write(json.dumps(g, indent=2) + "\n")
EOF
for n in node btc; do
  rm -rf "$work/cas-$n"
  cp -r "$work/export/cas" "$work/cas-$n"
  mkdir -p -m 0700 "$work/state-$n"
done
topics=$(for sig in 'AuthorizationUsed(address,bytes32)' 'Transfer(address,address,uint256)' \
  'Settled(uint64,bytes32,address,bytes32,uint128)' 'Credited(uint64,bytes32,uint128,address,bytes32)' \
  'ReceiptSkipped(uint64,bytes32,address,bytes32,uint8)'; do castw keccak "$sig"; done | paste -sd, -)
cat >"$work/soak.json" <<EOF
{
  "repo": "$REPO",
  "work": "$work",
  "anvil": "$RPC",
  "proxyPort": $PROXY_PORT,
  "nodePort": $NODE_PORT,
  "btcPort": $BTC_PORT,
  "nodeBin": "$TARGET/release/onyx-node",
  "btcBin": "$TARGET/release/onyx-node-btc",
  "loadBin": "$TARGET/release/examples/soak-load",
  "operatorKey": "$work/keys/operator.key",
  "revenue": "$revenue",
  "usdc": "$usdc",
  "topics": "$topics",
  "minutes": $minutes,
  "cycleMinutes": $cycle,
  "leakWindowMinutes": $leak,
  "clients": $clients,
  "rounds": $rounds,
  "roundEvery": $round_every,
  "driverPid": $driver_pid,
  "anvilPid": $anvil_pid
}
EOF

# 5. The soak itself.
status=0
python3 -I "$REPO/scripts/soak/soak.py" "$work/soak.json" || status=$?
rstatus=0
python3 -I "$REPO/scripts/soak/report.py" "$work/soak.json" || rstatus=$?
[ "$status" = 0 ] && status=$rstatus
echo "soak dir: $work (summary.json, samples.jsonl, events.jsonl, load.jsonl, logs)"
exit "$status"
