#!/usr/bin/env bash
# ROADMAP C51 rehearsal: TIME-LOCKED sealed level-2 reveals, end to end, with INDEPENDENT observer
# processes (distinct anvil dev keys) on ONE machine against a local anvil (chain id 31337).
#
# The beacon is drand quicknet's, but served from FIXTURES (fixtures/tlock/drand/
# quicknet-c51-rounds.json: published rounds, each signature verified against the pinned
# quicknet key wherever it is used). For that the anvil's clock is started in the past
# (`anvil --timestamp`, fixed below) and every process runs on `--clock anvil` (the system clock
# shifted to the anvil's block time; refused on any other chain), so each window's lock round is
# one already published. The LIVE drand path (`--beacon drand`, api.drand.sh) is exercised only by
# the opt-in test `ONYX_DRAND_LIVE=1 cargo test -p onyx-coordinator c51_live -- --ignored`.
#
# 1. The coordinator sets its own reveal key and receipt key and calls requireTimeLock() (ONE WAY:
#    nobody can turn it off); every draw from then on is keyed and time-locked (drawTimeLocked).
# 2. Draw 0: every observer posts GARBAGE: odd-numbered ones junk behind a VALID time-lock
#    (--sealed-garbage), even-numbered ones junk with no valid time-lock (--sealed-garbage-raw).
#    The coordinator is first run with a beacon fixture WITHOUT the lock round (drand down or
#    late): past reveal close it waits and opens nothing (no record on chain); it is stopped and
#    run again on the same book with the full fixture: it dismisses all five posts (openings for
#    the valid time-locks), noResult, recorded with recordAttempt.
# 3. Draw 1 of the same fact: honest observers seal their signed reveals time-locked to the lock
#    round; the coordinator opens them only with that round's verified signature and settles the
#    value (recordObservation).
# 4. The Rust client (level2-verify) checks the served answer off chain (every dismissal by exact
#    re-encryption or under the verified signature) and on chain (drawTimeLocked and the C11
#    calls), then refuses it doctored (time-lock hidden, signature forged, an opening changed or
#    dropped, and the C11 cases).
#
# Not shown: independence (one host, one operator: THREATS L2-SRC-1); the live drand relays (opt-in
# test only); a Base Sepolia run (the lead redeploys OnyxRegistry with C31); clock lag (C53).
#
# Env: FOUNDRY_BIN (~/.foundry/bin), ONYX_L2_RPC_PORT (8554), ONYX_L2_LOG_DIR (logs; default a temp
# dir that is removed). About 30 minutes (the anvil clock runs from T0 at wall-clock speed).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
PORT=${ONYX_L2_RPC_PORT:-8554}
RPC=http://127.0.0.1:$PORT
FIXTURE=$REPO/fixtures/tlock/drand/quicknet-c51-rounds.json
# Fixed so the lock rounds are the fixture's: the anvil starts at T0, draw 0's window opens at
# WS0 and draw 1's at WS1 (lock rounds 33152432 and 33152732; see the fixture).
T0=1792260000
WS0=$((T0 + 120))
WS1=$((T0 + 1020))
RECORD_DATE=2026-10-15
work=$(mktemp -d)
logs=${ONYX_L2_LOG_DIR:-$work/logs}
mkdir -p "$logs"
anvil_pid=""
coord_pid=""
pids=()
cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  [ -n "$coord_pid" ] && kill "$coord_pid" 2>/dev/null || true
  [ -n "$anvil_pid" ] && kill "$anvil_pid" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
castw() { (cd "$work" && ETH_RPC_URL=$RPC "$FOUNDRY_BIN/cast" "$@"); }
chain_now() { castw block latest -f timestamp; }

(cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/build.log" 2>&1) \
  || { tail -20 "$work/build.log"; exit 1; }
artifact=$REPO/contracts/out/OnyxRegistry.sol/OnyxRegistry.json
cargo build -q -p onyx-source-fiscaldata --example level2-coordinator --example level2-observer \
  --example level2-verify
bin=${CARGO_TARGET_DIR:-$REPO/target}/debug/examples

"$FOUNDRY_BIN/anvil" --port "$PORT" --silent --block-time 1 --accounts 10 --timestamp "$T0" \
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
echo "OnyxRegistry at $REG on a local anvil (chain 31337, 1 s blocks, clock from $T0)"
[ "$(chain_now)" -lt $((WS0 - 50)) ] || { echo "setup took too long for the fixed window"; exit 1; }

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
write("reveal.key", secrets.token_hex(32))
PY
# Draw 0's first coordinator run: the fixture without draw 0's lock round (drand late or down).
python3 -I - "$FIXTURE" "$work/beacon-late.json" 33152432 <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
d["rounds"] = [r for r in d["rounds"] if r["round"] != int(sys.argv[3])]
json.dump(d, open(sys.argv[2], "w"))
PY

chain=(--rpc "$RPC" --registry "$REG" --cast "$FOUNDRY_BIN/cast")
clock=(--clock anvil)
mkdir -m 700 "$work/spool"

wait_chain() { # until the anvil's block time reaches $1
  while [ "$(chain_now)" -lt "$1" ]; do sleep 1; done
}

start_observers() { # NAME WS [garbage]
  local name=$1 ws=$2 garbage=${3:-}
  mkdir -m 700 "$work/state-$name"
  pids=()
  for i in $(seq 3 9); do
    local extra=()
    if [ -n "$garbage" ]; then
      if [ $((i % 2)) = 1 ]; then extra=(--sealed-garbage); else extra=(--sealed-garbage-raw); fi
    fi
    "$bin/level2-observer" --key-file "$work/keys/observer-$i.key" --pool "$work/keys/pool.json" \
      --spool "$work/spool" --state "$work/state-$name/observer-$i" --record-date "$RECORD_DATE" \
      --window-start "$ws" "${chain[@]}" "${clock[@]}" "${extra[@]}" \
      >"$logs/$name-observer-$i.log" 2>&1 &
    pids+=($!)
  done
  echo "$name: 7 observer processes started; the window opens at $ws (anvil time)"
}

coordinator() { # NAME WS BEACON-FILE [--require-time-lock]
  local name=$1 ws=$2 beacon=$3; shift 3
  local new=()
  [ -f "$work/store.json" ] || new=(--new-store)
  "$bin/level2-coordinator" "${chain[@]}" "${clock[@]}" --coordinator "$COORD" --owner "$OWNER" \
    --reporter "$REPORTER" --book "$work/book.json" --store "$work/store.json" "${new[@]}" \
    --pool "$work/keys/pool.json" --spool "$work/spool" --receipt-key "$work/keys/receipt.key" \
    --reveal-key "$work/keys/reveal.key" --beacon "fixture:$beacon" \
    --record-date "$RECORD_DATE" --window-start "$ws" "$@"
}

finish_observers() { # NAME: counts posted/undrawn/failed; sets OPENABLE (drawn odd observers)
  local name=$1 posted=0 undrawn=0 failed=0
  OPENABLE=0
  for idx in "${!pids[@]}"; do
    local i=$((idx + 3))
    if wait "${pids[$idx]}"; then
      if grep -q "sealed post on chain" "$logs/$name-observer-$i.log"; then
        posted=$((posted + 1)); [ $((i % 2)) = 1 ] && OPENABLE=$((OPENABLE + 1))
        grep -q "time-locked" "$logs/$name-observer-$i.log" \
          || { echo "$name: observer $i did not post time-locked"; exit 1; }
      elif grep -q "not drawn" "$logs/$name-observer-$i.log"; then undrawn=$((undrawn + 1));
      else failed=$((failed + 1)); fi
    else
      failed=$((failed + 1)); echo "$name: observer $i failed:"; tail -5 "$logs/$name-observer-$i.log"
    fi
  done
  pids=()
  echo "$name observers: $posted time-locked sealed posts on chain, $undrawn not drawn, $failed failed"
  [ "$posted/$undrawn/$failed" = "5/2/0" ] || { echo "$name: expected 5/2/0"; exit 1; }
}

# ---- draw 0: garbage; drand "late" first.
wait_chain $((WS0 - 75))
start_observers draw0 "$WS0" garbage
coordinator draw0 "$WS0" "$work/beacon-late.json" --require-time-lock \
  >"$logs/draw0-late.json" 2>"$logs/draw0-late-coordinator.log" &
coord_pid=$!
until grep -q "waiting for drand quicknet round 33152432" "$logs/draw0-late-coordinator.log"; do
  kill -0 "$coord_pid" 2>/dev/null \
    || { echo "draw0: the coordinator stopped instead of waiting:"; tail -20 "$logs/draw0-late-coordinator.log"; exit 1; }
  sleep 2
done
finish_observers draw0
[ "$(castw call "$REG" 'timeLockRequired()(bool)')" = true ] || { echo "time-locks not required"; exit 1; }
sleep 10
# Past reveal close and the beacon missing: still waiting, nothing settled, nothing recorded.
kill -0 "$coord_pid" 2>/dev/null || { echo "draw0: the coordinator did not wait"; exit 1; }
[ -s "$logs/draw0-late.json" ] && { echo "draw0: settled without the beacon"; exit 1; }
recorded=$(castw logs --from-block 0 --address "$REG" "AttemptRecorded(bytes32,uint32,bytes32,bytes32)" --json \
  | python3 -I -c 'import json,sys; print(len(json.load(sys.stdin)))')
[ "$recorded" = 0 ] || { echo "draw0: a round was recorded without the beacon"; exit 1; }
kill "$coord_pid"; wait "$coord_pid" 2>/dev/null || true; coord_pid=""
echo "draw 0: drand round 33152432 missing past reveal close: the coordinator waited, opened nothing; stopped"
coordinator draw0 "$WS0" "$FIXTURE" >"$logs/draw0.json" 2>"$logs/draw0-coordinator.log" \
  || { echo "draw0: the coordinator failed:"; tail -20 "$logs/draw0-coordinator.log"; exit 1; }
cat "$logs/draw0.json"
python3 -I - "$logs/draw0.json" "$OPENABLE" <<'PY'
import json, sys
r = json.load(open(sys.argv[1])); openable = int(sys.argv[2])
assert r["attempt"] == 0 and r["outcome"] == "NoResult", r
assert r["commits_from_chain"] == 5 and r["reveals"] == 0, r
assert r["sealed_posts"] == 5 and len(r["dismissed"]) == 5, r
assert r["time_locked"] is True and r["lock_round"] == 33152432, r
# ROADMAP C52: the far round of the record date's last observable day (2026-10-25): the first
# quicknet round at or after its latest reveal close (window, commit grace, reveal phase) + 1 day.
far = -(-((20751 + 1) * 86400 - 1 + 300 + 600 + 86400 + 86400 - 1692803367) // 3) + 1
assert r["far_round"] == far, (r["far_round"], far)
assert r["openings"] == openable, (r["openings"], openable)
assert r["recorded_attempt"], r
PY
echo "draw 0: after the beacon, 5 garbage posts dismissed ($OPENABLE with time-lock openings), noResult recorded"

# ---- draw 1: honest, time-locked.
wait_chain $((WS1 - 75))
start_observers draw1 "$WS1"
coordinator draw1 "$WS1" "$FIXTURE" >"$logs/draw1.json" 2>"$logs/draw1-coordinator.log" \
  || { echo "draw1: the coordinator failed:"; tail -20 "$logs/draw1-coordinator.log"; exit 1; }
finish_observers draw1
cat "$logs/draw1.json"
python3 -I - "$logs/draw1.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
assert r["attempt"] == 1 and r["outcome"].startswith("Value(Number("), r
assert r["commits_from_chain"] == 5 and r["reveals"] == 5, r
assert r["sealed_posts"] == 5 and r["dismissed"] == [], r
assert r["time_locked"] is True and r["lock_round"] == 33152732, r
far = -(-((20751 + 1) * 86400 - 1 + 300 + 600 + 86400 + 86400 - 1692803367) // 3) + 1
assert r["far_round"] == far, (r["far_round"], far)
assert r["level2_claim"] is True, r
PY
echo "draw 1: 5 time-locked reveals opened with round 33152732's verified signature, value recorded"

"$bin/level2-verify" "${chain[@]}" --store "$work/store.json" >"$logs/client.json" \
  || { echo "the client refused the served rounds"; exit 1; }
cat "$logs/client.json"
python3 -I - "$logs/client.json" "$OPENABLE" <<'PY'
import json, sys
c = json.load(open(sys.argv[1])); openable = int(sys.argv[2])
assert c["client_checks"] == "passed"
[f] = c["facts"]
assert f["attempt"] == 1 and f["value_round_sealed_posts"] == 5 and f["value_round_time_locked"], f
[e] = f["earlier_rounds"]
assert e["attempt"] == 0 and e["dismissed"] == 5 and e["time_locked"], e
cases = {d["case"]: d["refused"] for d in f["doctored"]}
want = 8 + (2 if openable else 0)
assert len(cases) == want and all(cases.values()), cases
PY
echo "LEVEL-2 TIME-LOCKED SEALED REVEALS REHEARSAL PASSED (fixture beacon; live drand only in the opt-in test)"
