#!/usr/bin/env bash
# ROADMAP S26 slice 3 (ADR-0065 design test 10, THREATS EARLY-1, EARLY-2, EARLY-19): the
# early-contributor record end to end on a LOCAL anvil (chain id 31337): one outside address does
# accepted work of several kinds and some that is not accepted, and `early-record` gives it exactly
# its rows.
# 1. OnyxSettlement and OnyxDisputes (the registry's ledger), OnyxRegistry, tONYX, OnyxSignedBook,
#    OnyxStorage (epochs of 10 blocks, 2 challenge slots) and OnyxCorrections, deployed from the
#    contracts' own artifacts; every role (coordinator, reviewer, minter, both treasuries) is a
#    fresh throwaway key. The chain starts on 2026-11-13 01:00 UTC whatever today is.
# 2. Day 1 (2026-11-13): the outside address X registers a publisher key (the reviewer registers
#    it) and anchors a tiny dataset (`signed_report`); X and a second node Y bond and register
#    storage nodes; a payer buys a deal on X's dataset (R=2); in two epochs X proves every slot
#    (`storage_proof`, paid by another: `other`) except one, where it sends a forged row (failed
#    and slashed: no row); Y never answers, and X settles Y's misses of the first epoch
#    (`storage_miss_report`). A correction market opens on a recorded value; X bets No and also
#    Yes (the losing side: no row), another bettor bets Yes.
# 3. Day 2 (2026-11-14): X settles Y's misses of the second epoch; a recheck round disagrees and
#    the market settles No (`correction_found`, accepted on day 2 for a bet of day 1).
# 4. Day 3 starts and becomes final. `early-record build` (contracts.json and exclusions.json
#    written for the anvil addresses; the record's own key, made by `onyx-publish keygen`, in
#    `record_publisher`) writes day 1, then day 2 with `--prev`, then nothing new. Every row but
#    `day_close` is X's and they are exactly the expected ones (kinds, details, counterparties,
#    amounts against the contracts' own counters); every role key and the record's key are in
#    the exclusions, X is not; a second build of a built day writes nothing.
# 5. `onyx-publish build` makes day 1 bulk version 1 under the record's publisher id
#    `early-record` (and its exclusions), day 2 version 2 with `--prev` (the successor
#    check); `early-record verify` (through `localhost`, the "second node": one chain here)
#    rebuilds day 2 from the deploy blocks and day 1 from the saved state, and gives the same
#    bytes, state hash and shard roots of both datasets; a changed row fails it as a difference.
# 6. The daily run's dry run (scripts/early-record-daily.sh) with the real tools on the same chain,
#    the second node being this anvil as `localhost` (another scheme://host:port to the URL check;
#    the test network uses two providers): the same CSV and version bytes from a fresh state, the
#    shard roots verified after each bulk build, a catalog a day, nothing signed; a second dry run
#    rebuilds nothing.
# Not exercised here (unit tests: crates/onyx-api/src/early_tests.rs): `l1_root`, `l1_verdict`
# (a mining round), `stake_correct` (OnyxStakes) and `revoke`. Keys: anvil's dev account 0
# deploys; every other key is made fresh for the run and thrown away; no key is printed. cast and
# forge run in a temp dir outside the repo (Foundry loads a `.env` from its working directory).
# About 30 s once built. Env: FOUNDRY_BIN (~/.foundry/bin), CARGO_TARGET_DIR.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
target=${CARGO_TARGET_DIR:-$REPO/target}
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
fail() { echo "EARLY RECORD ANVIL FAILED: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }
t0=$SECONDS
(cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/forge.log" 2>&1) \
  || { tail -20 "$work/forge.log"; fail "forge build"; }
# --workspace (not -p): the same feature set as check-all's `cargo test --workspace`, so these
# targets reuse its build instead of compiling a second variant of the shared crates.
cargo build --locked -q --workspace --example early-record --bin onyx-publish --bin onyx-storage
er=$target/debug/examples/early-record pub=$target/debug/onyx-publish st=$target/debug/onyx-storage

D1=1794528000 # 2026-11-13T00:00:00Z
(cd "$work" && exec "$FOUNDRY_BIN/anvil" --host 127.0.0.1 --port 0 --timestamp $((D1 + 3600)) \
  >"$work/anvil.log" 2>&1) &
anvil_pid=$!
port=""
for _ in $(seq 200); do
  kill -0 "$anvil_pid" 2>/dev/null || { cat "$work/anvil.log"; fail "anvil exited"; }
  port=$(grep -oE 'Listening on 127\.0\.0\.1:[0-9]+' "$work/anvil.log" | head -n1 | sed 's/.*://' || true)  # pipefail-ok: anvil's log is a few lines, and the status is dropped (|| true)
  [ -n "$port" ] && break
  sleep 0.1
done
[ -n "$port" ] || fail "anvil did not report its port"
RPC=http://127.0.0.1:$port
c() { local sub=$1; shift; (cd "$work" && "$FOUNDRY_BIN/cast" "$sub" --rpc-url "$RPC" "$@"); }
[ "$(c chain-id)" = 31337 ] || fail "not a local anvil"
deployer=$(c rpc eth_accounts | python3 -I -c 'import json,sys; print(json.load(sys.stdin)[0].lower())')
lower() { tr 'A-F' 'a-f'; }
declare -A K A
# A fresh key per role, in a 0600 file for the tools that read one (never printed).
for who in coord reviewer minter streasury ctreasury opener bettor payer x y; do
  K[$who]=$(python3 -I -c 'import secrets; print("0x" + secrets.token_hex(32))')
  A[$who]=$( (cd "$work" && "$FOUNDRY_BIN/cast" wallet address --private-key "${K[$who]}") | lower)
  (umask 077; printf '%s\n' "${K[$who]}" >"$work/$who.key")
  c rpc anvil_setBalance "${A[$who]}" 0x56BC75E2D63100000 >/dev/null
done
X=${A[x]}
# The record's own publisher key, made the way ops makes it (it prints the address only).
record_key=$("$pub" keygen --out "$work/record.key" | grep -oE '0x[0-9a-fA-F]{40}' | lower)
[ -n "$record_key" ] || fail "onyx-publish keygen printed no address"

echo "1. contracts"
enc() { (cd "$work" && "$FOUNDRY_BIN/cast" abi-encode "$@") | sed 's/^0x//'; }
# deploy NAME ABI ARTIFACT ARGS [LIBRARY=ADDRESS...]: prints the address (the artifact's library
# references linked); "NAME ABI ADDRESS BLOCK TX" goes to addrs.txt (ABI "-": not listed).
deploy() {
  local code
  code=$(python3 -I - "$REPO/contracts/out/$3" "${@:5}" <<'PY'
import json, sys
b = json.load(open(sys.argv[1]))["bytecode"]
code, libs = b["object"][2:], dict(x.split("=") for x in sys.argv[2:])
for refs in b.get("linkReferences", {}).values():
    for lib, at in refs.items():
        for r in at:
            code = code[:2 * r["start"]] + libs[lib][2:].lower() + code[2 * (r["start"] + r["length"]):]
print("0x" + code)
PY
)$4
  c send --unlocked --from "$deployer" --create "$code" --json | python3 -I -c '
import json, sys
r = json.load(sys.stdin)
assert r.get("status") == "0x1", sys.argv[1] + ": " + str(r)[:400]
a = r["contractAddress"].lower()
with open(sys.argv[3], "a") as f:
    f.write(f"{sys.argv[1]} {sys.argv[2]} {a} {int(r["blockNumber"], 16)} {r["transactionHash"].lower()}\n")
print(a)' "$1" "$2" "$work/addrs.txt"
}
# A failed send stops the run with cast's answer (it never holds the key).
sendu() { local o; o=$(c send --unlocked --from "$deployer" "$@" --json 2>&1) || fail "send $2 to $1: ${o:0:400}"; }
sendk() { local k=$1 o; shift; o=$(c send --private-key "${K[$k]}" "$@" --json 2>&1) || fail "$k: send $2 to $1: ${o:0:400}"; }
call() { c call "$@" | awk '{print $1}'; }
token=$(deploy tONYX test-token-v1 OnyxTestToken.sol/OnyxTestToken.json "$(enc 'f(address)' "${A[minter]}")")
disputes=$(deploy OnyxDisputes disputes-v1 OnyxDisputes.sol/OnyxDisputes.json "")
round_lib=$(deploy OnyxRound - OnyxRound.sol/OnyxRound.json "")
settlement=$(deploy OnyxSettlement settlement-v1 OnyxSettlement.sol/OnyxSettlement.json "$(enc \
  'f(address,address,address,string,uint64,uint128,uint128,uint128)' "$token" "${A[coord]}" "$disputes" \
  onyx.testnet 1800 100000000 100000000 10000000)" "OnyxRound=$round_lib")
sendu "$disputes" 'bind(address)' "$settlement"
registry=$(deploy OnyxRegistry registry-v1 OnyxRegistry.sol/OnyxRegistry.json "$(enc 'f(address,address)' "$deployer" "$settlement")")
book=$(deploy OnyxSignedBook signed-book-v1 OnyxSignedBook.sol/OnyxSignedBook.json "$(enc 'f(address)' "${A[reviewer]}")")
EB=10
storage=$(deploy OnyxStorage storage-v3 OnyxStorage.sol/OnyxStorage.json "$(enc \
  'f(address,address,address,uint64,uint128,uint128,uint128,uint32,uint32,uint32)' \
  "$token" "$book" "${A[streasury]}" "$EB" 1000000000000000 10000000000000000 1000000000000000000 100000 2 10)")
corrections=$(deploy OnyxCorrections corrections-v1 OnyxCorrections.sol/OnyxCorrections.json "$(enc \
  'f(address,address,address,(uint128,uint128,uint128,uint128,uint32,uint32,uint32,uint32,uint32))' \
  "$token" "$registry" "${A[ctreasury]}" '(5000000000000000000,1000000000000000000,100000000000000000000,10000000000000000000000,20000,5,5,200,10)')")
for a in "$token" "$disputes" "$settlement" "$registry" "$book" "$storage" "$corrections"; do
  [[ "$a" =~ ^0x[0-9a-f]{40}$ ]] || fail "a deploy gave no address"
done
pass "settlement $settlement, book $book, storage $storage, corrections $corrections"

echo "2. day 1 (2026-11-13): X publishes, bets, proves and settles misses"
# signed_report: the reviewer registers X's key; X anchors a tiny dataset.
mkdir -p "$work/xd"
printf 't,v\n' >"$work/xd/data.csv"
for i in 1 2 3 4 5; do printf '%d,%d\n' "$((1700000000 + i))" "$((i * 7))" >>"$work/xd/data.csv"; done
printf '{"columns": [{"name": "t", "kind": "int"}, {"name": "v", "kind": "int"}], "key": "t"}\n' >"$work/xd/schema.json"
"$pub" keys-doc --key "$work/x.key" --publisher synthetic.outside --out "$work/xd/keys.json" \
  2>"$work/doc.log" >/dev/null
provenance=$(grep -oE '0x[0-9a-f]{64}' "$work/doc.log" | head -n1)  # pipefail-ok: keys-doc logs a few lines
sendk reviewer "$book" 'registerKey(string,address,string,bytes32,string)' synthetic.outside "$X" \
  outside.example "$provenance" "early-record anvil: keys document reviewed"
"$pub" build --schema "$work/xd/schema.json" --input "$work/xd/data.csv" --dataset synthetic.early-outside \
  --publisher synthetic.outside --license CC0-1.0 --out "$work/xv1" --created "$(c block latest --field timestamp)" \
  >/dev/null
digest=$("$pub" sign --key "$work/x.key" --dir "$work/xv1" | grep -oE 'digest=0x[0-9a-f]{64}' | cut -d= -f2)
# The answers below are matched from a variable, never through a pipe into grep -q (THREATS OPS-PIPE-1).
got=$("$pub" anchor --dir "$work/xv1" --rpc "$RPC" --chain-id 31337 --book "$book" --gas-key "$work/x.key") \
  && grep -q '^ANCHORED tx=' <<<"$got" || fail "X's anchor"
pass "X's report $digest anchored"
for who in x y payer; do sendk minter "$token" 'mint(address,uint256)' "${A[$who]}" 100000000000000000000; done
# The correction market (registry rounds as scripts/corrections-anvil.sh records them).
kk() { (cd "$work" && "$FOUNDRY_BIN/cast" keccak "$1"); }
metric=$(kk "early anvil metric"); rmetric=$(kk "early anvil metric.recheck")
subject=$(kk "early anvil subject"); pool=$(kk "demo pool"); cfg=$(kk "demo cfg"); spec=$(kk "demo spec")
window=1793250000
sendu "$registry" 'registerMetric(bytes32,uint8,bytes32,uint8)' "$metric" 3 "$(kk "early anvil template")" 0
sendu "$registry" 'registerMetric(bytes32,uint8,bytes32,uint8)' "$rmetric" 3 "$(kk "early anvil template.recheck")" 0
sendk coord "$registry" 'setReceiptSigner(address)' "${A[coord]}"
sendu "$corrections" 'pair(bytes32,bytes32,uint128)' "$metric" "$rmetric" 0
round() { # metric, round id, value
  local m=$1 r=$2 v=$3 fact n h
  fact=$(call "$registry" 'factKey(bytes32,bytes32,uint64)(bytes32)' "$m" "$subject" "$window")
  n=$(call "$registry" 'drawCount(bytes32)(uint32)' "$fact")
  h=$(( $(c block-number) + 2 ))
  sendk coord "$registry" 'announceDraw(bytes32,bytes32,uint64,uint32,bytes32,uint64)' "$m" "$subject" "$window" "$n" \
    "$(call "$registry" 'drawCommit(bytes32,bytes32,bytes32)(bytes32)' "$pool" "$spec" "$cfg")" "$h"
  c rpc anvil_mine 0x2 >/dev/null
  sendk coord "$registry" 'fixBeacon(bytes32,uint32)' "$fact" "$n"
  sendk coord "$registry" 'recordObservation(bytes32,bytes32,uint64,bytes32,bytes32,bytes32,bytes32)' "$m" "$subject" \
    "$window" "$spec" "$(call "$corrections" 'resultHash(bytes32,bytes32,int128)(bytes32)' "$spec" "$r" "$v")" "$pool" "$cfg"
}
oround=$(kk "original round"); rround=$(kk "recheck round")
round "$metric" "$oround" 100
sendk minter "$token" 'mint(address,uint256)' "${A[opener]}" 5000000000000000000
sendk minter "$token" 'mint(address,uint256)' "${A[bettor]}" 10000000000000000000
for who in opener bettor x; do sendk "$who" "$token" 'approve(address,uint256)' "$corrections" 100000000000000000000; done
b=$(c block-number)
sendk opener "$corrections" 'open(bytes32,bytes32,uint64,bytes32,int128,uint64,uint64)' "$metric" "$subject" "$window" \
  "$oround" 100 $((b + 10)) $((b + 150))
no_bet=15000000000000000000
sendk x "$corrections" 'bet(uint256,uint8,uint128)' 1 2 "$no_bet"
sendk x "$corrections" 'bet(uint256,uint8,uint128)' 1 1 5000000000000000000
sendk bettor "$corrections" 'bet(uint256,uint8,uint128)' 1 1 10000000000000000000
pass "market 1 (close block $((b + 10))): X No 15 and Yes 5 tONYX, another bettor Yes 10"
# Storage: X and Y register nodes, the payer buys R=2 on X's dataset.
S=(--rpc "$RPC" --contract "$storage" --chain-id 31337)
declare -A NODE
for who in x y; do
  out=$("$st" register "${S[@]}" --key "$work/$who.key" --capacity-gib 1 --url "http://127.0.0.1:9/$who") \
    || fail "register $who"
  NODE[$who]=$(grep -oE 'NODE_ID=0x[0-9a-f]{64}' <<<"$out" | cut -d= -f2)
  [ -n "${NODE[$who]}" ] || fail "no node id for $who: $out"
done
deal_out=$("$st" deal "${S[@]}" --key "$work/payer.key" --dir "$work/xv1" --r 2 --epochs 6 \
  --source-url http://127.0.0.1:9/) || fail "deal"
start=$(grep -oE 'epochs=[0-9]+' <<<"$deal_out" | cut -d= -f2)
genesis=$(call "$storage" 'genesis()(uint256)')
pay=$(call "$storage" 'slotPay(uint64)(uint256)' 0)
pass "nodes registered; deal 0 from epoch $start, $pay per proof"
mine_to() { # epoch: mine to its start block + 1 (its seed can be recorded)
  local want=$((genesis + $1 * EB + 1)) cur
  cur=$(c block-number)
  [ "$cur" -ge "$want" ] || c rpc anvil_mine "$((want - cur))" >/dev/null
  [ "$(call "$storage" 'currentEpoch()(uint64)')" = "$1" ] || fail "epoch $1 is already over (block $cur)"
}
# X's open asks of epoch E as "deal slot" lines.
x_asks() {
  "$st" epoch "${S[@]}" --epoch "$1" | python3 -I -c '
import json, sys
for a in json.load(sys.stdin)["asks"]:
    if a["node"] == sys.argv[1] and a["status"] == "open":
        print(a["deal"], a["slot"])' "${NODE[x]}"
}
answer() { # epoch deal slot [forged]
  "$st" prove "${S[@]}" --deal "$2" --slot "$3" --node "${NODE[x]}" --dir "$work/xv1" >"$work/prove.json"
  local data
  data=$(python3 -I - "$work/prove.json" "${4:-}" <<'PY'
import json, sys
p = json.load(open(sys.argv[1]))
b = bytearray.fromhex(p["calldata"][2:])
if sys.argv[2]:
    b[p["row_at"] + p["row_len"] - 1] ^= 1  # one changed byte in the row
print("0x" + b.hex())
PY
)
  sendk x "$storage" "$data"
  local want=1
  [ -z "${4:-}" ] || want=2
  [ "$(call "$storage" 'slotStatus(uint64,uint64,uint32,bytes32)(uint8)' "$2" "$1" "$3" "${NODE[x]}")" = "$want" ] \
    || fail "epoch $1 slot $3: X's ${4:-honest} answer did not give status $want"
}
proofs=0
for e in "$start" $((start + 1)); do
  mine_to "$e"
  "$st" seed "${S[@]}" --key "$work/x.key" >/dev/null
  mapfile -t asks < <(x_asks "$e")
  [ "${#asks[@]}" = 2 ] || fail "epoch $e: X has ${#asks[@]} asks, want 2"
  for i in 0 1; do
    read -r d slot <<<"${asks[i]}"
    if [ "$e" != "$start" ] && [ "$i" = 0 ]; then answer "$e" "$d" "$slot" forged
    else answer "$e" "$d" "$slot"; proofs=$((proofs + 1)); fi
  done
done
got=$("$st" settle "${S[@]}" --key "$work/x.key" --epoch "$start") && grep -q '^SETTLED 2$' <<<"$got" || fail "X settles epoch $start"
pass "epochs $start and $((start + 1)): $proofs proofs passed, 1 forged row failed; Y's 2 misses of epoch $start settled by X"

echo "3. day 2 (2026-11-14): more misses settled, the market settles No"
c rpc evm_setNextBlockTimestamp $((D1 + 86400 + 3600)) >/dev/null
mine_to $((start + 2))
got=$("$st" settle "${S[@]}" --key "$work/x.key" --epoch $((start + 1))) && grep -q '^SETTLED 2$' <<<"$got" \
  || fail "X settles epoch $((start + 1))"
round "$rmetric" "$rround" 101
sendk opener "$corrections" 'resolve(uint256,bytes32,int128)' 1 "$rround" 101
market_sig='market(uint256)((bytes32,bytes32,bytes32,bytes32,address,uint64,uint64,uint64,uint8,uint8,uint8,uint32,uint32,int128,int128,uint128,uint128,uint128,uint128,uint128,uint128,uint128[2],uint32[2]))'
c call "$corrections" "$market_sig" 1 --json | python3 -I -c 'import json,sys; m=json.load(sys.stdin)[0]; assert m[9] == 2, m' \
  || fail "market 1 did not settle No"
pass "Y's 2 misses of epoch $((start + 1)) settled by X; recheck 101 against 100: No"
# Day 3 starts; anvil's finalized block is 64 behind its head.
c rpc evm_setNextBlockTimestamp $((D1 + 2 * 86400 + 3600)) >/dev/null
c rpc anvil_mine 70 >/dev/null

echo "4. the record: build day 1, day 2, then nothing new"
day1=2026-11-13 day2=2026-11-14
python3 -I - "$work/addrs.txt" "$work/contracts.json" "$work/exclusions.json" "$record_key" "$day1" <<'PY'
import json, sys
src, contracts, exclusions, record, day = sys.argv[1:]
counted = {"settlement-v1", "disputes-v1", "signed-book-v1", "storage-v3", "corrections-v1"}
out = []
for line in open(src):
    name, abi, addr, block, tx = line.split()
    if abi == "-":
        continue
    e = {"name": name, "abi": abi, "address": addr, "doc": "scripts/early-record-anvil.sh"}
    if abi in counted:
        e.update(scan_from=int(block), scan_from_is="deploy", block_doc="scripts/early-record-anvil.sh",
                 block_quote=f"{name} {addr} deployed at block {block} of the local anvil", deploy_tx=tx)
    out.append(e)
json.dump({"format": 1, "chain_id": 31337, "network": "local anvil", "note": "scripts/early-record-anvil.sh",
           "contracts": out}, open(contracts, "w"), indent=1)
json.dump({"format": 1, "note": "scripts/early-record-anvil.sh", "operator_keys": [],
           "record_publisher": {"address": record, "from_day": day}}, open(exclusions, "w"), indent=1)
PY
E=(--contracts "$work/contracts.json" --exclusions "$work/exclusions.json")
v=$work/versions
record() { ONYX_EARLY_RPC=$RPC "$er" "$@"; }
out=$(record build "${E[@]}" --state "$work/state" --out "$v/$day1") || fail "build day 1"
grep -q "\"built\":\"$day1\"" <<<"$out" || fail "build day 1: $out"
out=$(record build "${E[@]}" --state "$work/state" --out "$v/$day2" --prev "$v/$day1") || fail "build day 2"
grep -q "\"built\":\"$day2\"" <<<"$out" || fail "build day 2: $out"
out=$(record build "${E[@]}" --state "$work/state" --out "$v/2026-11-15" --prev "$v/$day2") || fail "build day 3"
grep -q '"nothing_new"' <<<"$out" && [ ! -e "$v/2026-11-15" ] || fail "day 3 is not final yet, but: $out"
out=$(record build "${E[@]}" --state "$work/state" --out "$v/$day2") || fail "build day 2 again"
grep -q "\"already_built\":\"$day2\"" <<<"$out" || fail "a built day built again: $out"
pass "day 1 and day 2 built; day 3 not final yet; a built day is never built again"
earned=$(call "$storage" 'earned(address)(uint256)' "$X")
rewarded=$(call "$storage" 'rewarded()(uint256)')
python3 -I - "$v/$day1" "$v/$day2" <<PY || fail "the rows are not the expected ones"
import csv, sys
from collections import Counter
d1, d2 = sys.argv[1:]
rows = {d: list(csv.DictReader(open(f"{p}/data.csv"))) for d, p in (("$day1", d1), ("$day2", d2))}
X, payer, opener = "$X", "${A[payer]}", "${A[opener]}"
book, storage, corrections = "$book", "$storage", "$corrections"
work = []
for day, rs in rows.items():
    assert rs[-1]["kind"] == "day_close" and all(r["kind"] != "day_close" for r in rs[:-1]), day
    assert all(r["day"] == day for r in rs), day
    work += rs[:-1]
assert all(r["who"] == X for r in work), Counter(r["who"] for r in work)
got = Counter((r["day"], r["kind"], r["detail"], r["contract"], r["counterparty"]) for r in work)
want = Counter({
    ("$day1", "signed_report", "accepted", book, ""): 1,
    ("$day1", "storage_proof", "other", storage, payer): $proofs,
    ("$day1", "storage_miss_report", "reward", storage, payer): 2,
    ("$day2", "storage_miss_report", "reward", storage, payer): 2,
    ("$day2", "correction_found", "no_won", corrections, opener): 1,
})
assert got == want, (got, want)
by = lambda k: [r for r in work if r["kind"] == k]
assert by("signed_report")[0]["subject"] == "$digest"
assert all(int(r["amount"]) == $pay for r in by("storage_proof"))
miss = sum(int(r["amount"]) for r in by("storage_miss_report"))
assert miss == $rewarded and miss + $pay * $proofs == $earned, (miss, $rewarded, $earned)
f = by("correction_found")[0]
assert (f["subject"], int(f["amount"])) == ("1", $no_bet) and int(f["work_block"]) < int(f["accept_block"])
assert len({r["key"] for r in work}) == len(work)
print(f"  ok: {len(work)} rows, all X's: " + ", ".join(f"{k[1]} {k[2]} x{n}" for k, n in sorted(got.items())))
PY
python3 -I - "$v/$day1/exclusions/data.csv" "$v/$day2" <<PY || fail "the exclusions are not the expected ones"
import csv, os, sys
x = {r["address"]: r for r in csv.DictReader(open(sys.argv[1]))}
want = {  # an address wanted twice keeps its first source in sort order
    "${A[coord]}": ("role", "OnyxRegistry.receiptSigner()"),  # also OnyxSettlement.coordinator()
    "${A[reviewer]}": ("role", "OnyxSignedBook.reviewer()"),
    "${A[minter]}": ("role", "tONYX.minter()"),
    "${A[streasury]}": ("role", "OnyxStorage.treasury()"),
    "${A[ctreasury]}": ("role", "OnyxCorrections.treasury()"),
    "$deployer": ("role", "OnyxCorrections.owner()"),  # also anvil default account 0, a fixture
    "$record_key": ("operator_key", "the record's own publisher key"),
}
for a, (reason, source) in want.items():
    assert a in x and (x[a]["action"], x[a]["reason"], x[a]["source"]) == ("add", reason, source), (a, x.get(a))
for a in ("$X", "${A[y]}", "${A[payer]}", "${A[opener]}", "${A[bettor]}"):
    assert a not in x, a
assert not os.path.exists(os.path.join(sys.argv[2], "exclusions")), "day 2 changed no exclusion"
print(f"  ok: {len(x)} exclusions on day 1: every role key, the dev accounts and the record's own key; X is not one")
PY

echo "5. bulk versions under early-record, the successor check, verify"
# Each version is created at the end of its day (00:00 UTC of the next), as the daily run does.
bulk() { # folder, dataset, created, [--prev folder]
  local dir=$1 ds=$2 at=$3; shift 3
  "$pub" build --schema "$dir/schema.json" --input "$dir/data.csv" --dataset "$ds" --publisher early-record \
    --license CC0-1.0 --out "$dir" --created "$at" "$@"
}
got=$(bulk "$v/$day1" testnet.early-contributors $((D1 + 86400))) && grep -q '^BUILT version=1 ' <<<"$got" || fail "bulk version 1"
got=$(bulk "$v/$day1/exclusions" testnet.early-contributors.exclusions $((D1 + 86400))) && grep -q '^BUILT version=1 ' <<<"$got" \
  || fail "exclusions version 1"
got=$(bulk "$v/$day2" testnet.early-contributors $((D1 + 2 * 86400)) --prev "$v/$day1") && grep -q '^BUILT version=2 ' <<<"$got" \
  || fail "bulk version 2 (the successor check)"
# The second node: this anvil by another name (one chain here; on the test network, another provider).
RPC2=http://localhost:$port
check() { ONYX_EARLY_RPC=$RPC2 "$er" verify "${E[@]}" "$@"; }
out=$(check --dir "$v/$day2") || fail "verify day 2"
grep -q "\"verified\":\"$day2\"" <<<"$out" && grep -q '"shard_roots":1' <<<"$out" || fail "verify day 2: $out"
out=$(check --dir "$v/$day1" --state-before "$work/state/before/$day1.json") || fail "verify day 1"
grep -q "\"verified\":\"$day1\"" <<<"$out" && grep -q '"shard_roots":1' <<<"$out" \
  && grep -q '"exclusion_shard_roots":1' <<<"$out" || fail "verify day 1: $out"
grep -q '"state_keccak": "0x[0-9a-f]\{64\}"' "$v/$day1/day.json" || fail "day.json holds no state hash"
cp -a "$v/$day2" "$work/changed"
sed -i '2s/,chain,/,chainx,/' "$work/changed/data.csv"
if check --dir "$work/changed" >/dev/null 2>"$work/changed.err"; then fail "verify passed a changed row"; fi
grep -q "data.csv differs from the chain's" "$work/changed.err" || fail "verify refused a changed row for another reason: $(cat "$work/changed.err")"
pass "versions 1 and 2 built (2 follows 1); verify gives the same bytes, state hash and shard roots (both datasets); a changed row: data.csv differs"

echo "6. the daily run's dry run on the same chain, with the real tools (the second node: this anvil as localhost)"
idx=$(python3 -I scripts/early-record-daily-tests.py --stand-in-indexer "$work/idx")
mkdir "$work/bin"
ln -s "$er" "$work/bin/early-record"
ln -s "$pub" "$work/bin/onyx-publish"
daily() {
  env ONYX_EARLY_RPC="$RPC" ONYX_EARLY_RPC2="$RPC2" ONYX_EARLY_STATE="$work/dstate" ONYX_EARLY_OUT="$work/dout" \
    ONYX_EARLY_PUBLISHER=early-record ONYX_EARLY_INDEXER="$idx" ONYX_EARLY_BIN="$work/bin" \
    ONYX_EARLY_CONTRACTS="$work/contracts.json" ONYX_EARLY_EXCLUSIONS="$work/exclusions.json" \
    scripts/early-record-daily.sh --dry-run >"$work/daily.log" 2>&1 || { cat "$work/daily.log"; fail "the daily dry run"; }
}
mkdir "$work/dout"
daily
dv=$work/dout/versions dc=$work/dout/catalogs
# verify ran after each bulk build: it compared the shard roots of both versions.
[ "$(grep -c '"shard_roots":1' "$work/daily.log")" = 2 ] && [ "$(grep -c '"exclusion_shard_roots":1' "$work/daily.log")" = 1 ] \
  || fail "the daily run's verify did not compare the shard roots: $(cat "$work/daily.log")"
for d in "$day1" "$day2"; do
  # A fresh state gives the same CSV, and the same version bytes (each created at its day's end).
  cmp -s "$v/$d/data.csv" "$dv/$d/data.csv" && cmp -s "$v/$d/manifest.bin" "$dv/$d/manifest.bin" \
    || fail "the daily run's $d differs from the direct build"
  [ -s "$dc/$d.bin" ] && [ ! -e "$dc/$d.bin.report" ] || fail "the dry run's catalog of $d"
done
cmp -s "$v/$day1/exclusions/manifest.bin" "$dv/$day1/exclusions/manifest.bin" || fail "the daily run's exclusions"
grep -q "dry run done: 2 day(s) ready, nothing sent" "$work/daily.log" || fail "the dry run's summary: $(cat "$work/daily.log")"
sha=$(cat "$dc"/*.bin | sha256sum)
daily
[ "$(cat "$dc"/*.bin | sha256sum)" = "$sha" ] && ! grep -q ': built: {' "$work/daily.log" || fail "a second dry run rebuilt something"
pass "2 days built, scanned, verified (shard roots too) and catalogued (catalogs 0 and 1), byte for byte the direct build; nothing signed; a rerun rebuilds nothing"
echo "EARLY RECORD ANVIL PASSED ($((SECONDS - t0)) s)"
