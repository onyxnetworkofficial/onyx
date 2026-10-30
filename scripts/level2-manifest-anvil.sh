#!/usr/bin/env bash
# CL3 (ADR-0053): a manifest source reaches level 2 on a LOCAL anvil (chain id 31337), with no
# per-source Rust: OnyxRegistry next to a stub ledger, the metric registered from the manifest's
# template (`registerMetric`), the registry's answer checked against the manifest's template id
# (THREATS CL3-8), then one round. Never sends a Base Sepolia transaction.
#
# Usage: scripts/level2-manifest-anvil.sh <demo|live> [manifest]
#   manifest  default sources/us.treasury.tga_total_deposits/manifest.json
#   demo      the local end-to-end demo (about 1 minute): a local fixture server answers the
#             manifest's own URL with its recording; five observers fetch it through onyx_fetch;
#             the recorded round runs on the anvil (draw announced and fixed on chain). No
#             third-party call.
#   live      the live dry run (about 7 minutes): seven INDEPENDENT observer processes (anvil
#             dev accounts 3..9, each its own 0600 key file), each fetching the source itself,
#             live, through onyx_fetch (public addresses only, no redirects, size caps, 20 s);
#             five are drawn; the coordinator receipts and settles on the anvil (1 s blocks).
#
# Local only: anvil's public dev accounts; no key is read from .env or .onyx-testnet. What this
# does NOT show: independence (one host, one network path, one operator; THREATS L2-SRC-1).
#
# Env: FOUNDRY_BIN (~/.foundry/bin), ONYX_L2_RPC_PORT (8551), ONYX_L2_LEAD (seconds until the live
# window opens, 45), ONYX_L2_LOG_DIR (logs and outputs; default a temp dir under ~/.cache that
# is removed), ONYX_L2_RECORD_DATE (live: default `latest`).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
mode=${1:-}
manifest=${2:-sources/us.treasury.tga_total_deposits/manifest.json}
case "$mode" in demo | live) ;; *) echo "usage: $0 <demo|live> [manifest]" >&2; exit 2 ;; esac
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
PORT=${ONYX_L2_RPC_PORT:-8551}
RPC=http://127.0.0.1:$PORT
LEAD=${ONYX_L2_LEAD:-45}
mkdir -p "$HOME/.cache"
work=$(mktemp -d "$HOME/.cache/onyx-cl3-anvil.XXXXXX")
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
# cast and forge run outside the repo: Foundry loads a .env from its working directory (M8-L55-F4).
castw() { (cd "$work" && ETH_RPC_URL=$RPC "$FOUNDRY_BIN/cast" "$@"); }

(cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/build.log" 2>&1) \
  || { tail -20 "$work/build.log"; exit 1; }
artifact=$REPO/contracts/out/OnyxRegistry.sol/OnyxRegistry.json
cargo build -q -p onyx-level2 --example level2-manifest
bin=$work/bin
mkdir "$bin"
cp "${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/level2-manifest" "$bin/"
lm() { "$bin/level2-manifest" "$@" --manifest "$manifest" --repo "$REPO"; }
"$bin/level2-manifest" pin --manifest "$manifest" --repo "$REPO" >"$work/pin" \
  || { echo "the manifest does not load" >&2; exit 1; }
pin=$(cat "$work/pin")
metric=${pin%%=*}
template=${pin#*=}
echo "manifest $manifest: metric $metric, template $template"

anvil_args=(--port "$PORT" --silent --accounts 10 --config-out "$work/accounts.json")
[ "$mode" = live ] && anvil_args+=(--block-time 1)
"$FOUNDRY_BIN/anvil" "${anvil_args[@]}" &
anvil_pid=$!
for _ in $(seq 50); do castw chain-id >/dev/null 2>&1 && break; sleep 0.2; done
[ "$(castw chain-id)" = 31337 ] || { echo "not a local anvil" >&2; exit 1; }

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
echo "OnyxRegistry at $REG on the local anvil (chain 31337)"
chain=(--rpc "$RPC" --registry "$REG" --cast "$FOUNDRY_BIN/cast")

# THREATS CL3-8: after the run registered the metric, the registry holds exactly the manifest's
# template id.
check_registry() {
  local key row
  key=$(castw keccak "$metric")
  row=$(castw call "$REG" "metrics(bytes32)(uint8,bytes32,uint64)" "$key" | sed -n 2p)
  [ "$row" = "$template" ] || { echo "registry holds $row for $metric, not $template" >&2; exit 1; }
  echo "registry: $metric -> $row (the manifest's template id)"
}

if [ "$mode" = demo ]; then
  lm demo "${chain[@]}" --coordinator "$COORD" --owner "$OWNER" \
    --book "$work/book.json" --store "$logs/store.json" --new-store --out "$logs/summary.json" \
    2>"$logs/demo.log" || { tail -20 "$logs/demo.log"; exit 1; }
  cat "$logs/demo.log"
  check_registry
  python3 -I - "$logs/summary.json" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
print(f"value {s['value']} {s['unit']}, level {s['proof_level']}, {s['agreed']}/{s['observers']} agreed")
assert s["proof_level"] == 2 and s["agreed"] == s["observers"] == 5, s
PY
  echo "CL3 LOCAL MANIFEST DEMO PASSED (anvil)"
  exit 0
fi

# live: independent observers, each fetching the source itself.
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
mkdir -m 700 "$work/spool" "$work/state"
for i in $(seq 3 9); do
  lm observer --key-file "$work/keys/observer-$i.key" --pool "$work/keys/pool.json" \
    --spool "$work/spool" --state "$work/state/observer-$i" "${feed[@]}" "${chain[@]}" \
    >"$logs/observer-$i.log" 2>&1 &
  pids+=($!)
done
echo "7 observer processes started; the window opens at $window_start (in ${LEAD}s), about 7 minutes in all"
lm coordinator "${chain[@]}" --coordinator "$COORD" --owner "$OWNER" --reporter "$REPORTER" \
  --book "$work/book.json" --store "$logs/store.json" --new-store --pool "$work/keys/pool.json" \
  --spool "$work/spool" --receipt-key "$work/keys/receipt.key" "${feed[@]}" \
  >"$logs/round.json" 2>"$logs/coordinator.log" \
  || { echo "the coordinator failed:"; tail -20 "$logs/coordinator.log"; exit 1; }
cat "$logs/round.json"
check_registry
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
agreed = sum(1 for o in r["observers"] if o["status"] == "Agreed")
assert agreed >= 3, r["observers"]
assert r["level2_claim"] is True
assert (receipted, undrawn, failed) == (5, 2, 0), (receipted, undrawn, failed)
print(f"outcome {r['outcome']}, {agreed}/5 agreed, level-2 claim accepted")
PY
echo "CL3 LIVE MANIFEST DRY RUN PASSED (local anvil; the source fetched live)"
