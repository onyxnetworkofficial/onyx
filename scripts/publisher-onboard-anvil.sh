#!/usr/bin/env bash
# ADR-0066 (publisher onboarding), end to end on a local anvil with anvil dev keys and freshly made
# TEST keys only. Made-up ids and origins; nothing leaves this machine and no real chain is touched.
# THREATS PUBON-6 (the limit before the key), PUBON-10 (the relay in the run of registerKey),
# PUBON-13 and PUBON-14 (what a refusal keeps; the record against the book), PUBON-16 (a refused
# key is blocked), PUBON-18 (a paused publisher's tombstone), PUBON-19 (the reviewer's tool), and review S3-R1:
# S3-R1-M1, S3-R1-M3, S3-R1-M5, S3-R1-M6, S3-R1-L4, S3-R1-L6.
#
#   scripts/publisher-onboard-anvil.sh
#
# Steps (about 40 s once built):
#  1. its own anvil; the settlement and OnyxSignedBook (reviewer: dev account 1, its key in a 0600 file)
#  2. a fresh applicant: two keys, a keys document, a small signed sample, `apply`; `doctor` reads the
#     book from the chain and a folder copy of the origin, and names nothing missing
#  3. the reviewer: `verify-application --rpc` (the book read at a finalized block, never a list) keeps
#     the bytes; a node without limits per publisher is refused; a dry run of `book-reviewer approve`
#     sends and writes nothing; the first run writes the node's limit and stops (exit 6); once the node
#     runs with the entry (the tool asks the node), the run sends registerKey and relays the applicant's signed sample report; the record is written; the same
#     run again sends nothing
#  4. scripts/check-publishers.py: the record, its status list, the limits file and the book agree
#  5. the applicant uploads the sample; the node serves it; `onyx-check --bulk` passes against the book
#  6. a refused application: its record keeps the hash, the code, the date and the blocked signer only;
#     the same key is refused again (signer-blocked); a new key is free
#  7. a second key: refused without a cosignature, registered with one (the sample's report is not sent
#     twice)
#  8. a withdraw request: verified, taken once, refused the second time
#  9. a pause: the paused publisher's next version is refused, its tombstone catalog is taken
# 10. later requests: a raise needs its named limit; a stolen key is marked only while another key
#     of the id is active; a retirement is sent once
# 11. the application, request and keys-document files, the record and the status list pass the
#     repository's secret scanner (gitleaks, when installed)
# Every failure prints a FAIL line. Prints PUBLISHER ONBOARDING REHEARSAL PASSED.
# Env: FOUNDRY_BIN (~/.foundry/bin), CARGO_TARGET_DIR.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
target=${CARGO_TARGET_DIR:-$REPO/target}
work=$(mktemp -d)
anvil_pid="" node_pid=""
stop() { [ -n "$1" ] || return 0; kill "$1" 2>/dev/null || true; wait "$1" 2>/dev/null || true; }
cleanup() { stop "$node_pid"; stop "$anvil_pid"; rm -rf "$work"; }
trap cleanup EXIT
trap 'exit 130' INT TERM HUP
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }
# The JSON field `$2` (a dotted path) of the file `$1`.
field() { python3 -I -c 'import json,sys
v=json.load(open(sys.argv[1]))
for k in sys.argv[2].split("."): v=v[int(k)] if isinstance(v,list) else v.get(k)
print("null" if v is None else v)' "$1" "$2"; }
# run NAME EXPECTED_EXIT CMD...: the output goes to $work/NAME.out; any other exit code is a failure.
run() {
  local name=$1 want=$2 rc=0
  shift 2
  "$@" >"$work/$name.out" 2>"$work/$name.err" || rc=$?
  [ "$rc" = "$want" ] || { cat "$work/$name.out" "$work/$name.err" >&2; fail "$name: exit $rc, expected $want ($*)"; }
}
said() { grep -q -- "$2" "$work/$1.out" || { cat "$work/$1.out" >&2; fail "$1 did not say: $2"; }; }
erred() { grep -q -- "$2" "$work/$1.err" || { cat "$work/$1.err" >&2; fail "$1 did not fail with: $2"; }; }
# keccak_of NAME: the keccak=0x… a command printed into $work/NAME.out; a missing one is a FAIL line.
keccak_of() {
  local k
  k=$(grep -oE 'keccak=0x[0-9a-f]{64}' "$work/$1.out" | cut -d= -f2) || fail "$1 printed no keccak"
  [ -n "$k" ] || fail "$1 printed no keccak"
  echo "$k"
}
started=$(date +%s)

# --workspace (not -p): the feature set of check-all's test build, so nothing compiles twice.
cargo build --locked -q --workspace --bin onyx-node --example pin-signed-book \
  --bin onyx-publish --bin book-reviewer --bin onyx-check || fail "cargo build"
pub=$target/debug/onyx-publish
reviewer_bin=$target/debug/book-reviewer
check_bin=$target/debug/onyx-check
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
reviewer=$(acct 1)
# Anvil's finalized block is 64 behind its head: every read of the book is made there.
final() { castw rpc anvil_mine 70 >/dev/null; }

echo "1. settlement and signed book (reviewer: dev account 1, its key in a 0600 file)"
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
book=$(field "$manifest" signedBook)
# The reviewer's key goes from anvil's file into a file only its owner reads: never on a command
# line, in the environment or on the screen.
python3 -I - "$work/anvil.json" "$work/reviewer.key" <<'PY'
import json, os, sys
fd = os.open(sys.argv[2], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
os.write(fd, json.load(open(sys.argv[1]))["private_keys"][1].encode())
os.close(fd)
PY
NET=(--chain-id 31337 --book "$book")
CHAIN=(--rpc "$RPC" "${NET[@]}")
records=$work/records limits=$work/publishers.json
mkdir -p "$records"
echo '{"reserve_gib": 1, "publishers": []}' >"$limits"
MAX_GIB=4
final
pass "book $book, reviewer $reviewer"

# A small real table: 40 gauges on three days.
python3 -I - "$work" <<'PY'
import json, sys
w = sys.argv[1]
json.dump({"columns": [{"name": "t", "kind": "int"}, {"name": "gauge", "kind": "int"},
                       {"name": "level", "kind": "decimal", "scale": 2}], "key": "t"}, open(w + "/schema.json", "w"))
for name, start, n in (("data", 1, 120), ("more", 121, 40)):
    with open(f"{w}/{name}.csv", "w") as fh:
        fh.write("t,gauge,level\n")
        for i in range(start, start + n):
            fh.write(f"{i},{i % 40},{100 + i % 7}.{i % 100:02d}\n")
PY
# applicant DIR ID ORIGIN DATASET: two keys, the keys document, the signed sample.
applicant() {
  local d=$1 id=$2 dataset=$4
  mkdir -p "$d"
  "$pub" keygen --out "$d/publisher.key" >/dev/null || fail "keygen"
  "$pub" keygen --out "$d/spare.key" >/dev/null || fail "keygen"
  "$pub" keys-doc --key "$d/publisher.key" --publisher "$id" --out "$d/site/.well-known/onyx-source-keys.json" >/dev/null 2>&1 || fail "keys-doc"
  "$pub" build --schema "$work/schema.json" --input "$work/data.csv" --dataset "$dataset" --publisher "$id" \
    --license CC0-1.0 --out "$d/site/sample" >/dev/null || fail "build the sample"
  "$pub" sign --key "$d/publisher.key" --dir "$d/site/sample" >/dev/null || fail "sign the sample"
}
# apply_as NAME DIR KEY ID ORIGIN [more options]: writes the application; its Keccak in $hash.
apply_as() {
  local name=$1 d=$2 key=$3 id=$4 origin=$5
  shift 5
  run "$name" 0 "$pub" apply --key "$d/$key" --publisher "$id" --origin "$origin" --site "$d/site" \
    --data "Daily water levels of 40 public gauges, from the publisher's own sensors." "${NET[@]}" "$@"
  hash=$(keccak_of "$name") || exit 1
}

echo "2. a fresh applicant: keys, keys document, sample, apply, doctor"
A=$work/a ID=river-levels.open ORIGIN=data.example.org/river-levels
applicant "$A" "$ID" "$ORIGIN" river-levels.daily
apply_as apply-a "$A" publisher.key "$ID" "$ORIGIN"
hash_a=$hash
run doctor-a 0 "$pub" doctor --dir "$A" --mirror "$A/site" "${CHAIN[@]}"
said doctor-a 'registered key(s) read from the book at its finalized block'
said doctor-a '^ok       gas:'
said doctor-a '^ok       pointer: ready'
grep -qE '^(missing|problem)' "$work/doctor-a.out" && fail "doctor names something missing or wrong"
pass "apply and doctor: nothing missing, the book read from the chain, $(grep -c '^ok' "$work/doctor-a.out") lines ok"

echo "3. the reviewer: verify with --rpc, a dry run, the limit, registerKey and the relay, the record"
art=$work/artifact-a
run verify-a 0 "$pub" verify-application --dir "$A/site" --origin "$ORIGIN" --hash "$hash_a" "${CHAIN[@]}" \
  --decisions "$records" --save "$art"
[ "$(field "$work/verify-a.out" bookRead.reviewer)" = "$reviewer" ] || fail "the report does not name the book's reviewer"
[ "$(field "$work/verify-a.out" ok)" = True ] || fail "verify-application did not pass"
# Without the chain and without a list the same file is not a decision.
run verify-a-unread 4 "$pub" verify-application --dir "$A/site" --origin "$ORIGIN" --hash "$hash_a" "${NET[@]}" --decisions "$records"
# A list beside the chain is refused.
echo '[]' >"$work/list.json"
run verify-a-list 2 "$pub" verify-application --dir "$A/site" "${CHAIN[@]}" --registered "$work/list.json"
# The public answers: yes or no per line; line 12 (the operator grouping) is not published.
echo '{"7": "yes", "8": "yes", "9": "yes", "10": "yes", "11": "yes", "13": "yes"}' >"$work/answers.json"
approve() {  # approve NAME EXIT ARTIFACT ORIGIN HASH [more options]
  local name=$1 want=$2 a=$3 o=$4 h=$5
  shift 5
  run "$name" "$want" "$reviewer_bin" approve --application "$a" --origin "$o" --hash "$h" "${CHAIN[@]}" \
    --records "$records" --limits "$limits" --max-gib "$MAX_GIB" --answers "$work/answers.json" \
    --reviewer-key "$work/reviewer.key" --node "$NODE" "$@"
}
mkdir -p "$work/cas" "$work/state"
printf '{"version": 1, "shards": []}\n' >"$work/catalog.json"
node_port=$(python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
NODE="http://127.0.0.1:$node_port"
# start_node [nolimits]: the node with the limits file, or without limits per publisher.
start_node() {
  local with=(ONYX_BULK_PUBLISHERS="$limits")
  [ "${1:-}" != nolimits ] || with=()
  env -i PATH="$PATH" HOME="$HOME" ONYX_ENV=dev ONYX_RIGHTS=cleared ONYX_PAYMENTS=off \
    ONYX_LISTEN="127.0.0.1:$node_port" ONYX_PUBLIC_URL="$NODE/" ONYX_RPC_URL="$RPC" \
    ONYX_DEPLOYMENT="$manifest" ONYX_CATALOG="$work/catalog.json" ONYX_CAS_DIR="$work/cas" \
    ONYX_STATE_DIR="$work/state" ONYX_BULK_DIR="$work/bulk" ONYX_BULK_MAX_GIB="$MAX_GIB" \
    "${with[@]}" "$target/debug/onyx-node" >>"$work/node.log" 2>&1 &
  node_pid=$!
  for _ in $(seq 300); do
    kill -0 "$node_pid" 2>/dev/null || { cat "$work/node.log"; fail "onyx-node exited"; }
    curl -sf "$NODE/v1/health" >/dev/null 2>&1 && return 0
    sleep 0.2
  done
  fail "onyx-node did not come up"
}
restart_node() { stop "$node_pid"; node_pid=""; start_node "$@"; }
nonce() { castw nonce "$reviewer"; }
n0=$(nonce) limits0=$(cat "$limits")
# A node without limits per publisher would give the new key the node's one quota: refused before
# anything is written or sent.
start_node nolimits
approve nolimits-a 1 "$art" "$ORIGIN" "$hash_a"
erred nolimits-a 'runs without limits per publisher'
[ "$(nonce)" = "$n0" ] && [ "$(cat "$limits")" = "$limits0" ] || fail "a node without limits: something was sent or written"
restart_node
approve dry-a 0 "$art" "$ORIGIN" "$hash_a" --dry-run
said dry-a 'DRY RUN: would send registerKey'
said dry-a 'DRY RUN: would relay'
[ "$(nonce)" = "$n0" ] && [ "$(cat "$limits")" = "$limits0" ] && [ -z "$(ls "$records")" ] \
  || fail "the dry run sent or wrote something"
pass "the dry run sent nothing and wrote nothing"
# The key comes from a file only its owner reads: a file others can read is refused, and the tool
# has no option that takes a key's digits.
install -m 0644 /dev/null "$work/open.key" && cat "$work/reviewer.key" >"$work/open.key"
run open-key 1 "$reviewer_bin" approve --application "$art" --origin "$ORIGIN" --hash "$hash_a" "${CHAIN[@]}" \
  --records "$records" --limits "$limits" --max-gib "$MAX_GIB" --answers "$work/answers.json" --node "$NODE" \
  --reviewer-key "$work/open.key"
erred open-key 'readable by its owner only'
rm -f "$work/open.key"
run key-option 2 "$reviewer_bin" approve --application "$art" --origin "$ORIGIN" --hash "$hash_a" "${CHAIN[@]}" \
  --records "$records" --limits "$limits" --max-gib "$MAX_GIB" --answers "$work/answers.json" --node "$NODE" \
  --reviewer-key-hex 0x00
erred key-option 'not an option'
[ "$(nonce)" = "$n0" ] || fail "a refused key file: a transaction was sent"
# Another chain than Base Sepolia or a local one is refused before anything is read.
run mainnet 2 "$reviewer_bin" approve --application "$art" --origin "$ORIGIN" --hash "$hash_a" --rpc "$RPC" \
  --chain-id 8453 --book "$book" --records "$records" --limits "$limits" --max-gib "$MAX_GIB" \
  --answers "$work/answers.json" --reviewer-key "$work/reviewer.key" --node "$NODE"
erred mainnet 'test networks only'
approve limit-a 6 "$art" "$ORIGIN" "$hash_a"
said limit-a 'LIMIT WRITTEN'
[ "$(nonce)" = "$n0" ] || fail "the run that writes the limit sent a transaction"
[ "$(field "$limits" publishers.0.id)" = "$ID" ] || fail "the limits file has no entry for $ID"
pass "the first run wrote the limit (1 GiB, 16 datasets) and sent nothing (exit 6)"

# The same run again before the node has the entry: still nothing sent.
approve limit-a-again 6 "$art" "$ORIGIN" "$hash_a"
[ "$(nonce)" = "$n0" ] || fail "a run before the node's restart sent a transaction"
restart_node
approve approve-a 0 "$art" "$ORIGIN" "$hash_a"
said approve-a '^NODE: limits per publisher are on'
said approve-a '^REGISTERED tx='
said approve-a '^RELAYED tx='
said approve-a '^APPROVED '
[ "$(nonce)" = $((n0 + 2)) ] || fail "the approval is not exactly two transactions of the reviewer key"
cmp -s "$records/$ID/application.json" "$A/site/.well-known/onyx-publisher-application.json" \
  || fail "the record does not keep the application's bytes"
[ "$(field "$records/$ID/decision.json" 0.state)" = approved ] || fail "the record does not say approved"
[ "$(field "$records/$ID/decision.json" 0.relay.tx)" != null ] || fail "the record names no relay transaction"
# The key's digits as a pattern file (never an argument of a command).
python3 -I - "$work/reviewer.key" "$work/key.pattern" <<'PY'
import os, sys
fd = os.open(sys.argv[2], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
os.write(fd, open(sys.argv[1]).read().strip().removeprefix("0x").encode() + b"\n")
os.close(fd)
PY
grep -rqFf "$work/key.pattern" "$work"/*.out "$work"/*.err "$work/node.log" "$records" "$limits" \
  && fail "the reviewer's key was printed or written"
approve again-a 0 "$art" "$ORIGIN" "$hash_a"
said again-a 'REGISTERED already for this application'
said again-a 'RECORDED already'
[ "$(nonce)" = $((n0 + 2)) ] || fail "the same run again sent a transaction"
pass "registerKey and the relay in one run; the record is written; the same run again sends nothing"
final

echo "4. the record, its status list, the limits file and the book agree"
status=$work/status.json
checkp() { python3 -I scripts/check-publishers.py --records "$records" --status "$status" "$@"; }
# Without the book the list says so: "approved (record)". With it: "approved".
run checkp-write 0 checkp --write
[ "$(field "$status" publishers.0.state)" = "approved (record)" ] || fail "an approval the book did not confirm is listed as confirmed"
run checkp 0 checkp --write --limits "$limits" --max-gib "$MAX_GIB" --rpc "$RPC"
[ "$(field "$status" publishers.0.state)" = approved ] || fail "the status list does not say approved"
run checkp-offline 0 checkp
# The record changed by hand is caught: the applicant's text altered after the decision.
cp -a "$records" "$work/records-bad"
sed -i 's/40 public gauges/41 public gauges/' "$work/records-bad/$ID/application.json"
run checkp-bad 1 python3 -I scripts/check-publishers.py --records "$work/records-bad" --status "$status"
said checkp-bad 'does not hash to the recorded application'
run doctor-a2 0 "$pub" doctor --dir "$A" --mirror "$A/site" --publishers "$status" --node "$NODE"
said doctor-a2 "^ok       status: $ID: approved"
said doctor-a2 '^ok       node:'
pass "check-publishers passes on the tool's record (and refuses a changed application); doctor shows approved"

echo "5. the applicant uploads the sample; the node serves it; onyx-check --bulk passes"
run upload-a 0 "$pub" upload --dir "$A/site/sample" --node "$NODE"
curl -sf "$NODE/v1/bulk/$ID/river-levels.daily/rows?limit=200" >"$work/rows.json" || fail "the node does not serve the sample"
run check-rows 0 "$check_bin" --publisher "$ID" --dataset river-levels.daily --bulk "$work/rows.json" "${CHAIN[@]}"
[ "$(field "$work/check-rows.out" rows)" = 120 ] && [ "$(field "$work/check-rows.out" chainChecked)" = True ] \
  || fail "onyx-check did not prove the 120 rows against the book"
pass "120 rows served and proven against the book, with no anchor sent by the applicant"

echo "6. a refused application blocks its signer; a new key is free"
B=$work/b ID_B=tide-tables.open ORIGIN_B=tides.example.net
applicant "$B" "$ID_B" "$ORIGIN_B" tide-tables.daily
apply_as apply-b "$B" publisher.key "$ID_B" "$ORIGIN_B"
hash_b=$hash
run refuse-b 0 "$reviewer_bin" record --application "$B/site" --origin "$ORIGIN_B" --hash "$hash_b" "${CHAIN[@]}" \
  --records "$records" --state refused --reason rights-unclear
refused=$records/application-${hash_b:2:16}/decision.json
[ "$(python3 -I -c 'import json,sys; print(",".join(sorted(json.load(open(sys.argv[1]))[0])))' "$refused")" \
  = application,blocks_signer_until,date,reason,signer,state ] || fail "the refusal keeps more or less than its six fields"
grep -qE "$ID_B|$ORIGIN_B|gauges|tide-tables" "$refused" && fail "the refusal keeps the id, the origin, the data text or the sample"
[ "$(ls "$records/application-${hash_b:2:16}")" = decision.json ] || fail "the refusal keeps a file beside the decision"
run checkp-refused 0 checkp --write
# The same key again, with a new application: blocked until its refused report is stale.
apply_as apply-b2 "$B" publisher.key "$ID_B" "$ORIGIN_B" --now $(($(date +%s) + 5))
run verify-b2 1 "$pub" verify-application --dir "$B/site" --origin "$ORIGIN_B" --hash "$hash" "${CHAIN[@]}" --decisions "$records"
[ "$(field "$work/verify-b2.out" refused.code)" = signer-blocked ] || fail "the refused key is not blocked: $(field "$work/verify-b2.out" refused.code)"
# A new key: free.
C=$work/c
applicant "$C" "$ID_B" "$ORIGIN_B" tide-tables.daily
apply_as apply-c "$C" publisher.key "$ID_B" "$ORIGIN_B"
hash_c=$hash
run verify-c 0 "$pub" verify-application --dir "$C/site" --origin "$ORIGIN_B" --hash "$hash_c" "${CHAIN[@]}" --decisions "$records"
pass "refused: six fields kept, the key blocked (signer-blocked); the same id with a new key passes"

echo "7. a second key: refused without a cosignature, registered with one"
spare=$("$pub" address --key "$A/spare.key")
"$pub" keys-doc --key "$A/publisher.key" --publisher "$ID" --also-address "$spare" \
  --out "$A/site/.well-known/onyx-source-keys.json" >/dev/null 2>&1 || fail "keys-doc with the spare"
# The tool itself refuses to write a second key's application without a cosigning key.
run apply-spare-alone 1 "$pub" apply --key "$A/spare.key" --publisher "$ID" --origin "$ORIGIN" --site "$A/site" \
  --data "Daily water levels of 40 public gauges, from the publisher's own sensors." "${NET[@]}"
apply_as apply-spare "$A" spare.key "$ID" "$ORIGIN" --cosign-key "$A/publisher.key"
hash_s=$hash
# The same application with its cosignature taken out (it is not a signed field): the reviewer's
# check refuses it. Control of the origin alone gives no key.
cp -a "$A/site" "$work/site-nocosign"
python3 -I - "$work/site-nocosign/.well-known/onyx-publisher-application.json" <<'PY'
import re, sys
t = open(sys.argv[1]).read()
u = re.sub(r'"cosignature": "0x[0-9a-f]{130}"', '"cosignature": null', t)
assert u != t
open(sys.argv[1], "w").write(u)
PY
run verify-spare-alone 1 "$pub" verify-application --dir "$work/site-nocosign" --origin "$ORIGIN" "${CHAIN[@]}" --decisions "$records"
[ "$(field "$work/verify-spare-alone.out" refused.code)" = no-cosignature ] \
  || fail "a second key without a cosignature: $(field "$work/verify-spare-alone.out" refused.code)"
art2=$work/artifact-spare
run verify-spare 0 "$pub" verify-application --dir "$A/site" --origin "$ORIGIN" --hash "$hash_s" "${CHAIN[@]}" \
  --decisions "$records" --save "$art2"
[ "$(field "$work/verify-spare.out" standing)" = second-key ] || fail "the spare's application is not a second key's"
approve approve-spare 0 "$art2" "$ORIGIN" "$hash_s"
said approve-spare '^REGISTERED tx='
said approve-spare 'RELAYED already'
[ "$(nonce)" = $((n0 + 3)) ] || fail "the second key is not exactly one more transaction"
[ "$(field "$records/$ID/decision.json" 1.standing)" = second-key ] && [ -s "$records/$ID/application-2.json" ] \
  || fail "the record does not hold the second key's application"
final
run checkp-spare 0 checkp --write --limits "$limits" --max-gib "$MAX_GIB" --rpc "$RPC"
[ "$(field "$status" publishers.0.signers.1)" = "$spare" ] || fail "the status list does not name the second key"
pass "no-cosignature without one; with one the key is registered and the sample's report is not sent twice"

echo "8. a withdraw request: verified, taken once"
run request-c 0 "$pub" request --key "$C/publisher.key" --kind withdraw --publisher "$ID_B" --origin "$ORIGIN_B" \
  --site "$C/site" "${NET[@]}"
hash_r=$(keccak_of request-c) || exit 1
run verify-request-c 0 "$pub" verify-request --dir "$C/site" --origin "$ORIGIN_B" --hash "$hash_r" "${CHAIN[@]}" --decisions "$records"
[ "$(field "$work/verify-request-c.out" request)" = withdraw ] || fail "the request is not read as a withdraw"
take() { run "$1" "$2" "$reviewer_bin" request --request "$C/site" --origin "$ORIGIN_B" --hash "$hash_r" "${CHAIN[@]}" --records "$records"; }
take take-c 0
said take-c '^TAKEN withdraw'
withdrawn=$records/application-${hash_c:2:16}/decision.json
[ "$(field "$withdrawn" 0.state)" = withdrawn ] && [ "$(field "$withdrawn" 0.sequence)" = "$(field "$work/verify-request-c.out" sequence)" ] \
  || fail "the withdrawal is not recorded with its sequence"
take take-c-again 1
said take-c-again 'NOT TAKEN'
said take-c-again 'its sequence is not above the last request taken from this key'
run verify-request-c2 1 "$pub" verify-request --dir "$C/site" --origin "$ORIGIN_B" --hash "$hash_r" "${CHAIN[@]}" --decisions "$records"
run checkp-withdrawn 0 checkp --write
pass "the withdraw is taken once (sequence $(field "$withdrawn" 0.sequence) in the record) and refused the second time"

echo "9. a pause: no new version, but a tombstone"
# The publisher pays its own anchors from here on (a local balance: no transfer links keys).
castw rpc anvil_setBalance "$("$pub" address --key "$A/publisher.key")" 0xde0b6b3a7640000 >/dev/null
# While open: its catalog 0 lists the dataset (a tombstone ends a dataset its last catalog holds).
catalog() {  # catalog N [catalog-build options]: build, sign, anchor and upload catalog N
  local n=$1
  shift
  run "catalog$n-build" 0 "$pub" catalog-build --publisher "$ID" --out "$work/catalog$n.bin" "$@"
  run "catalog$n-sign" 0 "$pub" catalog-sign --key "$A/publisher.key" --catalog "$work/catalog$n.bin"
  run "catalog$n-anchor" 0 "$pub" catalog-anchor --catalog "$work/catalog$n.bin" "${CHAIN[@]}" --gas-key "$A/publisher.key"
  final
  run "catalog$n-upload" 0 "$pub" catalog-upload --catalog "$work/catalog$n.bin" --node "$NODE"
}
echo "$A/site/sample" >"$work/dirs.txt"
catalog 0 --dirs "$work/dirs.txt"
run pause-a 0 "$reviewer_bin" pause --publisher "$ID" --limits "$limits" --records "$records"
restart_node
run checkp-paused 0 checkp --write --limits "$limits" --max-gib "$MAX_GIB"
[ "$(field "$status" publishers.0.state)" = paused ] || fail "the status list does not say paused"
"$pub" build --schema "$work/schema.json" --input "$work/more.csv" --dataset river-levels.daily --publisher "$ID" \
  --license CC0-1.0 --out "$work/a-v2" --prev "$A/site/sample" >/dev/null || fail "build version 2"
"$pub" sign --key "$A/publisher.key" --dir "$work/a-v2" >/dev/null || fail "sign version 2"
run anchor-v2 0 "$pub" anchor --dir "$work/a-v2" "${CHAIN[@]}" --gas-key "$A/publisher.key"
final
run upload-v2 1 "$pub" upload --dir "$work/a-v2" --node "$NODE"
grep -q 'bulk_publisher_paused' "$work/upload-v2.out" "$work/upload-v2.err" || { cat "$work/upload-v2.err" >&2; fail "the paused publisher's version 2 was not refused as paused"; }
: >"$work/none.txt"
catalog 1 --dirs "$work/none.txt" --prev "$work/catalog0.bin" --remove river-levels.daily
code=$(curl -s -o "$work/gone.json" -w '%{http_code}' "$NODE/v1/bulk/$ID/river-levels.daily/rows?limit=5")
[ "$code" = 410 ] || fail "the removed dataset answers $code, not 410 (gone)"
pass "paused: version 2 refused (bulk_publisher_paused), the tombstone catalog taken, the dataset no longer served ($code)"

echo "10. later requests: a raise, a stolen key (the new key first), a retirement"
# later NAME KEY SEQUENCE request-options...: the signed request in the site folder; its Keccak in $hash.
later() {
  local name=$1 key=$2 seq=$3
  shift 3
  run "$name" 0 "$pub" request --key "$A/$key" --publisher "$ID" --origin "$ORIGIN" --site "$A/site" "${NET[@]}" --sequence "$seq" "$@"
  hash=$(keccak_of "$name") || exit 1
}
act() {  # act NAME EXIT [book-reviewer request options]
  local name=$1 want=$2
  shift 2
  run "$name" "$want" "$reviewer_bin" request --request "$A/site" --origin "$ORIGIN" --hash "$hash" "${CHAIN[@]}" \
    --records "$records" "$@"
}
now=$(date +%s) n1=$(nonce)
# A raise: the reviewer names the new limit; the ask alone grants nothing, and no limit goes down.
later raise-a publisher.key 1 --kind raise --quota-gib 3 --datasets 32
act raise-unnamed 2 --limits "$limits" --max-gib "$MAX_GIB"
erred raise-unnamed 'are required for a raise'
act raise-over 1 --limits "$limits" --max-gib "$MAX_GIB" --quota-gib 4 --datasets 32
act raise-full 1 --limits "$limits" --max-gib 3 --quota-gib 3 --datasets 32
said raise-full 'NOT TAKEN: no room'
act raise-dry 0 --limits "$limits" --max-gib "$MAX_GIB" --quota-gib 2 --datasets 32 --dry-run
[ "$(field "$limits" publishers.0.quota_gib)" = 1 ] || fail "the dry run of a raise wrote the limits file"
act raise 0 --limits "$limits" --max-gib "$MAX_GIB" --quota-gib 2 --datasets 32
said raise '^TAKEN raise'
[ "$(field "$limits" publishers.0.quota_gib)" = 2 ] && [ "$(field "$limits" publishers.0.state)" = paused ] \
  || fail "the raise is not in the limits file, or it lifted the pause"
[ "$(nonce)" = "$n1" ] || fail "a raise sent a transaction"
# The spare key is stolen: its own request, and a keys document that no longer lists it. The id has
# another active key (the working key), so the theft is marked.
"$pub" keys-doc --key "$A/publisher.key" --publisher "$ID" --out "$A/site/.well-known/onyx-source-keys.json" >/dev/null 2>&1 || fail "keys-doc without the spare"
later stolen-spare spare.key 2 --kind key-stolen --at $((now - 600))
act stolen-nokey 2
erred stolen-nokey 'reviewer-key is required'
act stolen-dry 0 --reviewer-key "$work/reviewer.key" --dry-run
said stolen-dry 'DRY RUN: would send markCompromised'
[ "$(nonce)" = "$n1" ] || fail "the dry run of a theft sent a transaction"
act stolen-spare-act 0 --reviewer-key "$work/reviewer.key"
said stolen-spare-act '^SENT markCompromised tx='
[ "$(nonce)" = $((n1 + 1)) ] || fail "the theft is not exactly one transaction"
final
# The working key is stolen too, and no other active key is left: the new key comes first.
"$pub" keys-doc --key "$A/spare.key" --publisher "$ID" --out "$A/site/.well-known/onyx-source-keys.json" >/dev/null 2>&1 || fail "keys-doc without the working key"
later stolen-last publisher.key 3 --kind key-stolen --at $((now - 600))
act stolen-last-act 1 --reviewer-key "$work/reviewer.key"
said stolen-last-act 'NOT SENT: .* has no other active key'
[ "$(nonce)" = $((n1 + 1)) ] || fail "a theft of the last key was sent before a new key was registered"
# The working key retires itself from now on: sent once, and the publisher has no key left.
later retire-a publisher.key 4 --kind retire-key --at "$now"
act retire-act 0 --reviewer-key "$work/reviewer.key"
said retire-act '^SENT retireKey tx='
act retire-again 1 --reviewer-key "$work/reviewer.key"
said retire-again 'NOT TAKEN'
[ "$(nonce)" = $((n1 + 2)) ] || fail "the retirement is not exactly one transaction"
final
run checkp-later 0 checkp --write --limits "$limits" --max-gib "$MAX_GIB" --rpc "$RPC"
[ "$(field "$status" publishers.0.state)" = retired ] && [ "$(field "$status" publishers.0.signers)" = "[]" ] \
  || fail "the status list does not show the publisher retired without a key"
pass "a raise needs its named limit (2 GiB written, the pause kept); a theft is marked while another key is active and refused for the last key; a retirement is sent once"

echo "11. the public files pass the repository's secret scanner"
# The application, the request, the keys document, the record and the status list are public and go
# through the public snapshot's gitleaks run: no field name may read as a credential there (a
# field named after a key with a hex value did). check-publishers scans the record in every run above.
if command -v gitleaks >/dev/null 2>&1; then
  mkdir -p "$work/public"
  cp -a "$A/site/.well-known" "$work/public/origin-a"
  cp -a "$C/site/.well-known" "$work/public/origin-c"
  cp -a "$records" "$work/public/records"
  cp "$status" "$work/public/publishers.json"
  run gitleaks 0 gitleaks dir "$work/public" --config "$REPO/.gitleaks.toml" --redact --no-banner --exit-code 1
  pass "gitleaks: no finding in $(find "$work/public" -type f | wc -l) public files (applications, requests, keys documents, the record, the status list)"
else
  pass "skipped: gitleaks is not installed"
fi

echo "PUBLISHER ONBOARDING REHEARSAL PASSED in $(($(date +%s) - started)) s"
