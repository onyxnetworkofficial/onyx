#!/usr/bin/env bash
# CL5 (ADR-0059): a large publisher dataset end to end on a local anvil, with anvil dev keys and a
# freshly generated TEST publisher key only. Synthetic data only (onyx-publish synth).
#
#   scripts/bulk-demo.sh local
#
# Steps: start its own anvil (port picked by anvil, stopped on any exit); deploy the settlement and
# OnyxSignedBook (reviewer: dev account 1) and pin it in a manifest; make a publisher key and its
# keys document; the reviewer registers the key; generate BULK_DEMO_MIB MiB (default 100) of
# synthetic CSV rows; build shards and the manifest; sign; anchor (one postReport); mine past
# anvil's finalized lag; start onyx-node with ONYX_BULK_DIR; upload; query ranges and check every
# row with onyx-check --bulk against the book at the client's own finalized block; then the
# attacks (THREATS CL5-1..7): a forged row, a forged shard, an unanchored swapped manifest, a key
# the book does not know, a compromised key and its recovery; an appended version 2; a node restart.
# Prints measured sizes and times, then BULK DEMO LOCAL PASSED.
# Env: BULK_DEMO_MIB (100), BULK_DEMO_SHARD_MIB (8), FOUNDRY_BIN (~/.foundry/bin), CARGO_TARGET_DIR.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
[ "${1:-}" = local ] || { echo "usage: $0 local" >&2; exit 2; }
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
target=${CARGO_TARGET_DIR:-$REPO/target}
mib=${BULK_DEMO_MIB:-100}
shard_mib=${BULK_DEMO_SHARD_MIB:-8}
work=$(mktemp -d)
anvil_pid="" node_pid=""
stop() { [ -n "$1" ] || return 0; kill "$1" 2>/dev/null || true; wait "$1" 2>/dev/null || true; }
cleanup() { stop "$node_pid"; stop "$anvil_pid"; rm -rf "$work"; }
trap cleanup EXIT
trap 'exit 130' INT TERM HUP
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }
now() { date +%s.%N; }
since() { python3 -I -c 'import sys; print(f"{float(sys.argv[2]) - float(sys.argv[1]):.1f}")' "$1" "$(now)"; }

cargo build --locked -q -p onyx-node --bin onyx-node --example pin-signed-book
cargo build --locked -q -p onyx-publish -p onyx-check
pub_bin=$target/debug/onyx-publish
check_bin=$target/debug/onyx-check
(cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/forge.log" 2>&1) \
  || { cat "$work/forge.log"; fail "forge build"; }

(cd "$work" && exec "$FOUNDRY_BIN/anvil" --host 127.0.0.1 --port 0 --config-out "$work/anvil.json" \
  >"$work/anvil.log" 2>&1) &
anvil_pid=$!
port=""
for _ in $(seq 600); do
  kill -0 "$anvil_pid" 2>/dev/null || { cat "$work/anvil.log"; fail "anvil exited"; }
  port=$(grep -oE 'Listening on 127\.0\.0\.1:[0-9]+' "$work/anvil.log" | head -n1 | sed 's/.*://' || true)
  [ -n "$port" ] && [ -s "$work/anvil.json" ] && break
  sleep 0.1
done
[ -n "$port" ] || fail "anvil did not report its port"
RPC=http://127.0.0.1:$port
castw() { (cd "$work" && ETH_RPC_URL=$RPC "$FOUNDRY_BIN/cast" "$@"); }
chain=$(castw chain-id)
[ "$chain" = 31337 ] || fail "anvil answers chain $chain"
acct() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["available_accounts"][int(sys.argv[2])].lower())' "$work/anvil.json" "$1"; }
deployer=$(acct 0) reviewer=$(acct 1)

echo "1. settlement and signed book (reviewer: dev account 1), pinned in a manifest"
cp -a "$REPO/contracts" "$work/contracts"
rm -rf "$work/contracts/broadcast"
sed -i "s#$REPO/contracts#$work/contracts#g" "$work/contracts/cache/solidity-files-cache.json"
key0=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["private_keys"][0])' "$work/anvil.json")
(cd "$work/contracts" && BASE_SEPOLIA_PRIVATE_KEY=$key0 ETH_RPC_URL=$RPC \
  "$FOUNDRY_BIN/forge" script script/Deploy.s.sol:Deploy --rpc-url "$RPC" --broadcast --slow \
  >"$work/deploy.log" 2>&1) || { tail -20 "$work/deploy.log"; fail "settlement deploy"; }
unset key0
settlement=$(python3 -I -c 'import json,sys
v=json.load(open(sys.argv[1]))
print(next(t["contractAddress"] for t in v["transactions"] if t["contractName"]=="OnyxSettlement" and t["transactionType"]=="CREATE").lower())' \
  "$work/contracts/broadcast/Deploy.s.sol/$chain/run-latest.json")
manifest=$work/deployment.json
python3 -I - "$manifest" "$chain" "$settlement" "$(castw codehash "$settlement")" <<'PY'
import json, sys
path, chain, st, h = sys.argv[1:]
json.dump({"chainId": int(chain), "settlement": st, "settlementCodeHash": h}, open(path, "w"), indent=2)
PY
ONYX_PIN_BIN=$target/debug/examples/pin-signed-book scripts/deploy-signed-book.sh anvil \
  --rpc "$RPC" --anvil-config "$work/anvil.json" --reviewer "$reviewer" --manifest "$manifest" \
  >"$work/pin.log" 2>&1 || { cat "$work/pin.log"; fail "deploy-signed-book.sh"; }
book=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["signedBook"])' "$manifest")
pass "book $book"

echo "2. a fresh TEST publisher key, its keys document, registered by the reviewer"
publisher=synthetic.publisher
origin=publisher.example.org
"$pub_bin" keygen --out "$work/publisher.key" >"$work/keygen.log"
signer=$("$pub_bin" address --key "$work/publisher.key")
"$pub_bin" keys-doc --key "$work/publisher.key" --publisher "$publisher" --out "$work/onyx-source-keys.json" 2>"$work/doc.log" >/dev/null
provenance=$(grep -oE '0x[0-9a-f]{64}' "$work/doc.log")
castw send --unlocked --from "$reviewer" "$book" 'registerKey(string,address,string,bytes32,string)' \
  "$publisher" "$signer" "$origin" "$provenance" "CL5 demo: keys document reviewed" >/dev/null
castw send --unlocked --from "$deployer" "$signer" --value 1ether >/dev/null
pass "publisher $publisher, signer $signer registered under $origin (keys document $provenance)"

echo "3. ${mib} MiB of synthetic rows, shards, manifest, signature"
t=$(now)
"$pub_bin" synth --out "$work/data.csv" --schema-out "$work/schema.json" --mib "$mib" | tee "$work/synth.log"
synth_secs=$(since "$t")
created=$(date +%s)
t=$(now)
"$pub_bin" build --schema "$work/schema.json" --input "$work/data.csv" --dataset synthetic.chain-metrics \
  --publisher "$publisher" --license CC0-1.0 --out "$work/v1" --created "$created" --shard-mib "$shard_mib" \
  | tee "$work/build.log"
build_secs=$(since "$t")
"$pub_bin" sign --key "$work/publisher.key" --dir "$work/v1" | tee "$work/sign.log"
rows=$(grep -oE 'rows=[0-9]+' "$work/build.log" | head -1 | cut -d= -f2)
shards=$(grep -oE ' shards=[0-9]+' "$work/build.log" | cut -d= -f2)
shard_bytes=$(grep -oE 'bytes=[0-9]+' "$work/build.log" | cut -d= -f2)
csv_bytes=$(stat -c %s "$work/data.csv")
[ "$csv_bytes" -ge $((mib << 20)) ] || fail "the input is under ${mib} MiB"

echo "4. anchor: one postReport in the book"
t=$(now)
"$pub_bin" anchor --dir "$work/v1" --rpc "$RPC" --chain-id 31337 --book "$book" --gas-key "$work/publisher.key" \
  | tee "$work/anchor.log"
anchor_secs=$(since "$t")
anchor_gas=$(grep -oE 'gas=[0-9]+' "$work/anchor.log" | cut -d= -f2)
grep -q '^ANCHORED tx=' "$work/anchor.log" || fail "anchor"
# THREATS CL5-3: anchoring the same report again sends nothing.
"$pub_bin" anchor --dir "$work/v1" --rpc "$RPC" --chain-id 31337 --book "$book" --gas-key "$work/publisher.key" \
  | grep -q '^ANCHORED already' || fail "a second anchor of the same report was sent"
pass "replayed anchor: nothing sent"

echo "5. onyx-node with ONYX_BULK_DIR"
mkdir -p "$work/cas" "$work/state"
printf '{"version": 1, "shards": []}\n' >"$work/catalog.json"
free_port() { python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'; }
node_port=$(free_port)
NODE="http://127.0.0.1:$node_port"
start_node() {
  env -i PATH="$PATH" HOME="$HOME" ONYX_ENV=dev ONYX_RIGHTS=cleared ONYX_PAYMENTS=off \
    ONYX_LISTEN="127.0.0.1:$node_port" ONYX_PUBLIC_URL="$NODE/" ONYX_RPC_URL="$RPC" \
    ONYX_DEPLOYMENT="$manifest" ONYX_CATALOG="$work/catalog.json" ONYX_CAS_DIR="$work/cas" \
    ONYX_STATE_DIR="$work/state" ONYX_BULK_DIR="$work/bulk" "$target/debug/onyx-node" >>"$work/node.log" 2>&1 &
  node_pid=$!
  for _ in $(seq 300); do
    kill -0 "$node_pid" 2>/dev/null || { cat "$work/node.log"; fail "onyx-node exited"; }
    curl -sf "$NODE/v1/health" >/dev/null 2>&1 && return 0
    sleep 0.2
  done
  fail "onyx-node did not come up"
}
# The node reads the settlement at its finalized block (anvil: 64 blocks behind the head).
castw rpc anvil_mine 70 >/dev/null
start_node
grep -q 'bulk datasets on' "$work/node.log" || { cat "$work/node.log"; fail "bulk not on"; }
pass "onyx-node up on $NODE"

echo "6. upload"
t=$(now)
"$pub_bin" upload --dir "$work/v1" --node "$NODE" | tee "$work/upload.log"
upload_secs=$(since "$t")
curl -sf "$NODE/v1/bulk" >"$work/list.json"
python3 -I - "$work/list.json" "$rows" "$shards" <<'PY' || fail "listing"
import json, sys
v = json.load(open(sys.argv[1]))["datasets"][0]
assert v["rows"] == int(sys.argv[2]) and v["stored_shards"] == v["shards"] == int(sys.argv[3]), v
PY
pass "all $shards shards stored; the listing counts $rows rows"

echo "7. range queries, each row checked by onyx-check against the book at the client's finalized block"
first=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["datasets"][0]["key_min"])' "$work/list.json")
last=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["datasets"][0]["key_max"])' "$work/list.json")
# A range in the middle of the dataset, crossing a shard boundary when there are several shards.
mid=$(( (first + last) / 2 ))
from=$((mid - 6000)) to=$((mid + 6000))
query_secs=""
checked=0
for page in 0 1; do
  t=$(now)
  curl -sf "$NODE/v1/bulk/$publisher/synthetic.chain-metrics/rows?from=$from&to=$to&limit=1000&page=$page" >"$work/rows$page.json"
  query_secs="$query_secs $(since "$t")"
  "$check_bin" --publisher "$publisher" --dataset synthetic.chain-metrics --bulk "$work/rows$page.json" --from "$from" --to "$to" --rpc "$RPC" --chain-id 31337 --book "$book" \
    >"$work/check$page.json" || { cat "$work/check$page.json"; fail "onyx-check refused page $page"; }
  n=$(python3 -I -c 'import json,sys; r=json.load(open(sys.argv[1])); assert r["ok"] and r["chainChecked"]; print(r["rows"])' "$work/check$page.json")
  checked=$((checked + n))
done
answer_bytes=$(stat -c %s "$work/rows0.json")
pass "$checked rows in [$from, $to] proven (pages of 1000; first page $answer_bytes bytes)"

echo "8. attacks"
# CL5-1/answer: one changed byte in a served row.
python3 -I - "$work/rows0.json" "$work/forged.json" <<'PY'
import json, sys
a = json.load(open(sys.argv[1]))
r = a["rows"][3]["row"]
a["rows"][3]["row"] = r[:-4] + ("0" if r[-4] != "0" else "1") + r[-3:]
json.dump(a, open(sys.argv[2], "w"))
PY
if "$check_bin" --publisher "$publisher" --dataset synthetic.chain-metrics --bulk "$work/forged.json" --from "$from" --to "$to" --rpc "$RPC" --chain-id 31337 --book "$book" >"$work/forged.out"; then
  fail "onyx-check accepted a forged row"
fi
pass "forged row refused: $(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["error"])' "$work/forged.out")"
# CL5-1/upload: random bytes under a root that is in no manifest.
code=$(head -c 4096 /dev/urandom | curl -s -o "$work/put.json" -w '%{http_code}' -X PUT --data-binary @- \
  "$NODE/v1/bulk/$publisher/synthetic.chain-metrics/shards/0x$(printf 'ab%.0s' $(seq 32))")
[ "$code" = 404 ] || { cat "$work/put.json"; fail "a forged shard got $code"; }
pass "forged shard refused (404 bulk_unknown_shard)"
# CL5-2: a swapped version 1 (other rows), signed by the same key but never anchored.
head -n 1000 "$work/data.csv" >"$work/swap.csv"
"$pub_bin" build --schema "$work/schema.json" --input "$work/swap.csv" --dataset synthetic.chain-metrics \
  --publisher "$publisher" --license CC0-1.0 --out "$work/swap" --created "$((created + 1))" >/dev/null
"$pub_bin" sign --key "$work/publisher.key" --dir "$work/swap" >/dev/null
if "$pub_bin" upload --dir "$work/swap" --node "$NODE" >"$work/swap.log" 2>&1; then fail "an unanchored manifest was taken"; fi
grep -q bulk_not_anchored "$work/swap.log" || { cat "$work/swap.log"; fail "swap refused for another reason"; }
pass "unanchored swapped manifest refused (409 bulk_not_anchored)"
# CL5-3: a key the book never registered cannot anchor.
"$pub_bin" keygen --out "$work/stranger.key" >/dev/null
castw send --unlocked --from "$deployer" "$("$pub_bin" address --key "$work/stranger.key")" --value 1ether >/dev/null
"$pub_bin" sign --key "$work/stranger.key" --dir "$work/swap" >/dev/null
if "$pub_bin" anchor --dir "$work/swap" --rpc "$RPC" --chain-id 31337 --book "$book" --gas-key "$work/stranger.key" >"$work/stranger.log" 2>&1; then
  fail "an unregistered key anchored a manifest"
fi
pass "unregistered key: postReport would revert, nothing sent"

echo "9. version 2 appends rows (one more postReport)"
python3 -I - "$last" "$work/more.csv" <<'PY'
import sys
last, out = int(sys.argv[1]), sys.argv[2]
with open(out, "w") as f:
    f.write("t,series,value,count\n")
    for i in range(1, 2001):
        f.write(f"{last + 12 * i},synthetic-{i % 5},{i}.00000001,{i}\n")
PY
"$pub_bin" build --schema "$work/schema.json" --input "$work/more.csv" --dataset synthetic.chain-metrics \
  --publisher "$publisher" --license CC0-1.0 --out "$work/v2" --prev "$work/v1" --created "$((created + 2))" | tee "$work/build2.log"
"$pub_bin" sign --key "$work/publisher.key" --dir "$work/v2" >/dev/null
"$pub_bin" anchor --dir "$work/v2" --rpc "$RPC" --chain-id 31337 --book "$book" --gas-key "$work/publisher.key" >"$work/anchor2.log"
castw rpc anvil_mine 70 >/dev/null
"$pub_bin" upload --dir "$work/v2" --node "$NODE" | tee "$work/upload2.log"
grep -q 'shards_sent=1 ' "$work/upload2.log" || fail "version 2 should send only its one new shard"
curl -sf "$NODE/v1/bulk/$publisher/synthetic.chain-metrics/rows?from=$((last - 24))&to=$((last + 36))" >"$work/rows-v2.json"
"$check_bin" --publisher "$publisher" --dataset synthetic.chain-metrics --bulk "$work/rows-v2.json" --from "$((last - 24))" --to "$((last + 36))" --rpc "$RPC" --chain-id 31337 --book "$book" \
  >"$work/check-v2.json" || { cat "$work/check-v2.json"; fail "version 2 rows"; }
python3 -I -c 'import json,sys; r=json.load(open(sys.argv[1])); assert r["version"] == 2 and r["rows"] == 6, r' "$work/check-v2.json"
pass "version 2 served: 6 rows across the v1/v2 boundary proven"

echo "10. restart: the node reloads manifests and tiers"
stop "$node_pid"; node_pid=""
start_node
curl -sf "$NODE/v1/bulk/$publisher/synthetic.chain-metrics/rows?from=$from&to=$((from + 50))" >"$work/rows-r.json"
"$check_bin" --publisher "$publisher" --dataset synthetic.chain-metrics --bulk "$work/rows-r.json" --from "$from" --to "$((from + 50))" --rpc "$RPC" --chain-id 31337 --book "$book" >/dev/null \
  || fail "after restart"
pass "served and proven after a restart"

echo "11. CL5-6: the reviewer marks the key compromised; the client refuses at once"
key_id=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["anchor"]["key"])' "$work/rows0.json")
castw send --unlocked --from "$reviewer" "$book" 'markCompromised(bytes32,uint64,string)' "$key_id" 0 "CL5 demo" >/dev/null
castw rpc anvil_mine 70 >/dev/null
if "$check_bin" --publisher "$publisher" --dataset synthetic.chain-metrics --bulk "$work/rows0.json" --from "$from" --to "$to" --rpc "$RPC" --chain-id 31337 --book "$book" >"$work/comp.json"; then
  fail "a row signed by a compromised key was accepted"
fi
pass "compromised key: $(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["error"])' "$work/comp.json")"

echo "12. recovery: a new key, registered by the reviewer, signs version 2 again; the node re-anchors it"
"$pub_bin" keygen --out "$work/publisher2.key" >/dev/null
signer2=$("$pub_bin" address --key "$work/publisher2.key")
castw send --unlocked --from "$reviewer" "$book" 'registerKey(string,address,string,bytes32,string)' \
  "$publisher" "$signer2" "$origin" "$provenance" "CL5 demo: rotation after a compromise" >/dev/null
castw send --unlocked --from "$deployer" "$signer2" --value 1ether >/dev/null
"$pub_bin" sign --key "$work/publisher2.key" --dir "$work/v2" >/dev/null
"$pub_bin" anchor --dir "$work/v2" --rpc "$RPC" --chain-id 31337 --book "$book" --gas-key "$work/publisher2.key" >/dev/null
castw rpc anvil_mine 70 >/dev/null
"$pub_bin" upload --dir "$work/v2" --node "$NODE" >"$work/upload3.log"
curl -sf "$NODE/v1/bulk/$publisher/synthetic.chain-metrics/rows?from=$from&to=$((from + 50))" >"$work/rows-new.json"
"$check_bin" --publisher "$publisher" --dataset synthetic.chain-metrics --bulk "$work/rows-new.json" --from "$from" --to "$((from + 50))" --rpc "$RPC" --chain-id 31337 --book "$book" \
  >"$work/check-new.json" || { cat "$work/check-new.json"; fail "after the key rotation"; }
python3 -I -c 'import json,sys; r=json.load(open(sys.argv[1])); assert r["signer"] == sys.argv[2], r' "$work/check-new.json" "$signer2"
pass "served again under the new key $signer2"

echo
echo "MEASURED (local anvil, debug build)"
echo "  input CSV:        $csv_bytes bytes ($mib MiB asked), synthetic in ${synth_secs}s"
echo "  rows:             $rows in $shards shards, $shard_bytes shard bytes (target $shard_mib MiB each)"
echo "  build (check, shard, Merkle, manifest): ${build_secs}s"
echo "  anchor:           1 postReport, gas $anchor_gas, ${anchor_secs}s"
echo "  upload:           ${upload_secs}s ($(grep -oE 'bytes_sent=[0-9]+' "$work/upload.log"))"
echo "  range queries:    2 pages of 1000 rows in${query_secs} s; $checked rows proven by onyx-check"
echo "BULK DEMO LOCAL PASSED"
