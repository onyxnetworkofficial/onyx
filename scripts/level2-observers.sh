#!/usr/bin/env bash
# ROADMAP B2 rehearsal: a live level-2 round with INDEPENDENT observer processes, each with its
# own key, on ONE machine against a local anvil (chain id 31337). Takes about 7 minutes (the
# Treasury feed's window is 300 s, commits close 60 s after it).
#
# - anvil with 1 s blocks (block time follows the wall clock), OnyxRegistry next to a stub
#   ledger whose coordinator is dev account 0 (owner: account 1; account 2 stands for the
#   reporter, so the receipt key is checked against it).
# - Seven pool members = anvil dev accounts 3..9, each a process of its own with its own 0600
#   key file (read from anvil's --config-out; no key is written into the repository); the
#   pool snapshot file lists their addresses and bonds. Five are drawn.
# - A fresh random receipt key (ROADMAP B3) in its own 0600 file.
# - The coordinator (examples/level2-coordinator.rs, system clock) draws, then waits; each
#   observer (examples/level2-observer.rs) reads the draw from the registry itself, fetches
#   the Treasury record itself over HTTPS at its own slot in the window (M2), posts its own commit on chain,
#   and writes its signed reveal to the spool after the commit phase; the coordinator absorbs
#   the posted commits, takes the reveals, receipts them on chain with the receipt key and
#   settles. Each drawn observer must see its receipt on chain; undrawn ones exit "not drawn".
#
# What this does NOT show: independence. Every process shares one host, one network path and
# one operator (THREATS L2-SRC-1); real independence needs separate operators (the project owner's
# decision). EDGAR observers stay rehearsal-only until an operator role contact exists (SEC-4).
#
# Env: FOUNDRY_BIN (~/.foundry/bin), ONYX_L2_RPC_PORT (8549), ONYX_L2_LEAD (seconds until the
# window opens, 45), ONYX_L2_LOG_DIR (logs and the round's output; default a temp dir that is
# removed), ONYX_L2_BODY (a saved response file: observers read it instead of fetching; an
# offline run, its record date must still be in the allowed lag), ONYX_L2_RECORD_DATE (the
# record date to observe, YYYY-MM-DD; default `latest`). The round's results store is kept as
# `$ONYX_L2_LOG_DIR/store.json` (pre-public loop PP9: scripts/feed-live.sh feeds it to a receipt).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
PORT=${ONYX_L2_RPC_PORT:-8549}
RPC=http://127.0.0.1:$PORT
LEAD=${ONYX_L2_LEAD:-45}
work=$(mktemp -d)
logs=${ONYX_L2_LOG_DIR:-$work/logs}
mkdir -p "$logs"
anvil_pid=""
pids=()
cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  [ -n "$anvil_pid" ] && kill "$anvil_pid" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
castw() { (cd "$work" && ETH_RPC_URL=$RPC "$FOUNDRY_BIN/cast" "$@"); }

(cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/build.log" 2>&1) \
  || { tail -20 "$work/build.log"; exit 1; }
artifact=$REPO/contracts/out/OnyxRegistry.sol/OnyxRegistry.json
cargo build -q -p onyx-source-fiscaldata --example level2-coordinator --example level2-observer
bin=${CARGO_TARGET_DIR:-$REPO/target}/debug/examples

"$FOUNDRY_BIN/anvil" --port "$PORT" --silent --block-time 1 --accounts 10 \
  --config-out "$work/accounts.json" &
anvil_pid=$!
for _ in $(seq 50); do castw chain-id >/dev/null 2>&1 && break; sleep 0.2; done
[ "$(castw chain-id)" = 31337 ] || { echo "not a local anvil"; exit 1; }

COORD=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266
OWNER=0x70997970C51812dc3A010C7d01b50e0d17dc79C8
REPORTER=0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC
LEDGER=0x00000000000000000000000000000000000c0de1
castw rpc anvil_setCode "$LEDGER" \
  "0x60003560e01c63d7d85a5714602c5773${COORD#0x}60005260206000f35b60206000f3" >/dev/null
code=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["bytecode"]["object"])' "$artifact")
ctor=$(castw abi-encode "c(address,address)" "$OWNER" "$LEDGER")
REG=$(castw send --unlocked --from "$OWNER" --create "${code}${ctor#0x}" --json \
  | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["contractAddress"])')
echo "OnyxRegistry at $REG on the local anvil (chain 31337, 1 s blocks)"

# Key files (0600) of observers 3..9 from anvil's own account list, the pool snapshot, and a
# fresh random receipt key. Nothing is printed but addresses.
mkdir -m 700 "$work/keys"
python3 -I - "$work/accounts.json" "$work/keys" <<'PY'
import json, os, secrets, sys
acc = json.load(open(sys.argv[1]))
keys, addrs = acc["private_keys"], acc["available_accounts"]
out = sys.argv[2]
def write(name, hexkey):
    fd = os.open(os.path.join(out, name), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    os.write(fd, hexkey.removeprefix("0x").encode()); os.close(fd)
pool = []
for i in range(3, 10):
    write(f"observer-{i}.key", keys[i])
    pool.append({"address": addrs[i], "bond": 1000})
json.dump(pool, open(os.path.join(out, "pool.json"), "w"))
write("receipt.key", secrets.token_hex(32))
PY

window_start=$(( $(date +%s) + LEAD ))
feed=(--record-date "${ONYX_L2_RECORD_DATE:-latest}" --window-start "$window_start")
chain=(--rpc "$RPC" --registry "$REG" --cast "$FOUNDRY_BIN/cast")
body=()
[ -n "${ONYX_L2_BODY:-}" ] && body=(--body "$ONYX_L2_BODY")
mkdir -m 700 "$work/spool" "$work/state"
for i in $(seq 3 9); do
  "$bin/level2-observer" --key-file "$work/keys/observer-$i.key" --pool "$work/keys/pool.json" \
    --spool "$work/spool" --state "$work/state/observer-$i" "${feed[@]}" "${chain[@]}" \
    "${body[@]}" >"$logs/observer-$i.log" 2>&1 &
  pids+=($!)
done
echo "7 observer processes started; the window opens at $window_start (in ${LEAD}s), the round ends about 7 minutes from now"

"$bin/level2-coordinator" "${chain[@]}" --coordinator "$COORD" --owner "$OWNER" \
  --reporter "$REPORTER" --book "$work/book.json" --store "$work/store.json" --new-store \
  --pool "$work/keys/pool.json" --spool "$work/spool" --receipt-key "$work/keys/receipt.key" \
  "${feed[@]}" >"$logs/round.json" 2>"$logs/coordinator.log" \
  || { echo "the coordinator failed:"; tail -20 "$logs/coordinator.log"; exit 1; }
cat "$logs/round.json"
cp "$work/store.json" "$logs/store.json"

receipted=0 undrawn=0 failed=0
for idx in "${!pids[@]}"; do
  i=$((idx + 3))
  if wait "${pids[$idx]}"; then
    if grep -q "receipt on chain" "$logs/observer-$i.log"; then receipted=$((receipted + 1));
    elif grep -q "not drawn" "$logs/observer-$i.log"; then undrawn=$((undrawn + 1));
    else failed=$((failed + 1)); fi
  else
    failed=$((failed + 1)); echo "observer $i failed:"; tail -5 "$logs/observer-$i.log"
  fi
done
pids=()
echo "observers: $receipted receipted on chain, $undrawn not drawn, $failed failed"
python3 -I - "$logs/round.json" "$receipted" "$undrawn" "$failed" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
receipted, undrawn, failed = map(int, sys.argv[2:])
assert r["outcome"].startswith("Value(Number("), r["outcome"]
assert r["commits_from_chain"] == 5 and r["reveals"] == 5, r
assert all(o["status"] == "Agreed" for o in r["observers"]), r["observers"]
assert r["level2_claim"] is True
assert (receipted, undrawn, failed) == (5, 2, 0), (receipted, undrawn, failed)
PY
echo "LEVEL-2 INDEPENDENT OBSERVERS REHEARSAL PASSED"
