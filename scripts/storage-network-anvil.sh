#!/usr/bin/env bash
# ADR-0060 (docs/STORAGE-NETWORK.md): the storage network end to end on a local anvil, with anvil
# dev keys and a freshly generated TEST publisher key only. Synthetic data only.
#
#   scripts/storage-network-anvil.sh
#
# Steps: its own anvil (port picked by anvil, stopped on any exit); the settlement and
# OnyxSignedBook (as scripts/bulk-demo.sh); a publisher dataset built, signed and anchored, served
# by the publisher's own node; a second dataset with no report of its own, anchored by catalog 0
# and listed unchanged by catalog 1 (ADR-0067); tONYX and OnyxStorage (epochs of
# STORAGE_EPOCH_BLOCKS blocks); five storage nodes register with a bond and run onyx-node in
# storage mode; a deal with R=3 on the first dataset (createDeal, its own report) and one on the
# second (createDealInCatalog, from the proof of the newest catalog that the publisher's node
# serves; a catalog that does not list a manifest and the old catalog 0 are refused by the tool
# before anything is sent, and the old catalog is refused by the contract too); the
# nodes fetch and check their shards (the catalog dataset from the carried proof: they hold no
# catalog); reads from the replicas, and rows of the catalog dataset from a storage node pass
# onyx-check; an epoch of challenges every node passes; one node killed (reads keep working) and
# one node that dropped its shards (a forged row it sends is refused on chain); misses settled and
# slashed (the keeper earns 10% of each); repair to new replicas; the catalog deal runs to its
# end and is closed (the unpaid deposit back to the payer); the books balanced on chain
# (conservation) and rebuilt from the contract's events; the declines report shows the slashes on
# the two misbehaving nodes only.
# Prints what it measured, then STORAGE NETWORK DEMO PASSED.
# Env: STORAGE_DEMO_MIB (4), STORAGE_EPOCH_BLOCKS (40), FOUNDRY_BIN (~/.foundry/bin),
# CARGO_TARGET_DIR.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
target=${CARGO_TARGET_DIR:-$REPO/target}
mib=${STORAGE_DEMO_MIB:-4}
EB=${STORAGE_EPOCH_BLOCKS:-40}
work=$(mktemp -d)
anvil_pid=""
declare -a node_pids=()
pub_pid=""
stop() { [ -n "${1:-}" ] || return 0; kill "$1" 2>/dev/null || true; wait "$1" 2>/dev/null || true; }
cleanup() {
  for p in "${node_pids[@]}"; do stop "$p"; done
  stop "$pub_pid"; stop "$anvil_pid"; rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP
fail() { echo "FAIL: $*" >&2; for i in 0 1 2 3 4; do [ -f "$work/node$i.log" ] && { echo "--- node$i.log (tail)" >&2; tail -n 15 "$work/node$i.log" >&2; }; done; exit 1; }
pass() { echo "  ok: $*"; }
now() { date +%s.%N; }
since() { python3 -I -c 'import sys; print(f"{float(sys.argv[2]) - float(sys.argv[1]):.1f}")' "$1" "$(now)"; }
t_start=$(now)

# --workspace (not -p): the same feature set as check-all's `cargo test --workspace`, so these
# targets reuse its build instead of compiling a second variant of the shared crates.
cargo build --locked -q --workspace --bin onyx-node --bin onyx-storage --example pin-signed-book \
  --bin onyx-publish --bin onyx-check
pub_bin=$target/debug/onyx-publish
check_bin=$target/debug/onyx-check
st_bin=$target/debug/onyx-storage
(cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/forge.log" 2>&1) \
  || { cat "$work/forge.log"; fail "forge build"; }

(cd "$work" && exec "$FOUNDRY_BIN/anvil" --host 127.0.0.1 --port 0 --config-out "$work/anvil.json" \
  >"$work/anvil.log" 2>&1) &
anvil_pid=$!
port=""
for _ in $(seq 600); do
  kill -0 "$anvil_pid" 2>/dev/null || { cat "$work/anvil.log"; fail "anvil exited"; }
  port=$(grep -oE 'Listening on 127\.0\.0\.1:[0-9]+' "$work/anvil.log" | head -n1 | sed 's/.*://' || true)  # pipefail-ok: anvil's log is a few lines, and the status is dropped (|| true)
  [ -n "$port" ] && [ -s "$work/anvil.json" ] && break
  sleep 0.1
done
[ -n "$port" ] || fail "anvil did not report its port"
RPC=http://127.0.0.1:$port
castw() { (cd "$work" && ETH_RPC_URL=$RPC "$FOUNDRY_BIN/cast" "$@"); }
chain=$(castw chain-id)
[ "$chain" = 31337 ] || fail "anvil answers chain $chain"
acct() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["available_accounts"][int(sys.argv[2])].lower())' "$work/anvil.json" "$1"; }
pkey() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["private_keys"][int(sys.argv[2])])' "$work/anvil.json" "$1"; }
deployer=$(acct 0) reviewer=$(acct 1) treasury=$(acct 9)

echo "1. settlement and signed book (reviewer: dev account 1), pinned in a manifest"
cp -a "$REPO/contracts" "$work/contracts"
rm -rf "$work/contracts/broadcast"
sed -i "s#$REPO/contracts#$work/contracts#g" "$work/contracts/cache/solidity-files-cache.json"
key0=$(pkey 0)
(cd "$work/contracts" && BASE_SEPOLIA_PRIVATE_KEY=$key0 ETH_RPC_URL=$RPC \
  "$FOUNDRY_BIN/forge" script script/Deploy.s.sol:Deploy --rpc-url "$RPC" --broadcast --slow \
  >"$work/deploy.log" 2>&1) || { tail -20 "$work/deploy.log"; fail "settlement deploy"; }
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

echo "2. a publisher dataset: ${mib} MiB of synthetic rows, shards of 1 MiB, signed and anchored"
publisher=synthetic.publisher
dataset=synthetic.storage-demo
"$pub_bin" keygen --out "$work/publisher.key" >/dev/null
signer=$("$pub_bin" address --key "$work/publisher.key")
"$pub_bin" keys-doc --key "$work/publisher.key" --publisher "$publisher" --out "$work/keys.json" 2>"$work/doc.log" >/dev/null
provenance=$(grep -oE '0x[0-9a-f]{64}' "$work/doc.log")
castw send --unlocked --from "$reviewer" "$book" 'registerKey(string,address,string,bytes32,string)' \
  "$publisher" "$signer" "publisher.example.org" "$provenance" "storage demo: keys document reviewed" >/dev/null
castw send --unlocked --from "$deployer" "$signer" --value 1ether >/dev/null
"$pub_bin" synth --out "$work/data.csv" --schema-out "$work/schema.json" --mib "$mib" >/dev/null
"$pub_bin" build --schema "$work/schema.json" --input "$work/data.csv" --dataset "$dataset" \
  --publisher "$publisher" --license CC0-1.0 --out "$work/v1" --created "$(date +%s)" --shard-mib 1 >"$work/build.log"
"$pub_bin" sign --key "$work/publisher.key" --dir "$work/v1" >/dev/null
"$pub_bin" anchor --dir "$work/v1" --rpc "$RPC" --chain-id 31337 --book "$book" --gas-key "$work/publisher.key" >"$work/anchor.log"
grep -q '^ANCHORED tx=' "$work/anchor.log" || fail "anchor"
shards=$(grep -oE ' shards=[0-9]+' "$work/build.log" | cut -d= -f2)
rows=$(grep -oE 'rows=[0-9]+' "$work/build.log" | head -1 | cut -d= -f2)  # pipefail-ok: the build prints a few lines
pass "$rows rows in $shards shards, anchored"

echo "2b. a second dataset with no report of its own: one catalog of two leaves anchors it (ADR-0067)"
cat_dataset=synthetic.storage-catalog
"$pub_bin" synth --out "$work/cat.csv" --schema-out "$work/cat-schema.json" --mib 2 >/dev/null
: >"$work/catalog-dirs.txt"
for d in "$cat_dataset:c1" "synthetic.storage-other:c2"; do
  "$pub_bin" build --schema "$work/cat-schema.json" --input "$work/cat.csv" --dataset "${d%%:*}" \
    --publisher "$publisher" --license CC0-1.0 --out "$work/${d##*:}" --created "$(date +%s)" --shard-mib 1 \
    >"$work/build-${d##*:}.log"
  echo "$work/${d##*:}" >>"$work/catalog-dirs.txt"
done
[ ! -e "$work/c1/report.bin" ] || fail "the catalog dataset has a report of its own"
"$pub_bin" catalog-build --publisher "$publisher" --dirs "$work/catalog-dirs.txt" --out "$work/catalog0.bin" >"$work/catalog.log"
grep -q 'number=0 leaves=2' "$work/catalog.log" || { cat "$work/catalog.log"; fail "catalog 0 of two datasets"; }
"$pub_bin" catalog-sign --key "$work/publisher.key" --catalog "$work/catalog0.bin" >/dev/null
"$pub_bin" catalog-anchor --catalog "$work/catalog0.bin" --rpc "$RPC" --chain-id 31337 --book "$book" \
  --gas-key "$work/publisher.key" >"$work/catalog-anchor.log" || { cat "$work/catalog-anchor.log"; fail "catalog anchor"; }
# The next day's catalog: one more dataset, the first two unchanged. From now on catalog 0 is an
# old catalog, and a deal must go through catalog 1.
"$pub_bin" build --schema "$work/cat-schema.json" --input "$work/cat.csv" --dataset synthetic.storage-third \
  --publisher "$publisher" --license CC0-1.0 --out "$work/c3" --created "$(date +%s)" --shard-mib 1 >"$work/build-c3.log"
echo "$work/c3" >"$work/catalog-dirs-1.txt"
"$pub_bin" catalog-build --publisher "$publisher" --dirs "$work/catalog-dirs-1.txt" --prev "$work/catalog0.bin" \
  --out "$work/catalog1.bin" >"$work/catalog1.log"
grep -q 'number=1 leaves=3' "$work/catalog1.log" || { cat "$work/catalog1.log"; fail "catalog 1 of three datasets"; }
"$pub_bin" catalog-sign --key "$work/publisher.key" --catalog "$work/catalog1.bin" >/dev/null
"$pub_bin" catalog-anchor --catalog "$work/catalog1.bin" --rpc "$RPC" --chain-id 31337 --book "$book" \
  --gas-key "$work/publisher.key" >"$work/catalog1-anchor.log" || { cat "$work/catalog1-anchor.log"; fail "catalog 1 anchor"; }
cat_shards=$(grep -oE ' shards=[0-9]+' "$work/build-c1.log" | cut -d= -f2)
cat_root=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["root"].lower())' "$work/c1/manifest.json")
# Nodes read the book at their finalized block (anvil: 64 behind the head).
castw rpc anvil_mine 70 >/dev/null
pass "$cat_dataset: $cat_shards shards, anchored only as a leaf of catalog 0; catalog 1 lists it unchanged"

free_port() { python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'; }
printf '{"version": 1, "shards": []}\n' >"$work/catalog.json"
# start_node NAME PORT [extra env...]: an onyx-node with its own state and bulk store.
start_node() {
  local name=$1 p=$2; shift 2
  mkdir -p "$work/$name/cas" "$work/$name/state"
  env -i PATH="$PATH" HOME="$HOME" ONYX_ENV=dev ONYX_RIGHTS=cleared ONYX_PAYMENTS=off \
    ONYX_LISTEN="127.0.0.1:$p" ONYX_PUBLIC_URL="http://127.0.0.1:$p/" ONYX_RPC_URL="$RPC" \
    ONYX_DEPLOYMENT="$manifest" ONYX_CATALOG="$work/catalog.json" ONYX_CAS_DIR="$work/$name/cas" \
    ONYX_STATE_DIR="$work/$name/state" ONYX_BULK_DIR="$work/$name/bulk" \
    ONYX_BULK_FREE_PER_MINUTE=100000 "$@" \
    "$target/debug/onyx-node" >>"$work/$name.log" 2>&1 &
  started=$!
  for _ in $(seq 300); do
    kill -0 "$started" 2>/dev/null || { cat "$work/$name.log"; fail "onyx-node $name exited"; }
    curl -sf "http://127.0.0.1:$p/v1/health" >/dev/null 2>&1 && return 0
    sleep 0.2
  done
  fail "onyx-node $name did not come up"
}

echo "3. the publisher's own node serves the dataset (the deal's source)"
pub_port=$(free_port)
PUB="http://127.0.0.1:$pub_port"
start_node publisher "$pub_port"
pub_pid=$started
"$pub_bin" upload --dir "$work/v1" --node "$PUB" >"$work/upload.log"
"$pub_bin" catalog-upload --catalog "$work/catalog0.bin" --node "$PUB" >/dev/null || fail "catalog upload"
"$pub_bin" upload --dir "$work/c1" --node "$PUB" --catalog 0 >"$work/upload-c1.log" || fail "upload through catalog 0"
# The manifest answer of the catalog dataset carries the catalog header and the leaf proof.
curl -sf "$PUB/v1/bulk/manifests/$cat_root" >"$work/manifest-answer.json" || fail "manifest answer"
python3 -I -c 'import json,sys; c=json.load(open(sys.argv[1]))["catalog"]; assert c["number"] == 0 and c["header"] and c["leaf"], c' \
  "$work/manifest-answer.json" || fail "the manifest answer carries no catalog proof"
# The node takes catalog 1. The version stays held under catalog 0 (a rows answer says so), but
# the manifest answer now proves it from catalog 1, the newest: the one a deal needs.
"$pub_bin" catalog-upload --catalog "$work/catalog1.bin" --node "$PUB" >/dev/null || fail "catalog 1 upload"
curl -sf "$PUB/v1/bulk/manifests/$cat_root" >"$work/manifest-answer.json" || fail "manifest answer"
python3 -I -c 'import json,sys; c=json.load(open(sys.argv[1]))["catalog"]; assert c["number"] == 1, c' \
  "$work/manifest-answer.json" || fail "the manifest answer does not prove from the newest catalog"
curl -sf "$PUB/v1/bulk/$publisher/$cat_dataset/rows?limit=5" >"$work/rows-pub.json" || fail "rows from the publisher"
python3 -I -c 'import json,sys; a=json.load(open(sys.argv[1]))["anchor"]; assert a["catalog"]["number"] == 0, a' "$work/rows-pub.json" \
  || fail "the publisher's rows answer left the catalog the version is held under"
pass "uploaded to $PUB; the catalog dataset's manifest answer carries its leaf proof, from the newest catalog (1)"

echo "4. tONYX and OnyxStorage (epochs of $EB blocks, R up to 8, 2 challenge slots per deal and epoch)"
(cd "$work/contracts" && "$FOUNDRY_BIN/forge" create --rpc-url "$RPC" --unlocked --from "$deployer" --broadcast \
  src/OnyxTestToken.sol:OnyxTestToken --constructor-args "$deployer" >"$work/token.log" 2>&1) \
  || { cat "$work/token.log"; fail "token deploy"; }
token=$(grep -oE 'Deployed to: 0x[0-9a-fA-F]{40}' "$work/token.log" | awk '{print tolower($3)}')
(cd "$work/contracts" && BASE_SEPOLIA_PRIVATE_KEY=$key0 ONYX_STORAGE_TOKEN=$token ONYX_SIGNED_BOOK=$book \
  ONYX_STORAGE_TREASURY=$treasury ONYX_STORAGE_EPOCH_BLOCKS=$EB ONYX_STORAGE_MIN_BOND=1000000000000000000 \
  "$FOUNDRY_BIN/forge" script script/DeployStorage.s.sol:DeployStorage --rpc-url "$RPC" --broadcast \
  >"$work/deploy-storage.log" 2>&1) || { tail -20 "$work/deploy-storage.log"; fail "OnyxStorage deploy"; }
unset key0
storage=$(python3 -I -c 'import json,sys
v=json.load(open(sys.argv[1]))
print(next(t["contractAddress"] for t in v["transactions"] if t["contractName"]=="OnyxStorage").lower())' \
  "$work/contracts/broadcast/DeployStorage.s.sol/$chain/run-latest.json")
# The gas of the deployment (one transaction: OnyxStorage, which creates OnyxStorageAnchor).
deploy_gas=$(python3 -I -c 'import json,sys
v=json.load(open(sys.argv[1]))
print(sum(int(r["gasUsed"], 16) for r in v["receipts"]))' \
  "$work/contracts/broadcast/DeployStorage.s.sol/$chain/run-latest.json")
genesis=$(castw call "$storage" 'genesis()(uint256)' | awk '{print $1}')
size=$(castw codesize "$storage")
[ "$size" -le 24576 ] || fail "OnyxStorage is $size bytes, over EIP-170"
pass "OnyxStorage $storage ($size bytes of code), genesis block $genesis"
S=(--rpc "$RPC" --contract "$storage" --from-block "$genesis")
SK() { echo --rpc "$RPC" --contract "$storage" --from-block "$genesis" --chain-id 31337 --key "$1"; }

echo "5. five storage nodes: bond, register, activate, start onyx-node in storage mode"
declare -a ids=() ports=() urls=() ops=() keys=()
for i in 0 1 2 3 4; do
  a=$((i + 2))
  ops[i]=$(acct "$a")
  keys[i]=$work/op$i.key
  (umask 077; pkey "$a" >"${keys[i]}")
  castw send --unlocked --from "$deployer" "$token" 'mint(address,uint256)' "${ops[i]}" 1000000000000000000000 >/dev/null
  ports[i]=$(free_port)
  urls[i]="http://127.0.0.1:${ports[i]}"
  # shellcheck disable=SC2046 # the option list is split on purpose
  out=$("$st_bin" register $(SK "${keys[i]}") --capacity-gib 100 --url "${urls[i]}/") || fail "register node $i"
  ids[i]=$(grep -oE 'NODE_ID=0x[0-9a-f]{64}' <<<"$out" | cut -d= -f2)
  [ -n "${ids[i]}" ] || fail "no node id: $out"
done
start_storage() {
  local i=$1; shift
  start_node "node$i" "${ports[i]}" ONYX_STORAGE_CONTRACT="$storage" ONYX_STORAGE_NODE_ID="${ids[i]}" \
    ONYX_STORAGE_KEY_FILE="${keys[i]}" ONYX_STORAGE_FROM_BLOCK="$genesis" ONYX_STORAGE_EVERY_SECS=1 "$@"
  node_pids[i]=$started
}
for i in 0 1 2 3 4; do
  extra=()
  [ "$i" = 2 ] && extra=(ONYX_STORAGE_KEEPER=on)
  start_storage "$i" "${extra[@]}"
done
pass "nodes ${ids[*]:0:1}... registered; node 2 is also the keeper"

echo "6. a deal: the publisher pays for 3 replicas of every shard for 14 epochs"
castw send --unlocked --from "$deployer" "$token" 'mint(address,uint256)' "$signer" 1000000000000000000000 >/dev/null
# shellcheck disable=SC2046
deal_out=$("$st_bin" deal $(SK "$work/publisher.key") --dir "$work/v1" --r 3 --epochs 14 --source-url "$PUB/") \
  || fail "deal"
deal=$(grep -oE 'DEAL_ID=[0-9]+' <<<"$deal_out" | cut -d= -f2)
DEAL_INFO='dealInfo(uint64)(address,bytes32,bytes32,bytes32,uint32,uint8,uint64,uint64,uint64,uint256,uint256,bool)'
# deal_field ID N: line N of dealInfo (8 start epoch, 9 end epoch, 10 deposit, 11 paid, 12 closed).
deal_field() { castw call "$storage" "$DEAL_INFO" "$1" | sed -n "$2p" | awk '{print $1}'; }
start_epoch=$(deal_field "$deal" 8)
grep -q ' anchor=own-report ' <<<"$deal_out" || fail "the first deal does not name its own report: $deal_out"
pass "$deal_out"

echo "6b. a deal on the catalog dataset (ADR-0067): createDealInCatalog, 3 replicas for 3 epochs"
# The tool refuses a catalog that does not list the manifest, before anything is sent.
# shellcheck disable=SC2046
if "$st_bin" deal $(SK "$work/publisher.key") --dir "$work/v1" --catalog "$work/catalog0.bin" --r 3 --epochs 3 \
  --source-url "$PUB/" >"$work/deal-refused.log" 2>&1; then
  fail "a deal through a catalog that does not list the manifest was sent"
fi
grep -q 'does not list manifest' "$work/deal-refused.log" || { cat "$work/deal-refused.log"; fail "the refusal does not say why"; }
[ "$(castw call "$storage" 'dealCount()(uint256)' | awk '{print $1}')" = 1 ] || fail "the refused deal reached the chain"
pass "a catalog that does not list the manifest: refused by the tool, nothing sent"
# Catalog 0 lists the manifest, but catalog 1 is in the book: the tool asks the book before the
# approval and stops; sent anyway (cast, the tool's own calldata rules), the contract reverts.
# shellcheck disable=SC2046
if "$st_bin" deal $(SK "$work/publisher.key") --dir "$work/c1" --catalog "$work/catalog0.bin" --r 3 --epochs 3 \
  --source-url "$PUB/" >"$work/deal-old.log" 2>&1; then
  fail "a deal through the old catalog was sent"
fi
grep -q 'CatalogSuperseded' "$work/deal-old.log" || { cat "$work/deal-old.log"; fail "the old catalog's refusal does not say why"; }
allow_before=$(castw call "$token" 'allowance(address,address)(uint256)' "$signer" "$storage" | awk '{print $1}')
[ "$allow_before" = 0 ] || fail "the refused deal left an approval of $allow_before"
# The same deal sent without asking the book first (--book-check no, a test switch): the
# contract itself refuses it, and no deal is made.
# shellcheck disable=SC2046
if "$st_bin" deal $(SK "$work/publisher.key") --dir "$work/c1" --catalog "$work/catalog0.bin" --r 3 --epochs 3 \
  --source-url "$PUB/" --book-check no >"$work/deal-old-chain.log" 2>&1; then
  fail "the contract opened a deal through the old catalog"
fi
superseded=$(castw sig 'CatalogSuperseded()')
grep -qiE "revert|${superseded#0x}" "$work/deal-old-chain.log" || { cat "$work/deal-old-chain.log"; fail "the contract's refusal of the old catalog is not a revert"; }
[ "$(castw call "$storage" 'dealCount()(uint256)' | awk '{print $1}')" = 1 ] || fail "the old-catalog deal reached the chain"
pass "the old catalog 0 (catalog 1 is in the book): refused by the tool before any approval, and by the contract when sent anyway (CatalogSuperseded)"
# The real one, from the manifest answer of the publisher's node (header, leaf proof, report).
# shellcheck disable=SC2046
cat_out=$("$st_bin" deal $(SK "$work/publisher.key") --root "$cat_root" --node "$PUB/" --r 3 --epochs 3 --source-url "$PUB/") \
  || fail "catalog deal"
cat_deal=$(grep -oE 'DEAL_ID=[0-9]+' <<<"$cat_out" | cut -d= -f2)
grep -q ' anchor=catalog-1 ' <<<"$cat_out" || fail "the catalog deal does not name the newest catalog (1): $cat_out"
[ "$(deal_field "$cat_deal" 8)" = "$start_epoch" ] || fail "the two deals start in different epochs"
cat_end=$(deal_field "$cat_deal" 9)
cat_deposit=$(deal_field "$cat_deal" 10)
# DealAnchor(dealId, digest, inCatalog): false for the own report, true for the catalog.
castw logs --from-block "$genesis" --address "$storage" 'DealAnchor(uint64 indexed,bytes32,bool)' --json >"$work/anchors.json"
python3 -I - "$work/anchors.json" "$deal" "$cat_deal" <<'PY' || fail "DealAnchor"
import json, sys
logs, own, cat = json.load(open(sys.argv[1])), int(sys.argv[2]), int(sys.argv[3])
by = {int(l["topics"][1], 16): l["data"][2:] for l in logs}
assert set(by) == {own, cat}, by
assert int(by[own][64:128], 16) == 0 and int(by[cat][64:128], 16) == 1, by
assert by[own][:64] != by[cat][:64] and int(by[cat][:64], 16) != 0, by
PY
pass "$cat_out"
pass "DealAnchor: deal $deal by its own report, deal $cat_deal in a catalog"

# The JSON of `onyx-storage status --node` for node i.
status_of() { "$st_bin" status "${S[@]}" --node "${ids[$1]}"; }
# Wait (cap 180 s, polled) until every live node in "$@" serves every shard placed on it next epoch.
wait_placed() {
  local ok
  for _ in $(seq 360); do
    ok=1
    for i in "$@"; do
      status_of "$i" >"$work/st$i.json" || { ok=0; break; }
      python3 -I - "$work/st$i.json" "${urls[i]}/v1/bulk/$publisher" "$deal=$dataset" "$cat_deal=$cat_dataset" <<'PY' || { ok=0; break; }
import json, sys, urllib.request
st = json.load(open(sys.argv[1]))["node"]
names = dict(x.split("=") for x in sys.argv[3:])
need = {}
for a in st["shards_next_epoch"] + st["shards_now"]:
    need.setdefault(str(a["deal"]), set()).add(a["shard"])
for d, shards in need.items():
    req = urllib.request.Request(f"{sys.argv[2]}/{names[d]}", headers={"User-Agent": "onyx-dev"})
    try:
        info = json.load(urllib.request.urlopen(req, timeout=5))
    except Exception:
        sys.exit(1)
    if not shards <= {s["index"] for s in info["shards"] if s.get("tier")}:
        sys.exit(1)
PY
    done
    [ "$ok" = 1 ] && return 0
    sleep 0.5
  done
  return 1
}
echo "7. every node fetches the shards placed on it (from the publisher or a replica) and checks every byte"
t=$(now)
wait_placed 0 1 2 3 4 || fail "the nodes did not fetch their shards"
fetch_secs=$(since "$t")
python3 -I - "$work"/st{0,1,2,3,4}.json "$((shards + cat_shards))" <<'PY' || fail "placement"
import json, sys
files, n = sys.argv[1:-1], int(sys.argv[-1])
copies = {}
for f in files:
    for a in json.load(open(f))["node"]["shards_next_epoch"]:
        k = (a["deal"], a["shard"])
        copies[k] = copies.get(k, 0) + 1
assert len(copies) == n and all(c == 3 for c in copies.values()), copies
print(f"  ok: {n} shards of 2 deals x 3 replicas placed by rendezvous: " + ", ".join(
    str(len(json.load(open(f))["node"]["shards_next_epoch"])) for f in files) + " shards per node")
PY
for i in 0 1 2 3 4; do cp "$work/st$i.json" "$work/initial$i.json"; done
pass "fetched and checked in ${fetch_secs}s"

key_min=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["key_min"])' "$work/v1/manifest.json")
key_max=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["key_max"])' "$work/v1/manifest.json")
read_all() { "$st_bin" read "${S[@]}" --deal "$deal" --from "$key_min" --to "$((key_min + ($key_max - key_min) / 2))"; }
echo "8. a client reads half the dataset from the replicas the placement names, every row checked"
read_all >"$work/read1.json" || fail "read"
python3 -I -c 'import json,sys; r=json.load(open(sys.argv[1])); assert r["ok"] and r["rows"] > 0, r; print("  ok: %d rows from %d nodes" % (r["rows"], len({s["url"] for s in r["served_by"]})))' "$work/read1.json"
# The catalog deal: the same read, and a rows answer of a storage node (which holds no catalog)
# checked by onyx-check against the book: the catalog's report, the leaf, the rows.
cat_min=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["key_min"])' "$work/c1/manifest.json")
"$st_bin" read "${S[@]}" --deal "$cat_deal" --from "$cat_min" --to "$((cat_min + 500))" >"$work/read-cat.json" || fail "read of the catalog deal"
python3 -I -c 'import json,sys; r=json.load(open(sys.argv[1])); assert r["ok"] and r["rows"] > 0, r' "$work/read-cat.json" || fail "catalog deal rows"
holder=$(python3 -I - "$work" "$cat_deal" <<'PY'
import json, sys
for i in range(5):
    st = json.load(open(f"{sys.argv[1]}/st{i}.json"))["node"]
    if any(a["deal"] == int(sys.argv[2]) and a["shard"] == 0 for a in st["shards_next_epoch"]):
        print(i)
        break
PY
)
[ -n "$holder" ] || fail "no node holds shard 0 of the catalog deal"
curl -sf "${urls[holder]}/v1/bulk/$publisher/$cat_dataset/rows?limit=20" >"$work/rows-cat.json" || fail "rows from storage node $holder"
python3 -I -c 'import json,sys; a=json.load(open(sys.argv[1]))["anchor"]; assert a["catalog"]["number"] == 1, a' "$work/rows-cat.json" \
  || fail "the storage node's answer does not name catalog 1"
"$check_bin" --publisher "$publisher" --dataset "$cat_dataset" --bulk "$work/rows-cat.json" --rpc "$RPC" --chain-id 31337 --book "$book" \
  >"$work/check-cat.json" || { cat "$work/check-cat.json"; fail "onyx-check refused the storage node's catalog-anchored rows"; }
pass "catalog deal $cat_deal: $(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["rows"])' "$work/read-cat.json") rows read from the replicas; rows from storage node $holder pass onyx-check"

# mine_to E: mine to block start(E) + 1 (the seed of E can be recorded).
mine_to() {
  local want=$((genesis + $1 * EB + 1)) cur
  cur=$(castw block-number)
  [ "$cur" -lt "$want" ] && castw rpc anvil_mine "$((want - cur))" >/dev/null
  return 0
}
epoch_json() { "$st_bin" epoch "${S[@]}" --epoch "$1"; }
# wait_epoch E SKIP...: wait (cap 120 s) until every challenge of E to a node not in SKIP passed.
wait_epoch() {
  local e=$1; shift
  local skip
  skip=$(printf '%s,' "$@")
  for _ in $(seq 240); do
    epoch_json "$e" >"$work/epoch$e.json" 2>/dev/null || true
    if python3 -I - "$work/epoch$e.json" "$skip" <<'PY'
import json, sys
try:
    r = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(1)
skip = set(x for x in sys.argv[2].split(",") if x)
if r.get("seed") is None:
    sys.exit(1)
sys.exit(0 if all(a["status"] == "passed" for a in r["asks"] if a["node"] not in skip) else 1)
PY
    then return 0; fi
    sleep 0.5
  done
  return 1
}

echo "9. epoch $start_epoch: the seed from a past block hash picks 2 slots; every challenged holder proves on chain"
t=$(now)
mine_to "$start_epoch"
wait_epoch "$start_epoch" || { cat "$work/epoch$start_epoch.json"; fail "honest nodes did not all pass"; }
answer_secs=$(since "$t")
python3 -I - "$work/epoch$start_epoch.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
assert r["asks"] and all(a["status"] == "passed" for a in r["asks"]), r
paid = sum(int(l["earned"]) for l in r["lines"])
print(f"  ok: {len(r['asks'])} challenges, all passed; {paid} base units of tONYX paid to {len(r['lines'])} nodes")
PY

# The node killed (A) and the node that drops its data (B) are the two with the most shards,
# never the keeper (node 2): a node holding 1 of 8 shards is asked so rarely that it was not jailed
# within 8 epochs once (a public check run, 2026-11-02). H: the three honest nodes, keeper included.
read -r A B <<<"$(python3 -I - "$work" <<'PY'
import json, sys
n = {i: len(json.load(open(f"{sys.argv[1]}/initial{i}.json"))["node"]["shards_next_epoch"]) for i in (0, 1, 3, 4)}
order = sorted(n, key=lambda i: (-n[i], i))
print(order[0], order[1])
PY
)"
H=()
for i in 0 1 2 3 4; do [ "$i" = "$A" ] || [ "$i" = "$B" ] || H+=("$i"); done
echo "10. node $A is killed: reads keep working from the other replicas"
stop "${node_pids[A]}"; node_pids[A]=""
read_all >"$work/read2.json" || fail "read with a node down"
python3 -I - "$work/read2.json" "${urls[A]}" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
assert r["ok"] and r["rows"] > 0, r
assert all(s["url"].rstrip("/") != sys.argv[2] for s in r["served_by"]), r
print(f"  ok: {r['rows']} rows, {len(r['refused'])} refusals failed over ({', '.join(sorted(set(x['why'][:40] for x in r['refused']))) or 'none'})")
PY

echo "11. node $B drops every shard it holds (disk wiped, fetching off) and stays online"
stop "${node_pids[B]}"
rm -rf "$work/node$B/bulk/hot" "$work/node$B/bulk/tiers.json"
start_storage "$B" ONYX_STORAGE_FETCH=off

e=$((start_epoch + 1))
forged=0 a_missed=0 b_slashed=0
while [ "$e" -le $((start_epoch + 8)) ]; do
  echo "12.$((e - start_epoch)). epoch $e: honest nodes prove; node $A (down) and node $B (no data) cannot"
  mine_to "$e"
  wait_epoch "$e" "${ids[A]}" "${ids[B]}" || { cat "$work/epoch$e.json"; fail "honest nodes did not all pass in epoch $e"; }
  if [ "$forged" = 0 ]; then
    ask=$(python3 -I -c 'import json,sys; r=json.load(open(sys.argv[1])); a=[x for x in r["asks"] if x["node"]==sys.argv[2] and x["status"]=="open"]; print("%d %d" % (a[0]["deal"], a[0]["slot"]) if a else "")' "$work/epoch$e.json" "${ids[B]}")
    if [ -n "$ask" ]; then
      read -r d slot <<<"$ask"
      pdir=$work/v1
      [ "$d" = "$cat_deal" ] && pdir=$work/c1
      "$st_bin" prove "${S[@]}" --deal "$d" --slot "$slot" --node "${ids[B]}" --dir "$pdir" >"$work/prove.json"
      calldata=$(python3 -I - "$work/prove.json" <<'PY'
import json, sys
p = json.load(open(sys.argv[1]))
b = bytearray.fromhex(p["calldata"][2:])
b[p["row_at"] + p["row_len"] - 1] ^= 1  # one changed byte in the row
print("0x" + b.hex())
PY
)
      castw send --unlocked --from "${ops[B]}" "$storage" "$calldata" >/dev/null
      st=$(castw call "$storage" 'slotStatus(uint64,uint64,uint32,bytes32)(uint8)' "$d" "$e" "$slot" "${ids[B]}")
      [ "$st" = 2 ] || fail "a forged row was not refused (status $st)"
      forged=1
      pass "node $B's answer with one forged row byte: refused on chain (failed) and slashed"
    fi
  fi
  mine_to $((e + 1))
  # The keeper (node 2) settles the misses of epoch e after it ends.
  for _ in $(seq 240); do
    epoch_json "$e" >"$work/epoch$e.json"
    python3 -I -c 'import json,sys; r=json.load(open(sys.argv[1])); sys.exit(0 if r["open"] == 0 else 1)' "$work/epoch$e.json" && break
    sleep 0.5
  done
  read -r am bs <<<"$(python3 -I - "$work/epoch$e.json" "${ids[A]}" "${ids[B]}" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
assert r["open"] == 0, r
line = {l["node"]: l for l in r["lines"]}
a = line.get(sys.argv[2], {"missed": 0})
b = line.get(sys.argv[3], {"missed": 0, "failed": 0})
print(a["missed"], b["missed"] + b["failed"])
PY
)"
  a_missed=$((a_missed + am)) b_slashed=$((b_slashed + bs))
  pass "epoch $e settled: node $A missed $am, node $B missed or failed $bs (keeper)"
  status_of "$A" >"$work/stA.json"
  a_status=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["node"]["status"])' "$work/stA.json")
  if [ "$forged" = 1 ] && [ "$a_status" = 3 ] && [ "$b_slashed" -ge 1 ]; then
    e=$((e + 1)); break
  fi
  e=$((e + 1))
done
[ "$forged" = 1 ] || fail "node $B was never asked: no forged answer sent"
[ "$a_status" = 3 ] || fail "node $A was not jailed after its misses (status $a_status, missed $a_missed)"
pass "node $A jailed after $a_missed misses; node $B slashed $b_slashed times"

echo "13. repair: the shards node $A held now have new replicas, fetched and checked"
t=$(now)
mine_to "$e"
wait_placed "${H[@]}" || fail "the live nodes did not repair"
repair_secs=$(since "$t")
python3 -I - "$work" "$A" "${H[@]}" <<'PY' || fail "no repaired replica"
import json, sys
w, a, live = sys.argv[1], int(sys.argv[2]), [int(x) for x in sys.argv[3:]]
def held(prefix, i):
    return {(a["deal"], a["shard"]) for a in json.load(open(f"{w}/{prefix}{i}.json"))["node"]["shards_next_epoch"]}
before_a = held("initial", a)
new = 0
for i in live:
    gained = held("st", i) - held("initial", i)
    new += len(gained & before_a)
assert new > 0, "no shard of the jailed node moved to a live node"
print(f"  ok: {new} copies of node {a}'s shards repaired onto nodes {', '.join(map(str, live))}")
PY
read_all >"$work/read3.json" || fail "read after repair"
pass "reads after repair: $(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["rows"])' "$work/read3.json") rows"

echo "13b. the catalog deal runs to its end (epoch $cat_end) and is closed: the unpaid deposit goes back"
while [ "$e" -lt "$cat_end" ]; do
  mine_to "$e"
  wait_epoch "$e" "${ids[A]}" "${ids[B]}" || { cat "$work/epoch$e.json"; fail "honest nodes did not all pass in epoch $e"; }
  mine_to $((e + 1))
  for _ in $(seq 240); do
    epoch_json "$e" >"$work/epoch$e.json"
    python3 -I -c 'import json,sys; r=json.load(open(sys.argv[1])); sys.exit(0 if r["open"] == 0 else 1)' "$work/epoch$e.json" && break
    sleep 0.5
  done
  e=$((e + 1))
done
mine_to "$cat_end"
# Its life in the epoch reports: passed answers in every one of its epochs.
cat_life=$(python3 -I - "$work" "$cat_deal" "$start_epoch" "$cat_end" <<'PY'
import json, sys
w, d, a, b = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4])
out = []
for e in range(a, b):
    asks = [x for x in json.load(open(f"{w}/epoch{e}.json"))["asks"] if x["deal"] == d]
    passed = sum(1 for x in asks if x["status"] == "passed")
    assert asks and passed > 0, (e, asks)
    assert all(x["status"] != "open" for x in asks) or e == a, (e, asks)
    out.append(f"epoch {e}: {passed} of {len(asks)} passed")
print("; ".join(out))
PY
) || fail "the catalog deal was not answered in every epoch"
[ "$(deal_field "$cat_deal" 12)" = false ] || fail "the catalog deal is closed before anyone closed it"
cat_paid=$(deal_field "$cat_deal" 11)
before=$(castw call "$token" 'balanceOf(address)(uint256)' "$signer" | awk '{print $1}')
# shellcheck disable=SC2046
"$st_bin" close $(SK "$work/publisher.key") --deal "$cat_deal" >"$work/close.log" || { cat "$work/close.log"; fail "close"; }
after=$(castw call "$token" 'balanceOf(address)(uint256)' "$signer" | awk '{print $1}')
[ "$(deal_field "$cat_deal" 12)" = true ] || fail "the catalog deal is not closed"
python3 -I -c 'import sys; d, p, b, a = map(int, sys.argv[1:]); assert 0 < p <= d and a - b == d - p, (d, p, b, a)' \
  "$cat_deposit" "$cat_paid" "$before" "$after" || fail "the refund is not the unpaid deposit"
[ "$(castw call "$storage" 'conserved()(bool)')" = true ] || fail "conserved() is false after the close"
# A closed deal is placed nowhere, and the first deal goes on.
status_of "${H[0]}" | python3 -I -c 'import json,sys; n=json.load(sys.stdin)["node"]; d=int(sys.argv[1]); assert all(a["deal"] != d for a in n["shards_now"] + n["shards_next_epoch"]), n' "$cat_deal" \
  || fail "the closed deal is still placed"
pass "catalog deal $cat_deal: $cat_life"
pass "catalog deal $cat_deal closed after epoch $((cat_end - 1)): paid $cat_paid of $cat_deposit, $((cat_deposit - cat_paid)) back to the payer; conserved() is true"

echo "14. money: honest nodes earned, slashes to the treasury, the books balance on chain"
for i in "${H[@]}"; do
  status_of "$i" >"$work/final$i.json"
done
python3 -I - "$work" "${H[@]}" <<'PY'
import glob, json, sys
w, live = sys.argv[1], [int(x) for x in sys.argv[2:]]
# Pay is per passed answer (R-STOR-8): an honest node the random slots never asked earns nothing
# yet (a public check run hit this: one node with 3 of 8 shards, 6 slots drawn). Every node with a
# passed answer earned, none of them was jailed, and the honest nodes earned something together.
passed = {a["node"] for f in glob.glob(f"{w}/epoch*.json") for a in json.load(open(f))["asks"] if a["status"] == "passed"}
nodes = [json.load(open(f"{w}/final{i}.json"))["node"] for i in live]
for n in nodes:
    assert n["status"] == 1, n
    assert n["id"] not in passed or int(n["earned"]) > 0, n
assert sum(int(n["earned"]) for n in nodes) > 0, nodes
print(f"  ok: nodes {', '.join(map(str, live))} earned " + ", ".join(n["earned"] for n in nodes))
PY
bA=$(status_of "$A" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["node"]["bond"])')
bB=$(status_of "$B" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["node"]["bond"])')
# Each node activated with its 1 tONYX bond less the 2% registration fee (ADR-0060 amendment 1).
python3 -I -c 'import sys; sys.exit(0 if all(int(b) < 98 * 10**16 for b in sys.argv[1:]) else 1)' "$bA" "$bB" \
  || fail "a misbehaving node kept its whole bond"
# W: the honest node that earned the most withdraws (one the slots never asked has nothing to take).
W=$(python3 -I - "$work" "${H[@]}" <<'PY'
import json, sys
w, live = sys.argv[1], sys.argv[2:]
print(max(live, key=lambda i: int(json.load(open(f"{w}/final{i}.json"))["node"]["earned"])))
PY
)
before=$(castw call "$token" 'balanceOf(address)(uint256)' "${ops[W]}" | awk '{print $1}')
# shellcheck disable=SC2046
"$st_bin" withdraw $(SK "${keys[W]}") >/dev/null
after=$(castw call "$token" 'balanceOf(address)(uint256)' "${ops[W]}" | awk '{print $1}')
python3 -I -c 'import sys; assert int(sys.argv[2]) > int(sys.argv[1])' "$before" "$after" || fail "withdraw"
# shellcheck disable=SC2046
"$st_bin" sweep $(SK "${keys[W]}") >/dev/null
tre=$(castw call "$token" 'balanceOf(address)(uint256)' "$treasury" | awk '{print $1}')
"$st_bin" books "${S[@]}" --replay yes >"$work/books.json" || { cat "$work/books.json"; fail "the books do not balance or do not replay"; }
python3 -I - "$work/books.json" "$tre" <<'PY'
import json, sys
b = json.load(open(sys.argv[1]))
assert b["conserved"] is True and b["balance"] == b["held"], b
# The same totals rebuilt from the contract's money events (onyx_storage::ledger::replay).
assert b["replayed"] is True, b
# Slashes and the five registration fees; the keeper's reward for the misses it settled stays
# in its earnings; the rest went to the treasury.
slashed, rewarded = int(b["slashed"]), int(b["rewarded"])
assert slashed > 5 * 2 * 10**16 and rewarded > 0, b
assert int(sys.argv[2]) == int(b["swept"]) == slashed - rewarded, (b, sys.argv[2])
print(f"  ok: deposits {b['deposits']}, bonds {b['bonds_in']}; paid {b['paid']}, slashed and fees {slashed}, {rewarded} to the keeper, the rest swept to the treasury; held {b['held']} = balance; the events replay to the same books")
PY
[ "$(castw call "$storage" 'conserved()(bool)')" = true ] || fail "conserved() is false"
pass "conserved() is true on chain"
# ADR-0060 amendment 2 item 1: the public report of declines and slashes per node and per deal.
"$st_bin" declines "${S[@]}" >"$work/declines.json" || { cat "$work/declines.json"; fail "declines"; }
python3 -I - "$work/declines.json" "${ids[A]}" "${ids[B]}" <<'PY'
import json, sys
r, bad = json.load(open(sys.argv[1])), {x.lower() for x in sys.argv[2:]}
by = {n["id"].lower(): n for n in r["nodes"]}
assert len(by) == 5 and bad <= set(by), r
for i, n in by.items():
    assert n["shard_declines"] == 0 and n["whole_deal_declines"] == 0, n
    assert (n["slashes"] > 0) == (i in bad), n
total = sum(n["slashes"] for n in r["nodes"])
assert sum(d["slashes"] for d in r["deals"]) == total, r
print(f"  ok: {total} slashed shards, all on the two misbehaving nodes; no declines")
PY
pass "declines report: slashes only on the misbehaving nodes"

gas_of() { python3 -I -c 'import json,sys; print(int(json.load(sys.stdin)["gasUsed"], 16))'; }
answer_tx=$(castw logs --from-block "$genesis" --address "$storage" 'Answered(uint64,uint64,uint32,bytes32,uint32,bool,uint256)' --json \
  | python3 -I -c 'import json,sys; print(json.load(sys.stdin)[0]["transactionHash"])')
answer_gas=$(castw rpc eth_getTransactionReceipt "$answer_tx" | gas_of)
echo
echo "MEASURED (local anvil, debug build, epochs of $EB blocks)"
echo "  deployment:     OnyxStorage with its anchor reader, $size bytes of code, gas $deploy_gas"
echo "  dataset:        $rows rows in $shards shards (synthetic, ${mib} MiB CSV), R=3 over 5 nodes"
echo "  catalog deal:   $cat_shards shards, opened through catalog 1 (catalog 0 refused), R=3; $cat_life; closed, $((cat_deposit - cat_paid)) of $cat_deposit back"
echo "  first fetch:    every node's shards fetched and checked in ${fetch_secs}s"
echo "  first epoch:    every challenge answered in ${answer_secs}s; one answer: gas $answer_gas"
echo "  repair:         live nodes held their new shards ${repair_secs}s after the jail epoch"
echo "  total:          $(since "$t_start")s"
echo "STORAGE NETWORK DEMO PASSED"
