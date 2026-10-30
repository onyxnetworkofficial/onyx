#!/usr/bin/env bash
# Correction markets end to end on a local anvil (contributor loop CL7, ADR-0057).
# A published value, a market on it, bets on both sides, a recheck round that disagrees,
# settlement and payouts, checked to the wei on chain:
# 1. OnyxRegistry (owner: anvil's dev account; coordinator: a fresh key through a stand-in
#    ledger), the tONYX test token and OnyxCorrections are deployed.
# 2. The live manifest source us.treasury.tga_total_deposits and its paired `.recheck` metric
#    are registered with their real template ids (`onyx-source recheck`, onyx_core::recheck_of)
#    and paired on OnyxCorrections with the recheck template's tolerance.
# 3. The coordinator records the original round (record date 2026-10-28: 18463 million USD).
# 4. An opener opens the value's market; two bettors say Yes ("accurate"), two say No; an
#    insider's bet and a forged value are refused; a market opened after the recheck is refused.
# 5. After the close the coordinator draws and records the recheck round: 18470 (the source now
#    says otherwise). Resolving with the original round is refused; with the recheck it settles No.
# 6. The No bettors withdraw their stakes plus the Yes pool pro rata less the 2% fee; Yes gets
#    nothing; the treasury gets the fee, the rounding dust and the open fee; the contract ends
#    holding nothing.
# The rounds here are recorded by the demo coordinator key (no observer processes): the chain
# rules are what is shown (the observer round itself is the CL3 runner's, scripts/
# level2-manifest-anvil.sh). Local only: chain id 31337; every key is made fresh for the run and
# thrown away. cast runs in a temp dir outside the repo (Foundry loads a `.env` from its working
# directory). Env: FOUNDRY_BIN (~/.foundry/bin), CARGO_TARGET_DIR.
set -euo pipefail
cd "$(dirname "$0")/.."
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
repo=$PWD
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
fail() { echo "CORRECTIONS ANVIL FAILED: $*"; exit 1; }
(cd contracts && "$FOUNDRY_BIN/forge" build >"$work/forge.log" 2>&1) || { cat "$work/forge.log"; fail "forge build"; }
# The real ids of the live manifest and its recheck (7 days after the last observable day, 7 days).
manifest=sources/us.treasury.tga_total_deposits/manifest.json
cargo run --locked -q -p onyx-level2 --example onyx-source -- \
  recheck "$manifest" --after-days 7 --span-days 7 --date 2026-10-28 >"$work/ids.txt"
id() { awk -v k="$1" '$1 == k {print $2}' "$work/ids.txt"; }
metric=$(id original_metric_key); template=$(id original_template_id)
rmetric=$(id recheck_metric_key); rtemplate=$(id recheck_template_id)
tolerance=$(id tolerance); subject=$(id subject_key)
for v in "$metric" "$template" "$rmetric" "$rtemplate" "$tolerance" "$subject"; do
  [ -n "$v" ] || fail "onyx-source recheck printed no ids"
done
(cd "$work" && exec "$FOUNDRY_BIN/anvil" --host 127.0.0.1 --port 0 >"$work/anvil.log" 2>&1) &
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
deployer=$(c rpc eth_accounts | python3 -I -c 'import json,sys; print(json.load(sys.stdin)[0])')
newkey() { python3 -I -c 'import secrets; print("0x" + secrets.token_hex(32))'; }
addr() { (cd "$work" && "$FOUNDRY_BIN/cast" wallet address --private-key "$1"); }
declare -A K A
for who in coord opener yes1 yes2 no1 no2 treasury; do
  K[$who]=$(newkey)
  A[$who]=$(addr "${K[$who]}")
  c rpc anvil_setBalance "${A[$who]}" 0x56BC75E2D63100000 >/dev/null
done
bytecode() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["bytecode"]["object"])' "$repo/contracts/out/$1"; }
deploy() { # artifact, then the abi-encoded constructor args (hex, may be empty)
  local code; code=$(bytecode "$1")$2
  c send --unlocked --from "$deployer" --create "$code" --json | python3 -I -c 'import json,sys; r=json.load(sys.stdin); assert r["status"]=="0x1", r; print(r["contractAddress"])'
}
enc() { (cd "$work" && "$FOUNDRY_BIN/cast" abi-encode "$@") | sed 's/^0x//'; }
sendk() { local k=$1; shift; c send --private-key "${K[$k]}" "$@" --json >/dev/null; }
sendu() { c send --unlocked --from "$deployer" "$@" --json >/dev/null; }
# Expect a revert whose data starts with the custom error's selector.
refused() { # error signature, key name, then the send arguments
  local err=$1 k=$2; shift 2
  local sel; sel=$(cd "$work" && "$FOUNDRY_BIN/cast" sig "$err")
  local out
  if out=$(c send --private-key "${K[$k]}" "$@" --json 2>&1); then fail "accepted, want $err: $*"; fi
  grep -q "$sel\|${err%%(*}" <<<"$out" || fail "refused for another reason than $err: $out"
}
call() { c call "$@" | awk '{print $1}'; }
ledger=$(deploy OnyxObservedFacts.t.sol/CoordinatorLedger.json "$(enc 'f(address)' "${A[coord]}")")
registry=$(deploy OnyxRegistry.sol/OnyxRegistry.json "$(enc 'f(address,address)' "$deployer" "$ledger")")
token=$(deploy OnyxTestToken.sol/OnyxTestToken.json "$(enc 'f(address)' "$deployer")")
# Demo windows in blocks: trading >= 5, the recheck >= 5, a market <= 200 blocks, 10 grace blocks.
corrections=$(deploy OnyxCorrections.sol/OnyxCorrections.json "$(enc \
  'f(address,address,address,(uint128,uint128,uint128,uint128,uint32,uint32,uint32,uint32,uint32))' \
  "$token" "$registry" "${A[treasury]}" '(5000000000000000000,1000000000000000000,100000000000000000000,10000000000000000000000,20000,5,5,200,10)')")
echo "deployed: registry $registry, tONYX $token, corrections $corrections"
# 2. Register both metrics (dated feeds: kind 3) and pair them.
sendu "$registry" 'registerMetric(bytes32,uint8,bytes32,uint8)' "$metric" 3 "$template" 0
sendu "$registry" 'registerMetric(bytes32,uint8,bytes32,uint8)' "$rmetric" 3 "$rtemplate" 0
sendk coord "$registry" 'setReceiptSigner(address)' "${A[coord]}"
sendu "$corrections" 'pair(bytes32,bytes32,uint128)' "$metric" "$rmetric" "$tolerance"
kk() { (cd "$work" && "$FOUNDRY_BIN/cast" keccak "$1"); }
pool=$(kk "demo pool"); cfg=$(kk "demo cfg"); spec=$(kk "demo spec")
window=1793250000
# One coordinator round: announce a draw with its beacon one block ahead, fix it, record.
round() { # metric, round id, value
  local m=$1 r=$2 v=$3
  local fact n h commit result
  fact=$(call "$registry" 'factKey(bytes32,bytes32,uint64)(bytes32)' "$m" "$subject" "$window")
  n=$(call "$registry" 'drawCount(bytes32)(uint32)' "$fact")
  h=$(( $(c block-number) + 2 ))
  commit=$(call "$registry" 'drawCommit(bytes32,bytes32,bytes32)(bytes32)' "$pool" "$spec" "$cfg")
  sendk coord "$registry" 'announceDraw(bytes32,bytes32,uint64,uint32,bytes32,uint64)' "$m" "$subject" "$window" "$n" "$commit" "$h"
  c rpc anvil_mine 0x2 >/dev/null
  sendk coord "$registry" 'fixBeacon(bytes32,uint32)' "$fact" "$n"
  result=$(call "$corrections" 'resultHash(bytes32,bytes32,int128)(bytes32)' "$spec" "$r" "$v")
  sendk coord "$registry" 'recordObservation(bytes32,bytes32,uint64,bytes32,bytes32,bytes32,bytes32)' "$m" "$subject" "$window" "$spec" "$result" "$pool" "$cfg"
}
# 3. The published value.
oround=$(kk "original round"); rround=$(kk "recheck round")
round "$metric" "$oround" 18463
echo "value: us.treasury.tga_total_deposits 2026-10-28 = 18463 (million USD), recorded"
# 4. The market and the bets.
for who in opener yes1 yes2 no1 no2; do
  sendu "$token" 'mint(address,uint256)' "${A[$who]}" 100000000000000000000
  sendk "$who" "$token" 'approve(address,uint256)' "$corrections" 100000000000000000000
done
b=$(c block-number); close=$(( b + 12 )); resolve=$(( close + 40 ))
refused 'WrongValue()' opener "$corrections" 'open(bytes32,bytes32,uint64,bytes32,int128,uint64,uint64)' "$metric" "$subject" "$window" "$oround" 18464 "$close" "$resolve"
sendk opener "$corrections" 'open(bytes32,bytes32,uint64,bytes32,int128,uint64,uint64)' "$metric" "$subject" "$window" "$oround" 18463 "$close" "$resolve"
sendk yes1 "$corrections" 'bet(uint256,uint8,uint128)' 1 1 30000000000000000000
sendk yes2 "$corrections" 'bet(uint256,uint8,uint128)' 1 1 10000000000000000000
sendk no1 "$corrections" 'bet(uint256,uint8,uint128)' 1 2 15000000000000000000
sendk no2 "$corrections" 'bet(uint256,uint8,uint128)' 1 2 5000000000000000000
sendu "$token" 'mint(address,uint256)' "${A[coord]}" 1000000000000000000
sendk coord "$token" 'approve(address,uint256)' "$corrections" 1000000000000000000
refused 'NotAllowed()' coord "$corrections" 'bet(uint256,uint8,uint128)' 1 2 1000000000000000000
echo "market 1 open until block $close: Yes 40 tONYX (2 bettors), No 20 tONYX (2 bettors); insider refused"
# 5. After the close: the recheck round disagrees.
now=$(c block-number); [ "$now" -lt "$close" ] && c rpc anvil_mine "$(printf '0x%x' $(( close - now + 1 )))" >/dev/null
round "$rmetric" "$rround" 18470
# The original round never settles its own market.
refused 'WrongValue()' opener "$corrections" 'resolve(uint256,bytes32,int128)' 1 "$oround" 18463
sendk opener "$corrections" 'resolve(uint256,bytes32,int128)' 1 "$rround" 18470
refused 'WrongPhase()' opener "$corrections" 'resolve(uint256,bytes32,int128)' 1 "$rround" 18470
echo "recheck: 18470, outside the tolerance $tolerance: settled No"
b=$(c block-number)
refused 'RecheckStarted()' opener "$corrections" 'open(bytes32,bytes32,uint64,bytes32,int128,uint64,uint64)' "$metric" "$subject" "$window" "$oround" 18463 $(( b + 12 )) $(( b + 40 ))
# 6. Payouts, to the wei.
bal() { call "$token" 'balanceOf(address)(uint256)' "$1"; }
for who in yes1 yes2 no1 no2; do
  if [ "${who:0:2}" = no ]; then sendk "$who" "$corrections" 'withdraw(uint256)' 1
  else refused 'NothingOwed()' "$who" "$corrections" 'withdraw(uint256)' 1; fi
done
sendk opener "$corrections" 'payTreasury()'
read -r y1 y2 n1 n2 tr held < <(echo "$(bal "${A[yes1]}") $(bal "${A[yes2]}") $(bal "${A[no1]}") $(bal "${A[no2]}") $(bal "${A[treasury]}") $(bal "$corrections")")
echo "chain: yes1 $y1, yes2 $y2, no1 $n1, no2 $n2, treasury $tr, contract $held"
python3 -I - "$y1" "$y2" "$n1" "$n2" "$tr" "$held" <<'PY' || fail "payouts are not the parimutuel split"
import sys
y1, y2, n1, n2, tr, held = map(int, sys.argv[1:])
E = 10**18
start = 100 * E
yes, no = 40 * E, 20 * E
fee = yes * 20_000 // 1_000_000           # 2% of the losing side
dist = yes - fee
p1 = 15 * E + 15 * E * dist // no
p2 = 5 * E + 5 * E * dist // no
dust = (no + dist) - p1 - p2
assert y1 == start - 30 * E and y2 == start - 10 * E, (y1, y2)
assert n1 == start - 15 * E + p1 and n2 == start - 5 * E + p2, (n1, n2, p1, p2)
assert tr == fee + dust + 5 * E, (tr, fee, dust)   # trading fee, dust, the open fee
assert held == 0, held
assert (p1 + p2) + fee + dust == yes + no
print(f"no1 +{(p1 - 15 * E) / E} tONYX, no2 +{(p2 - 5 * E) / E} tONYX; treasury {tr / E} tONYX (fee, dust {dust} wei, open fee)")
PY
echo "CORRECTIONS ANVIL PASSED"
