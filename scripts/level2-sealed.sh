#!/usr/bin/env bash
# ROADMAP C11 rehearsal: sealed level-2 reveals, end to end, with INDEPENDENT observer processes
# (their own keys) on ONE machine against a local anvil (chain id 31337). Two rounds of one fact,
# about 20 minutes (the Treasury feed's window is 300 s; commits close 60 s after it, reveals
# 120 s later):
#
# 1. Draw 0: the coordinator (examples/level2-coordinator.rs) has a reveal key of its own
#    (--reveal-key, a fresh random key in a 0600 file; distinct from the receipt key and every
#    other role, refused otherwise) and sets it on OnyxRegistry before announcing the draw, which
#    snapshots it. Every observer process posts GARBAGE (--sealed-garbage, a griefer): each drawn
#    one's post lands on chain in the reveal phase; the coordinator reads every drawn slot from one
#    block past reveal close, dismisses each post with a proof, finds no reveal (noResult),
#    checks revealLogComplete and records the round with recordAttempt before writing it.
# 2. Draw 1 of the same fact: honest observers seal their signed reveals to the reveal key with a
#    fresh ephemeral key and post them (postSealedReveal; no spool, no receipt: the post is the
#    receipt); the coordinator opens each one and settles the value, recorded with
#    recordObservation.
# 3. The Rust client (examples/level2-verify.rs) serves the results store as the API does
#    (outcome=all) and checks it off chain (every key proof and dismissal) and on chain against
#    this anvil with its own arguments (drawRevealKey, revealLogComplete, every served post
#    against sealedRevealOf/sealedKeyProofOf, attemptMatches, and the level-2 calls); then it
#    serves the answer doctored three ways (a round without its sealed posts, a dismissal left out,
#    another outcome) and must refuse each.
#
# What this does NOT show: independence (one host, one network path, one operator: THREATS
# L2-SRC-1), the coordinator's early view of posts (C4-7, TODO C12), or a Base Sepolia run (the
# lead redeploys OnyxRegistry).
#
# Env: FOUNDRY_BIN (~/.foundry/bin), ONYX_L2_RPC_PORT (8553), ONYX_L2_LEAD (45 s until each window
# opens), ONYX_L2_LOG_DIR (logs and outputs; default a temp dir that is removed),
# ONYX_L2_RECORD_DATE (default: the newest record the Treasury API publishes, read once).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
PORT=${ONYX_L2_RPC_PORT:-8553}
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
cargo build -q -p onyx-source-fiscaldata --example level2-coordinator --example level2-observer \
  --example level2-verify
bin=${CARGO_TARGET_DIR:-$REPO/target}/debug/examples

date_of=${ONYX_L2_RECORD_DATE:-}
if [ -z "$date_of" ]; then
  date_of=$(curl -sS --fail --proto =https -m 20 \
    "https://api.fiscaldata.treasury.gov/services/api/fiscal_service/v2/accounting/od/debt_to_penny?sort=-record_date&page%5Bsize%5D=1&fields=record_date" \
    | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["data"][0]["record_date"])')
fi
echo "record date $date_of (one fact, two draws)"

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

# Key files (0600): observers 3..9 from anvil's own account list, the pool snapshot, a fresh
# random receipt key and a fresh random reveal key. Nothing is printed but addresses.
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

chain=(--rpc "$RPC" --registry "$REG" --cast "$FOUNDRY_BIN/cast")
mkdir -m 700 "$work/spool"

# round NAME [observer flags...]: one draw of the fact, its observers and its coordinator.
round() {
  local name=$1; shift
  local ws=$(( $(date +%s) + LEAD ))
  local feed=(--record-date "$date_of" --window-start "$ws")
  mkdir -m 700 "$work/state-$name"
  pids=()
  for i in $(seq 3 9); do
    "$bin/level2-observer" --key-file "$work/keys/observer-$i.key" --pool "$work/keys/pool.json" \
      --spool "$work/spool" --state "$work/state-$name/observer-$i" "${feed[@]}" "${chain[@]}" \
      "$@" >"$logs/$name-observer-$i.log" 2>&1 &
    pids+=($!)
  done
  echo "$name: 7 observer processes started; the window opens at $ws (in ${LEAD}s)"
  local new=()
  [ -f "$work/store.json" ] || new=(--new-store)
  "$bin/level2-coordinator" "${chain[@]}" --coordinator "$COORD" --owner "$OWNER" \
    --reporter "$REPORTER" --book "$work/book.json" --store "$work/store.json" "${new[@]}" \
    --pool "$work/keys/pool.json" --spool "$work/spool" --receipt-key "$work/keys/receipt.key" \
    --reveal-key "$work/keys/reveal.key" "${feed[@]}" >"$logs/$name.json" 2>"$logs/$name-coordinator.log" \
    || { echo "$name: the coordinator failed:"; tail -20 "$logs/$name-coordinator.log"; exit 1; }
  cat "$logs/$name.json"
  local posted=0 undrawn=0 failed=0
  for idx in "${!pids[@]}"; do
    local i=$((idx + 3))
    if wait "${pids[$idx]}"; then
      if grep -q "sealed post on chain" "$logs/$name-observer-$i.log"; then posted=$((posted + 1));
      elif grep -q "not drawn" "$logs/$name-observer-$i.log"; then undrawn=$((undrawn + 1));
      else failed=$((failed + 1)); fi
    else
      failed=$((failed + 1)); echo "$name: observer $i failed:"; tail -5 "$logs/$name-observer-$i.log"
    fi
  done
  pids=()
  echo "$name observers: $posted sealed posts on chain, $undrawn not drawn, $failed failed"
  [ "$posted/$undrawn/$failed" = "5/2/0" ] || { echo "$name: expected 5/2/0"; exit 1; }
}

round draw0 --sealed-garbage
python3 -I - "$logs/draw0.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
assert r["attempt"] == 0 and r["outcome"] == "NoResult", r
assert r["commits_from_chain"] == 5 and r["reveals"] == 0, r
assert r["sealed_posts"] == 5 and len(r["dismissed"]) == 5, r
assert r["recorded_attempt"], r
assert r["level2_claim"] is False
PY
echo "draw 0: 5 garbage posts dismissed with proofs, noResult recorded with recordAttempt"

round draw1
python3 -I - "$logs/draw1.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
assert r["attempt"] == 1 and r["outcome"].startswith("Value(Number("), r
assert r["commits_from_chain"] == 5 and r["reveals"] == 5, r
assert r["sealed_posts"] == 5 and r["dismissed"] == [], r
assert r["recorded_attempt"] is None and r["level2_claim"] is True, r
assert all(o["status"] == "Agreed" for o in r["observers"]), r["observers"]
PY
echo "draw 1: 5 sealed reveals opened, value settled and recorded with recordObservation"

"$bin/level2-verify" "${chain[@]}" --store "$work/store.json" >"$logs/client.json" \
  || { echo "the client refused the served rounds"; exit 1; }
cat "$logs/client.json"
python3 -I - "$logs/client.json" <<'PY'
import json, sys
c = json.load(open(sys.argv[1]))
assert c["client_checks"] == "passed"
[f] = c["facts"]
assert f["attempt"] == 1 and f["value_round_sealed_posts"] == 5, f
[e] = f["earlier_rounds"]
assert e["attempt"] == 0 and e["dismissed"] == 5 and e["sealed_posts"] == 5, e
assert len(f["doctored"]) == 4 and all(d["refused"] for d in f["doctored"]), f["doctored"]
PY
echo "LEVEL-2 SEALED REVEALS REHEARSAL PASSED"
