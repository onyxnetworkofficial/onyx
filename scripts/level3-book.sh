#!/usr/bin/env bash
# ROADMAP C10 rehearsal (ADR-0028 amendment 7): the level-3 chain book wired end to end on an
# anvil, with anvil dev keys only.
#
#   scripts/level3-book.sh local   a local anvil (chain 31337); in scripts/check-all.sh
#   scripts/level3-book.sh fork    an anvil fork of Base Sepolia (chain 84532, ONYX_FORK_RPC,
#                                  default https://sepolia.base.org); nothing is broadcast to the
#                                  network
#
# Steps: start its own anvil (port picked by anvil, stopped on any exit); deploy the settlement
# (Deploy.s.sol, deployer = dev key 0, the coordinator) and write a manifest; deploy the book with
# the deployer as reviewer (must be refused, manifest unchanged: THREATS C10-1); --plan with the
# real reviewer (sends nothing, review C10-R2-3); deploy and pin
# it with dev account 1 as reviewer (scripts/deploy-signed-book.sh anvil); start onyx-node with a
# manifest pinning a wrong reviewer, then a wrong code hash (each must refuse to start); start
# onyx-node with the pin and ONYX_SIGNED_REPORTS; run examples/level3-book-anvil.rs (dev
# account 2 is the coordinator): posts, chain-time store, finalized-only serving, the serving
# margin, a held-back contradiction, the delayed-post breaker (and /v1/health's level3, ROADMAP
# C62), a skipped post, clock skew; then examples/signed-client.rs (apps/web/verify, ROADMAP C61):
# the client checks F2 against its own read of the book at its finalized block.
# Prints LEVEL3 BOOK <MODE> PASSED. Env: FOUNDRY_BIN (~/.foundry/bin), CARGO_TARGET_DIR,
# ONYX_FORK_RPC.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
mode=${1:-}
case "$mode" in
  local | fork) ;;
  *) echo "usage: $0 local|fork" >&2; exit 2 ;;
esac
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
target=${CARGO_TARGET_DIR:-$REPO/target}
work=$(mktemp -d)
anvil_pid="" node_pid="" bad_pids=()
stop() {
  local p=$1
  [ -n "$p" ] || return 0
  kill "$p" 2>/dev/null || true
  wait "$p" 2>/dev/null || true
}
cleanup() {
  for p in "${bad_pids[@]:-}"; do stop "$p"; done
  stop "$node_pid"
  stop "$anvil_pid"
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

cargo build --locked -q -p onyx-node --bin onyx-node --example pin-signed-book
cargo build --locked -q -p onyx-coordinator --example level3-book-anvil
cargo build --locked -q -p onyx-web-verify --example signed-client
cargo build --locked -q -p onyx-api --example submit-review
cargo build --locked -q -p onyx-source-fiscaldata --example feed-approved --example check-observation
(cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/forge.log" 2>&1) \
  || { cat "$work/forge.log"; fail "forge build"; }

# Port 0: anvil binds a free port itself and prints it (review TR-R4-3).
if [ "$mode" = fork ]; then
  fork_args=(--fork-url "${ONYX_FORK_RPC:-https://sepolia.base.org}")
else
  fork_args=()
fi
(cd "$work" && exec "$FOUNDRY_BIN/anvil" --host 127.0.0.1 --port 0 "${fork_args[@]}" \
  --config-out "$work/anvil.json" >"$work/anvil.log" 2>&1) &
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
case "$mode:$chain" in
  local:31337 | fork:84532) ;;
  *) fail "$mode anvil answers chain $chain" ;;
esac
acct() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["available_accounts"][int(sys.argv[2])].lower())' "$work/anvil.json" "$1"; }
deployer=$(acct 0) reviewer=$(acct 1) coordinator=$(acct 2) other=$(acct 3)
key0=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["private_keys"][0])' "$work/anvil.json")
echo "anvil $mode, chain $chain; deployer $deployer, reviewer $reviewer, coordinator $coordinator"

echo "1. settlement (Deploy.s.sol, dev key 0) and its manifest"
cp -a "$REPO/contracts" "$work/contracts"
rm -rf "$work/contracts/broadcast"
sed -i "s#$REPO/contracts#$work/contracts#g" "$work/contracts/cache/solidity-files-cache.json"
(cd "$work/contracts" && BASE_SEPOLIA_PRIVATE_KEY=$key0 ETH_RPC_URL=$RPC \
  "$FOUNDRY_BIN/forge" script script/Deploy.s.sol:Deploy --rpc-url "$RPC" --broadcast --slow \
  >"$work/deploy.log" 2>&1) || { tail -20 "$work/deploy.log"; fail "settlement deploy"; }
unset key0
bc=$work/contracts/broadcast/Deploy.s.sol/$chain/run-latest.json
settlement=$(python3 -I -c 'import json,sys
v=json.load(open(sys.argv[1]))
print(next(t["contractAddress"] for t in v["transactions"] if t["contractName"]=="OnyxSettlement" and t["transactionType"]=="CREATE").lower())' "$bc")
[ "$(castw call "$settlement" 'coordinator()(address)' | tr '[:upper:]' '[:lower:]')" = "$deployer" ] \
  || fail "the settlement's coordinator is not the deployer"
manifest=$work/deployment.json
python3 -I - "$manifest" "$chain" "$settlement" "$(castw codehash "$settlement")" <<'PY'
import json, sys
path, chain, st, h = sys.argv[1:]
json.dump({"chainId": int(chain), "settlement": st, "settlementCodeHash": h}, open(path, "w"), indent=2)
PY
cp "$manifest" "$work/manifest.before"
nonce_before=$(castw nonce "$deployer")
pass "settlement $settlement, manifest written"

echo "2. THREATS C10-1: the deployer as reviewer is refused before anything is sent"
if scripts/deploy-signed-book.sh anvil --rpc "$RPC" --anvil-config "$work/anvil.json" \
  --reviewer "$deployer" --manifest "$manifest" >"$work/refused.log" 2>&1; then
  cat "$work/refused.log"; fail "a book with the deployer as reviewer was deployed"
fi
# The deployer is the settlement's coordinator: refused (C10-R1-5) by the coordinator() check before the dry
# run (the dry run's own ReviewerIsDeployer is covered by DeploySignedBook.t.sol).
grep -qE "refused \(THREATS C10-1\); nothing sent|dry run refused" "$work/refused.log" \
  || { cat "$work/refused.log"; fail "no refusal before sending"; }
cmp -s "$manifest" "$work/manifest.before" || fail "the refused deploy changed the manifest"
[ "$(castw nonce "$deployer")" = "$nonce_before" ] \
  || fail "the refused deploy sent a transaction"
pass "refused before sending ($(grep -oE 'settlement.s coordinator|ReviewerIsDeployer' "$work/refused.log" | head -1)); manifest and deployer nonce unchanged"

# Review C10-R2-3: --plan (the runbook's first step) simulates and sends nothing.
ONYX_PIN_BIN=$target/debug/examples/pin-signed-book scripts/deploy-signed-book.sh anvil \
  --rpc "$RPC" --anvil-config "$work/anvil.json" --reviewer "$reviewer" --manifest "$manifest" \
  --plan >"$work/plan.log" 2>&1 || { cat "$work/plan.log"; fail "--plan"; }
for want in '^1\. dry run \(simulation; nothing sent\)$' 'Estimated total gas used for script' \
  'Estimated amount required' '^PLAN ONLY: nothing sent'; do
  grep -qE "$want" "$work/plan.log" || { cat "$work/plan.log"; fail "--plan printed no '$want'"; }
done
if grep -qE '^2\. deploy|OnyxSignedBook 0x' "$work/plan.log"; then cat "$work/plan.log"; fail "--plan deployed"; fi
cmp -s "$manifest" "$work/manifest.before" || fail "--plan changed the manifest"
[ "$(castw nonce "$deployer")" = "$nonce_before" ] || fail "--plan sent a transaction"
[ ! -e "$manifest.signed-book-broadcast.json" ] || fail "--plan left a broadcast"
sed 's/^/    | /' "$work/plan.log"
pass "--plan: dry run and gas printed; manifest, deployer nonce unchanged; nothing sent"

echo "3. deploy and pin the book (reviewer: dev account 1)"
ONYX_PIN_BIN=$target/debug/examples/pin-signed-book scripts/deploy-signed-book.sh anvil \
  --rpc "$RPC" --anvil-config "$work/anvil.json" --reviewer "$reviewer" --manifest "$manifest" \
  | tee "$work/pin.log"
grep -q '^SIGNED BOOK DEPLOYED AND PINNED$' "$work/pin.log" || fail "deploy-signed-book.sh"
book=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["signedBook"])' "$manifest")
book_hash=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["signedBookCodeHash"])' "$manifest")
[ "$(castw codehash "$book" | tr '[:upper:]' '[:lower:]')" = "$book_hash" ] || fail "pinned hash is not the chain's"
[ "$(castw call "$book" 'reviewer()(address)' | tr '[:upper:]' '[:lower:]')" = "$reviewer" ] || fail "reviewer()"
pass "book $book pinned (code hash and reviewer read back)"
# Review C10-R1-4: a pin that failed after the deploy resumes from the kept broadcast, sending nothing.
cp "$work/manifest.before" "$work/resume.json"
ONYX_PIN_BIN=$target/debug/examples/pin-signed-book scripts/deploy-signed-book.sh anvil \
  --rpc "$RPC" --anvil-config "$work/anvil.json" --reviewer "$reviewer" --manifest "$work/resume.json" \
  --pin-only "$manifest.signed-book-broadcast.json" >"$work/resume.log" 2>&1 \
  || { cat "$work/resume.log"; fail "--pin-only"; }
grep -q '^SIGNED BOOK PINNED$' "$work/resume.log" || { cat "$work/resume.log"; fail "--pin-only"; }
[ "$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["signedBook"])' "$work/resume.json")" = "$book" ] \
  || fail "--pin-only pinned another book"
pass "--pin-only resumed from the kept broadcast: the same book pinned, nothing sent"

# The node reads the settlement's code at the finalized block (anvil: 64 blocks behind the head).
castw rpc anvil_mine 70 >/dev/null

echo "4. onyx-node refuses a broken pin"
mkdir -p "$work/cas" "$work/state"
printf '{"version": 1, "shards": []}\n' >"$work/catalog.json"
python3 -I - "$work/posts.json" "$chain" "$book" <<'PY'
import json, sys
path, chain, book = sys.argv[1:]
json.dump({"version": 1, "chainId": int(chain), "book": book, "reports": []}, open(path, "w"))
PY
free_port() { python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'; }
# The node's whole environment (env -i: nothing from this shell leaks in). $1 manifest, $2 port.
node_cmd() {
  cmd=(env -i PATH="$PATH" HOME="$HOME" ONYX_ENV=dev ONYX_RIGHTS=cleared ONYX_PAYMENTS=off
    ONYX_LISTEN="127.0.0.1:$2" ONYX_PUBLIC_URL="http://127.0.0.1:$2/" ONYX_RPC_URL="$RPC"
    ONYX_DEPLOYMENT="$1" ONYX_CATALOG="$work/catalog.json" ONYX_CAS_DIR="$work/cas"
    ONYX_STATE_DIR="$work/state" ONYX_SIGNED_REPORTS="$work/posts.json"
    ONYX_SUBMISSIONS="$work/submissions.journal" "$target/debug/onyx-node")
}
# A manifest with one edit; onyx-node must exit non-zero within its start tries (about 32 s).
bad_pids=() bad_names=()
bad_start() {
  local name=$1 edit=$2
  python3 -I - "$manifest" "$work/$name.json" "$edit" "$other" <<'PY'
import json, sys
src, dst, edit, other = sys.argv[1:]
v = json.load(open(src))
exec(edit)
json.dump(v, open(dst, "w"))
PY
  node_cmd "$work/$name.json" "$(free_port)"
  timeout 90 "${cmd[@]}" >"$work/$name.log" 2>&1 &
  bad_pids+=($!)
  bad_names+=("$name")
}
bad_start wrong-reviewer 'v["signedBookReviewer"] = other'
bad_start wrong-hash 'v["signedBookCodeHash"] = "0x" + "00" * 32'
bad_start book-is-settlement 'v["signedBook"] = v["settlement"]; v["signedBookCodeHash"] = v["settlementCodeHash"]'
for i in "${!bad_pids[@]}"; do
  name=${bad_names[$i]}
  set +e
  wait "${bad_pids[$i]}"
  code=$?
  set -e
  [ "$code" != 0 ] && [ "$code" != 124 ] || { cat "$work/$name.log"; fail "$name: onyx-node did not refuse (exit $code)"; }
  if grep -q 'pinned (code hash and reviewer' "$work/$name.log"; then fail "$name: onyx-node pinned a broken book"; fi
  why=$(grep -E '^onyx-node: signed book 0x[0-9a-f]*: ' "$work/$name.log" | tail -1 || true)
  [ -n "$why" ] || { cat "$work/$name.log"; fail "$name: onyx-node stopped, but not at the signed book check"; }
  pass "$name: refused (exit $code): ${why#onyx-node: }"
done
bad_pids=()

echo "5. onyx-node with the pin and the coordinator's post log"
node_port=$(free_port)
node_cmd "$manifest" "$node_port"
"${cmd[@]}" >"$work/node.log" 2>&1 &
node_pid=$!
for _ in $(seq 300); do
  kill -0 "$node_pid" 2>/dev/null || { cat "$work/node.log"; fail "onyx-node exited"; }
  curl -sf -A onyx-dev "http://127.0.0.1:$node_port/v1/health" >/dev/null 2>&1 && break
  sleep 0.2
done
grep -q "signed book $book pinned (code hash and reviewer checked)" "$work/node.log" \
  || { cat "$work/node.log"; fail "onyx-node started without the book pin"; }
pass "onyx-node up on 127.0.0.1:$node_port with the book pinned"

echo "6. the coordinator and the API (examples/level3-book-anvil.rs)"
"$target/debug/examples/level3-book-anvil" --rpc "$RPC" --cast "$FOUNDRY_BIN/cast" \
  --book "$book" --reviewer "$reviewer" --coordinator "$coordinator" --log "$work/posts.json" \
  --node "http://127.0.0.1:$node_port" 2>&1 | tee "$work/run.log"
grep -q '^LEVEL3 BOOK ANVIL PASSED$' "$work/run.log" || fail "rehearsal"

# ROADMAP C61: the client does not take the node's word. It reads settleable itself at its own
# RPC's finalized block (here the same anvil; the browser uses a public RPC it chooses) and refuses
# a forged value and a pin naming another book or build; every read at the block asks
# requireCanonical (review C61-R1-7). ONYX_LEVEL3_EXPORT=FILE also writes the
# website's recorded rehearsal (apps/web/static/level3/signed-rehearsal.json).
echo "7. the client checks F2 against its own read of the book (signed-client)"
fact=$(grep -oE '^FACT F2 0x[0-9a-f]{64}$' "$work/run.log" | awk '{print $3}')
[ -n "$fact" ] || fail "the rehearsal named no fact F2"
export_args=()
[ -n "${ONYX_LEVEL3_EXPORT:-}" ] && export_args=(--export "$ONYX_LEVEL3_EXPORT")
"$target/debug/examples/signed-client" --rpc "$RPC" --node "http://127.0.0.1:$node_port" \
  --fact "$fact" --chain-id "$chain" --book "$book" --code-hash "$book_hash" \
  --clock-stand-in 5460 "${export_args[@]}" 2>&1 | tee "$work/client.log"
grep -q '^SIGNED CLIENT CHECK PASSED$' "$work/client.log" || fail "client check"
# U2: exercise intake on the actual guarded node, then stop/review/restart.
echo "8. durable intake and maintenance review (ADR-0039)"
python3 -I - "$work/submission.json" <<'PYDATA'
import json,sys
with open('fixtures/intake/vectors.json') as f: example=json.load(f)['example']
with open(sys.argv[1],'w') as f: json.dump(example,f)
PYDATA
api="http://127.0.0.1:$node_port"
code=$(curl -sS --max-time 20 -o "$work/receipt.json" -w '%{http_code}' -H 'Content-Type: application/json' --data-binary "@$work/submission.json" "$api/v1/submit")
[ "$code" = 201 ] || fail "intake POST: $code"
read -r receipt_id receipt_hash < <(python3 -I - "$work/receipt.json" <<'PYDATA'
import json,re,sys
r=json.load(open(sys.argv[1])); assert r['status']=='pending_review'
assert re.fullmatch(r'sub_[0-9a-f]{32}',r['id'])
print(r['id'],r['hash'])
PYDATA
)
[ -n "$receipt_id" ] && [ -n "$receipt_hash" ] || fail "invalid receipt"
python3 -I - "$work/oversize.json" <<'PYDATA'
import sys
open(sys.argv[1],'w').write(' ' * (64*1024+1))
PYDATA
code=$(curl -sS --max-time 20 -o /dev/null -w '%{http_code}' -H 'Content-Type: application/json' --data-binary "@$work/oversize.json" "$api/v1/submit")
[ "$code" = 413 ] || fail "node intake body cap: $code"
if "$target/debug/examples/submit-review" "$work/submissions.journal" list >"$work/locked-review.log" 2>&1; then fail "reviewer bypassed the node lock"; fi
stop "$node_pid"; node_pid=""
"$target/debug/examples/submit-review" "$work/submissions.journal" show "$receipt_id" >"$work/private-review.json"
"$target/debug/examples/submit-review" "$work/submissions.journal" decide "$receipt_id" 0 "$receipt_hash" approved reviewed >"$work/decision.json"
# U4 / pre-public PP1: feed the approved receipt to the pinned observer round (recorded fetches,
# simulated registry); the restarted node then shows the round's public evidence on the receipt.
"$target/debug/examples/feed-approved" "$work/submissions.journal" "$receipt_id" --out "$work/fed-store.json" >"$work/fed.json"
node_cmd "$manifest" "$node_port"
"${cmd[@]}" >"$work/node-restarted.log" 2>&1 &
node_pid=$!
for _ in $(seq 300); do
  kill -0 "$node_pid" 2>/dev/null || fail "review restart exited"
  curl -sf --max-time 2 "$api/v1/health" >/dev/null 2>&1 && break
  sleep 0.2
done
curl -sf --max-time 20 -D "$work/status.headers" "$api/v1/submit/$receipt_id" >"$work/status.json" || fail "receipt after restart"
python3 -I - "$work/status.json" "$work/status.headers" "$receipt_id" "$work/fed.json" <<'PYDATA'
import json,sys
import re
s=json.load(open(sys.argv[1])); c=s.pop('check')
assert s=={'id':sys.argv[3],'status':'approved'}, s
assert sorted(c)==['level','recordDate','registry','result','roundId','value'] and c['level']==2 and c['registry']=='simulated', c
assert c['value']=='40305316210829.72', c
assert c['recordDate']=='2026-10-08' and re.fullmatch(r'0x[0-9a-f]{64}',c['roundId']) and re.fullmatch(r'0x[0-9a-f]{64}',c['result']), c
fed=json.load(open(sys.argv[4]))
assert (fed['roundId'],fed['result'])==(c['roundId'],c['result']), 'the node serves the worker evidence'
assert 'cache-control: no-store' in open(sys.argv[2]).read().lower()
PYDATA
pass "intake durable, bounded, local-review locked; approved and fed status survives restart with the observers' evidence and no private fields"
# U5 / PP4 hand-off: the node serves the fed round as a level-2 value (ONYX_OBSERVATIONS with the
# reviewed template pin), and a buyer checks it with its own copy of the template.
read -r obs_metric obs_template obs_subject < <(python3 -I - "$work/fed.json" <<'PYDATA'
import json,sys
f=json.load(open(sys.argv[1])); print(f['metric'], f['templateId'], f['subject'])
PYDATA
)
stop "$node_pid"; node_pid=""
node_cmd "$manifest" "$node_port"
last=$(( ${#cmd[@]} - 1 ))
cmd=("${cmd[@]:0:$last}" ONYX_OBSERVATIONS="$work/fed-store.json" ONYX_OBSERVATION_TEMPLATES="$obs_metric=$obs_template" "${cmd[$last]}")
"${cmd[@]}" >"$work/node-served.log" 2>&1 &
node_pid=$!
for _ in $(seq 300); do
  kill -0 "$node_pid" 2>/dev/null || { cat "$work/node-served.log"; fail "serving restart exited"; }
  curl -sf --max-time 2 "$api/v1/health" >/dev/null 2>&1 && break
  sleep 0.2
done
code=$(curl -sS --max-time 20 -o "$work/bought.json" -w '%{http_code}' "$api/v1/observations/$obs_metric?subject=$obs_subject")
[ "$code" = 200 ] || { cat "$work/bought.json"; fail "buyer GET /v1/observations: $code"; }
"$target/debug/examples/check-observation" "$work/bought.json" "${obs_subject##*:}" >"$work/buyer-check.json" || fail "the buyer's check refused the served value"
python3 -I - "$work/buyer-check.json" "$work/fed.json" "$work/status.json" <<'PYDATA'
import json,sys
b=json.load(open(sys.argv[1])); f=json.load(open(sys.argv[2])); s=json.load(open(sys.argv[3]))
assert b['checked'] and b['level']==2, b
assert (b['roundId'], b['result']) == (f['roundId'], f['result']) == (s['check']['roundId'], s['check']['result']), (b, f, s)
assert b['value'] == '40305316210829.72', b
assert s['check']['value'] == b['value'], 'PP11: the receipt shows the value the buyer checked'
PYDATA
pass "hand-off: the buyer's own check of the served value gives the receipt's round and result (40305316210829.72)"
# Optional connected browser rehearsal: uses the actual node above and leaves all artifacts local.
if [ "${ONYX_SUBMIT_BROWSER:-0}" = 1 ]; then
  node apps/web/tests/submission-live.mjs "$api" "$receipt_id"
fi
label=$(echo "$mode" | tr '[:lower:]' '[:upper:]')
echo "LEVEL3 BOOK $label PASSED"
