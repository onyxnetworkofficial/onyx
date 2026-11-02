#!/usr/bin/env bash
# CL7 (ADR-0057 item 7): a correction market settled by a real recheck round, the recheck runner
# end to end on a LOCAL anvil (chain id 31337; never a Base Sepolia transaction):
# 1. OnyxRegistry next to a stand-in ledger; the original round of the live manifest source
#    us.treasury.tga_total_deposits (record date 2026-10-27, its recording) runs through the
#    shared rehearsal: five simulated observers drawn from the default nine.
# 2. The paired `.recheck` metric is registered with its real template id (`onyx-source recheck`,
#    7 days after the last observable day, for 7 days); tONYX and OnyxCorrections are deployed
#    and the pair is set.
# 3. A market opens on the recorded value; two bettors bet, and so does a member of the rehearsal
#    pool that is no original observer.
# 4. The runner's plan (`level2-manifest recheck-plan`) waits while trading is open; after the
#    close it gives the first recheck window and a 16-member rehearsal pool without the five
#    original observers and the bettor: 10 members.
# 5. The recheck round runs on the paired recheck (`demo --recheck 7:7 --pool <plan pool>`); its
#    stored transcript holds none of the six left out.
# 6. `recheck-resolve` settles the market from the recheck's results store (the source still says
#    the same: Yes); the Yes bettor withdraws its stake plus the No pool less the fee.
# Keys: anvil's public dev accounts and fresh throwaway keys; the rehearsal observers' keys are
# public test keys. Env: FOUNDRY_BIN (~/.foundry/bin), CARGO_TARGET_DIR.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
manifest=sources/us.treasury.tga_total_deposits/manifest.json
date=2026-10-27
work=$(mktemp -d)
anvil_pid=""
cleanup() {
  if [ -n "$anvil_pid" ]; then
    kill "$anvil_pid" 2>/dev/null || true
    wait "$anvil_pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP
fail() { echo "RECHECK ANVIL FAILED: $*"; exit 1; }
(cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/forge.log" 2>&1) \
  || { tail -20 "$work/forge.log"; fail "forge build"; }
# --workspace (not -p): the same feature set as check-all's `cargo test --workspace`, so these
# targets reuse its build instead of compiling a second variant of the shared crates.
cargo build --locked -q --workspace --example level2-manifest --example onyx-source
bin=$work/bin
mkdir "$bin"
cp "${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/level2-manifest" \
  "${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/onyx-source" "$bin/"
lm() { "$bin/level2-manifest" "$@" --manifest "$manifest" --repo "$REPO"; }
"$bin/onyx-source" recheck "$manifest" --after-days 7 --span-days 7 --date "$date" >"$work/ids.txt"
id() { awk -v k="$1" '$1 == k {print $2}' "$work/ids.txt"; }
metric=$(id original_metric_key); rmetric=$(id recheck_metric_key); rtemplate=$(id recheck_template_id)
tolerance=$(id tolerance); subject=$(id subject_key)
for v in "$metric" "$rmetric" "$rtemplate" "$tolerance" "$subject"; do
  [ -n "$v" ] || fail "onyx-source recheck printed no ids"
done
(cd "$work" && exec "$FOUNDRY_BIN/anvil" --host 127.0.0.1 --port 0 --accounts 10 >"$work/anvil.log" 2>&1) &
anvil_pid=$!
port=""
for _ in $(seq 200); do
  kill -0 "$anvil_pid" 2>/dev/null || { cat "$work/anvil.log"; fail "anvil exited"; }
  port=$(grep -oE 'Listening on 127\.0\.0\.1:[0-9]+' "$work/anvil.log" | head -n1 | sed 's/.*://' || true)
  [ -n "$port" ] && break
  sleep 0.1
done
[ -n "$port" ] || fail "anvil did not report its port"
RPC=http://127.0.0.1:$port
c() { local sub=$1; shift; (cd "$work" && "$FOUNDRY_BIN/cast" "$sub" --rpc-url "$RPC" "$@"); }
[ "$(c chain-id)" = 31337 ] || fail "not a local anvil"
call() { c call "$@" | awk '{print $1}'; }
MARKET_SIG='market(uint256)((bytes32,bytes32,bytes32,bytes32,address,uint64,uint64,uint64,uint8,uint8,uint8,uint32,uint32,int128,int128,uint128,uint128,uint128,uint128,uint128,uint128,uint128[2],uint32[2]))'
sendu() { local from=$1; shift; c send --unlocked --from "$from" "$@" --json >/dev/null; }
# Anvil's public dev accounts 0 and 1: the coordinator and the registry owner (as
# scripts/level2-manifest-anvil.sh); account 2 deploys the market contracts and owns the pair.
COORD=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266
OWNER=0x70997970C51812dc3A010C7d01b50e0d17dc79C8
DEPLOYER=0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC
LEDGER=0x00000000000000000000000000000000000c0de1
c rpc anvil_setCode "$LEDGER" \
  "0x60003560e01c63d7d85a5714602c5773${COORD#0x}60005260206000f35b60206000f3" >/dev/null
bytecode() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["bytecode"]["object"])' "$REPO/contracts/out/$1"; }
enc() { (cd "$work" && "$FOUNDRY_BIN/cast" abi-encode "$@") | sed 's/^0x//'; }
deploy() { # from, artifact, abi-encoded constructor args
  c send --unlocked --from "$1" --create "$(bytecode "$2")$3" --json \
    | python3 -I -c 'import json,sys; r=json.load(sys.stdin); assert r["status"]=="0x1", r; print(r["contractAddress"])'
}
REG=$(deploy "$OWNER" OnyxRegistry.sol/OnyxRegistry.json "$(enc 'f(address,address)' "$OWNER" "$LEDGER")")
chain=(--rpc "$RPC" --registry "$REG" --cast "$FOUNDRY_BIN/cast")
# 1. The original round.
lm demo "${chain[@]}" --record-rounds --coordinator "$COORD" --owner "$OWNER" --record-date "$date" \
  --book "$work/book.json" --store "$work/original.json" --new-store --out "$work/original-summary.json" \
  2>"$work/original.log" || { tail -20 "$work/original.log"; fail "the original round"; }
read -r round value <<<"$(python3 -I -c 'import json,sys; s=json.load(open(sys.argv[1])); print(s["round_id"], s["value"])' "$work/original-summary.json")"
echo "original round $round: $date = $value (million USD), recorded"
# 2. The recheck metric, tONYX, OnyxCorrections and the pair.
sendu "$OWNER" "$REG" 'registerMetric(bytes32,uint8,bytes32,uint8)' "$rmetric" 3 "$rtemplate" 0
newkey() { python3 -I -c 'import secrets; print("0x" + secrets.token_hex(32))'; }
addr() { (cd "$work" && "$FOUNDRY_BIN/cast" wallet address --private-key "$1"); }
declare -A K A
for who in opener yes1 no1 treasury; do
  K[$who]=$(newkey)
  A[$who]=$(addr "${K[$who]}")
  c rpc anvil_setBalance "${A[$who]}" 0x56BC75E2D63100000 >/dev/null
done
sendk() { local k=$1; shift; c send --private-key "${K[$k]}" "$@" --json >/dev/null; }
token=$(deploy "$DEPLOYER" OnyxTestToken.sol/OnyxTestToken.json "$(enc 'f(address)' "$DEPLOYER")")
# Demo windows in blocks: trading >= 5, the recheck >= 5, a market <= 1,000 blocks, 10 grace blocks.
corrections=$(deploy "$DEPLOYER" OnyxCorrections.sol/OnyxCorrections.json "$(enc \
  'f(address,address,address,(uint128,uint128,uint128,uint128,uint32,uint32,uint32,uint32,uint32))' \
  "$token" "$REG" "${A[treasury]}" '(5000000000000000000,1000000000000000000,100000000000000000000,10000000000000000000000,20000,5,5,1000,10)')")
sendu "$DEPLOYER" "$corrections" 'pair(bytes32,bytes32,uint128)' "$metric" "$rmetric" "$tolerance"
echo "deployed: tONYX $token, OnyxCorrections $corrections; $rmetric paired"
# 3. The market and the bets; one bettor is a rehearsal pool member (impersonated: anvil only).
lm rehearsal-pool --size 16 >"$work/pool16.json"
lm rehearsal-pool --size 9 >"$work/pool9.json"
member=$(python3 -I -c 'import json,sys; a={m["address"].lower() for m in json.load(open(sys.argv[1]))}; b=[m["address"] for m in json.load(open(sys.argv[2])) if m["address"].lower() not in a]; print(b[0])' "$work/pool9.json" "$work/pool16.json")
c rpc anvil_impersonateAccount "$member" >/dev/null
c rpc anvil_setBalance "$member" 0x56BC75E2D63100000 >/dev/null
for who in opener yes1 no1; do
  sendu "$DEPLOYER" "$token" 'mint(address,uint256)' "${A[$who]}" 100000000000000000000
  sendk "$who" "$token" 'approve(address,uint256)' "$corrections" 100000000000000000000
done
sendu "$DEPLOYER" "$token" 'mint(address,uint256)' "$member" 100000000000000000000
sendu "$member" "$token" 'approve(address,uint256)' "$corrections" 100000000000000000000
b=$(c block-number); close=$(( b + 12 )); resolve=$(( close + 400 ))
sendk opener "$corrections" 'open(bytes32,bytes32,uint64,bytes32,int128,uint64,uint64)' "$metric" "$subject" 0 "$round" "$value" "$close" "$resolve"
sendk yes1 "$corrections" 'bet(uint256,uint8,uint128)' 1 1 30000000000000000000
sendk no1 "$corrections" 'bet(uint256,uint8,uint128)' 1 2 10000000000000000000
sendu "$member" "$corrections" 'bet(uint256,uint8,uint128)' 1 2 5000000000000000000
echo "market 1 open until block $close: Yes 30 tONYX, No 15 tONYX (one No bettor is pool member $member)"
# 4. The runner's plan: wait, then run.
plan() {
  lm recheck-plan --recheck 7:7 --record-date "$date" --rpc "$RPC" --cast "$FOUNDRY_BIN/cast" \
    --corrections "$corrections" --margin-blocks 5 --pool "$work/pool16.json" \
    --original-store "$work/original.json" --out-pool "$work/recheck-pool.json"
}
p=$(plan) || fail "recheck-plan"
python3 -I -c 'import json,sys; p=json.loads(sys.argv[1]); assert (p["state"], p["served"], p["bettors"]) == ("wait", [1], 3), p' "$p" \
  || fail "the plan before the close: $p"
now=$(c block-number)
[ "$now" -le "$close" ] && c rpc anvil_mine "$(printf '0x%x' $(( close - now + 1 )))" >/dev/null
p=$(plan) || fail "recheck-plan after the close"
python3 -I -c 'import json,sys; p=json.loads(sys.argv[1]); assert (p["state"], p["served"], p["pool"], p["excluded"]) == ("run", [1], 10, 6), p' "$p" \
  || fail "the plan after the close: $p"
start=$(python3 -I -c 'import json,sys; print(json.loads(sys.argv[1])["windowStart"])' "$p")
echo "plan: run the recheck in the window from $start, 10 of 16 pool members (5 original observers and 1 bettor left out)"
# 5. The recheck round from the plan's pool. The chain's clock moves past its window first (as the
# original round's window was past when it ran): the rehearsal replays its observers' clock and
# never waits for the window.
c rpc evm_setNextBlockTimestamp "$(( start + 2 * 86400 ))" >/dev/null
c rpc anvil_mine 0x1 >/dev/null
lm demo --recheck 7:7 "${chain[@]}" --record-rounds --coordinator "$COORD" --record-date "$date" \
  --window-start "$start" \
  --pool "$work/recheck-pool.json" --book "$work/rbook.json" --store "$work/recheck.json" --new-store \
  --out "$work/recheck-summary.json" 2>"$work/recheck.log" \
  || { tail -20 "$work/recheck.log"; fail "the recheck round"; }
python3 -I - "$work/original.json" "$work/recheck.json" "$member" <<'PY' || fail "the recheck's pool"
import json, sys
def rounds(path):
    s = json.load(open(path))
    return s["rounds"] if isinstance(s, dict) and "rounds" in s else s
def addrs(xs):
    return {x["address"].lower() if isinstance(x, dict) else str(x).lower() for x in xs}
orig = rounds(sys.argv[1])[-1]
rech = rounds(sys.argv[2])[-1]
left_out = addrs(orig["observers"]) | {sys.argv[3].lower()}
pool = addrs(rech["pool"])
assert len(pool) == 10 and not (pool & left_out), (sorted(pool), sorted(left_out))
PY
echo "recheck round recorded; its pool holds none of the 6 left out"
# 6. Resolve and pay.
r=$(lm recheck-resolve --recheck 7:7 --record-date "$date" --rpc "$RPC" --cast "$FOUNDRY_BIN/cast" \
  --corrections "$corrections" --store "$work/recheck.json" --from "$DEPLOYER") || fail "recheck-resolve"
python3 -I -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["markets"] == [{"market": 1, "resolved": True}], r' "$r" \
  || fail "resolve: $r"
state=$(c call "$corrections" "$MARKET_SIG" 1 --json)
python3 -I -c 'import json,sys; m=json.loads(sys.argv[1])[0]; m = m if isinstance(m, list) else json.loads(m); assert (int(m[8]), int(m[9])) == (2, 1), m' "$state" \
  || fail "the market is not resolved Yes: $state"
before=$(call "$token" 'balanceOf(address)(uint256)' "${A[yes1]}")
sendk yes1 "$corrections" 'withdraw(uint256)' 1
after=$(call "$token" 'balanceOf(address)(uint256)' "${A[yes1]}")
python3 -I -c 'import sys; got = int(sys.argv[2]) - int(sys.argv[1]); assert got == 30 * 10**18 + 15 * 10**18 * 98 // 100, got' "$before" "$after" \
  || fail "the Yes payout"
echo "resolved Yes from the recheck (round $(python3 -I -c 'import json,sys; print(json.loads(sys.argv[1])["round"])' "$r")); yes1 +44.7 tONYX"
echo "RECHECK ANVIL PASSED"
