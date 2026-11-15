#!/usr/bin/env bash
# CL7 (ADR-0057 item 7): the rehearsal of scripts/recheck-sepolia.sh on a local anvil FORK of
# Base Sepolia, end to end, before the live recheck days. NO Base Sepolia transaction: every send
# goes to the fork (127.0.0.1), whose chain id is not 84532, so nothing signed on it is valid on
# Base Sepolia.
#   1. anvil forks Base Sepolia at its head (`--chain-id RECHECK_FORK_CHAIN_ID`, a block a second)
#      and mines past the latest close of the open markets with no time between the blocks
#      (`anvil_mine N 0`): the contract counts blocks, and the fork's clock stays the wall clock,
#      which the live coordinator checks against its own, so today must be a recheck day.
#   2. Every signer the pre-flight names gets 0.01 test ETH on the fork (`anvil_setBalance`).
#   3. `recheck-sepolia.sh check`, a dry run (nothing sent), `run --yes` (the recheck round with
#      the real source fetched by each observer, resolve, withdraw, payTreasury) and `run --yes`
#      again, which must send nothing. With RECHECK_FORK_FAULT a fault comes first (THREATS
#      CL7-25), then `run --yes` until it is done (at most three runs):
#      - `interrupt`: SIGINT to the runner (by PID) once its coordinator has started, before the
#        draw; the next run waits until that window's commit close has passed. It must pick a
#        new window, and the recheck fact must end with one draw.
#      - `observers`: every observer stopped (by PID) once the draw is announced and opened, so
#        the round records no value. No later run may announce a draw for that window again,
#        and the market must still resolve (two draws).
#      - `lost`: the proxy drops the coordinator's `announceDraw` (the round's first raw
#        transaction), and the runner gets a SIGINT while the coordinator waits for it, so the
#        book holds a draw that never landed; the next run, after that window's commit close,
#        resumes it, finds it never landed and must refuse a new draw for the dead window; then a
#        new window (one draw in all).
#   4. The checks: every served market resolved, the winner by the contract's rule, each held
#      winner paid its share to the wei, the treasury paid, the contract's balance its
#      liabilities; then the gas of every transaction per step.
# The export dir is copied (deployment.json, the original round's store, the pins): the recheck's
# store and pin go to the copy, never to EXPORT_DIR.
# anvil suggests a 1 gwei tip, which with any base fee is above KeySigner's 1 gwei cap, so a
# loopback proxy in front of the fork answers eth_maxPriorityFeePerGas with Base Sepolia's own
# suggestion and passes every other call through unchanged.
#
#   scripts/recheck-fork.sh MANIFEST DATE EXPORT_DIR      (about 15 minutes: the round runs in real
#                                                          time; 25 to 35 with a fault)
#
# Env: RECHECK_FORK_UPSTREAM (https://sepolia.base.org: the coordinator finds the registry's
# deploy block by reading code at past blocks, so the upstream must serve archive state; a pruned
# endpoint fails there), RECHECK_FORK_CHAIN_ID (845320), RECHECK_FORK_FAULT (none, interrupt,
# observers or lost), FOUNDRY_BIN, and recheck-sepolia.sh's (ONYX_TESTNET_DIR, ONYX_ENV_FILE,
# ONYX_CORRECTIONS, RECHECK_*_KEYS, ...; ONYX_L2_RPC and RECHECK_BLOCK_MS are set here).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
[ $# = 3 ] || { sed -n '/^#   scripts\/recheck-fork.sh/p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }
manifest=$(realpath -e "$1")
date=$2
export_src=$(cd "$3" && pwd)
name=$(basename "$(dirname "$manifest")")
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
UPSTREAM=${RECHECK_FORK_UPSTREAM:-https://sepolia.base.org}
FORK_CHAIN_ID=${RECHECK_FORK_CHAIN_ID:-845320}
case $FORK_CHAIN_ID in 84532 | 31337 | *[!0-9]*) echo "RECHECK_FORK_CHAIN_ID: not 84532, not 31337, a number" >&2; exit 2 ;; esac
export RECHECK_FORK_CHAIN_ID=$FORK_CHAIN_ID
FAULT=${RECHECK_FORK_FAULT:-none}
case $FAULT in none | interrupt | observers | lost) ;; *) echo "RECHECK_FORK_FAULT: none, interrupt, observers or lost" >&2; exit 2 ;; esac
TESTNET=${ONYX_TESTNET_DIR:-$REPO/.onyx-testnet}
work=$(mktemp -d)
chmod 700 "$work"
# The runner's per-value lock lives here, so rehearsals on two forks at once do not collide.
export RECHECK_LOCK_DIR=$work/locks
anvil_pid="" proxy_pid=""
cleanup() {
  for p in $proxy_pid $anvil_pid; do
    kill "$p" 2>/dev/null || true
    wait "$p" 2>/dev/null || true
  done
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP
fail() { echo "RECHECK FORK FAILED: $*"; exit 1; }
cu() { (cd "$work" && ETH_RPC_URL=$UPSTREAM "$FOUNDRY_BIN/cast" "$@"); }

# The export copy.
mkdir -p "$work/export/observations"
cp "$export_src/deployment.json" "$work/export/"
cp "$export_src/observations/$name.json" "$work/export/observations/"
[ ! -e "$export_src/observations/pins" ] || cp "$export_src/observations/pins" "$work/export/observations/"
[ ! -e "$export_src/observations/$name.recheck.json" ] || fail "$export_src already holds a recheck store: nothing to rehearse"

[ "$(cu chain-id)" = 84532 ] || fail "the upstream is not Base Sepolia"
tip=$(cu rpc eth_maxPriorityFeePerGas | tr -d '"')
# Base Sepolia's block time for the runner's time estimates: the fork's own blocks past the fork
# share one timestamp, so a block time measured there means nothing.
up_head=$(cu block-number)
export RECHECK_BLOCK_MS
RECHECK_BLOCK_MS=$(python3 -I -c 'import sys; a, b = map(int, sys.argv[1:]); print(round((a - b) * 1000 / 432000))' \
  "$(cu block "$up_head" --field timestamp)" "$(cu block $((up_head - 432000)) --field timestamp)")
(cd "$work" && exec "$FOUNDRY_BIN/anvil" --fork-url "$UPSTREAM" --chain-id "$FORK_CHAIN_ID" \
  --host 127.0.0.1 --port 0 --block-time 1 >"$work/anvil.log" 2>&1) &
anvil_pid=$!
port=""
for _ in $(seq 600); do
  kill -0 "$anvil_pid" 2>/dev/null || { tail -5 "$work/anvil.log"; fail "anvil exited"; }
  port=$(grep -oE 'Listening on 127\.0\.0\.1:[0-9]+' "$work/anvil.log" | awk 'NR == 1' | sed 's/.*://' || true)
  [ -n "$port" ] && break
  sleep 0.1
done
[ -n "$port" ] || fail "anvil did not report its port"
cat >"$work/proxy.py" <<'PY'
import http.server, json, os, sys, urllib.request
up, tip, out, drop, dropped = sys.argv[1:6]
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", 0)))
        req = json.loads(body)
        reqs = req if isinstance(req, list) else [req]
        if all(r.get("method") == "eth_maxPriorityFeePerGas" for r in reqs):
            ans = [{"jsonrpc": "2.0", "id": r.get("id"), "result": tip} for r in reqs]
            data = json.dumps(ans if isinstance(req, list) else ans[0]).encode()
        elif (not isinstance(req, list) and req.get("method") == "eth_sendRawTransaction"
              and os.path.exists(drop) and not os.path.exists(dropped)):
            # The `lost` fault: the next raw transaction (the coordinator's announceDraw, the
            # round's first send) never reaches the fork.
            open(dropped, "w").write("1")
            data = json.dumps({"jsonrpc": "2.0", "id": req.get("id"),
                               "error": {"code": -32000, "message": "dropped by the rehearsal proxy"}}).encode()
        else:
            r = urllib.request.Request(up, data=body, headers={"content-type": "application/json"})
            with urllib.request.urlopen(r, timeout=600) as f:
                data = f.read()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
s = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
open(out, "w").write(str(s.server_address[1]))
s.serve_forever()
PY
(cd "$work" && exec python3 -I "$work/proxy.py" "http://127.0.0.1:$port" "$tip" "$work/proxy.port" \
  "$work/drop-next-send" "$work/dropped") &
proxy_pid=$!
for _ in $(seq 100); do [ -s "$work/proxy.port" ] && break; sleep 0.1; done
[ -s "$work/proxy.port" ] || fail "the proxy did not start"
RPC=http://127.0.0.1:$(cat "$work/proxy.port")
c() { (cd "$work" && ETH_RPC_URL=$RPC "$FOUNDRY_BIN/cast" "$@"); }
[ "$(c chain-id)" = "$FORK_CHAIN_ID" ] || fail "the fork's chain id is not $FORK_CHAIN_ID"
[ "$(c rpc eth_maxPriorityFeePerGas | tr -d '"')" = "$tip" ] || fail "the proxy's tip"
export ONYX_L2_RPC=$RPC

C=${ONYX_CORRECTIONS:-$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["contract"])' "$TESTNET/corrections-sepolia.json")}
export ONYX_CORRECTIONS=$C
MARKET_SIG='market(uint256)((bytes32,bytes32,bytes32,bytes32,address,uint64,uint64,uint64,uint8,uint8,uint8,uint32,uint32,int128,int128,uint128,uint128,uint128,uint128,uint128,uint128,uint128[2],uint32[2]))'
mjson() { c call "$C" "$MARKET_SIG" "$1" --json | python3 -I -c 'import json,sys; m=json.load(sys.stdin)[0]; m = m if isinstance(m, list) else json.loads(m); json.dump(m, sys.stdout)'; }
n=$(c call "$C" 'marketCount()(uint256)' | awk '{print $1}')
open_ids=() close=0
for ((i = 1; i <= n; i++)); do
  read -r s cb <<<"$(mjson "$i" | python3 -I -c 'import json,sys; m=json.load(sys.stdin); print(m[8], m[6])')"
  if [ "$s" = 1 ]; then open_ids+=("$i"); [ "$cb" -le "$close" ] || close=$cb; fi
done
[ ${#open_ids[@]} -gt 0 ] || fail "no open market on $C to rehearse"
fork_head=$(c block-number)
echo "fork of Base Sepolia at block $fork_head (chain id $FORK_CHAIN_ID, $RPC; tip $((tip)) wei as Base Sepolia's); open market(s) ${open_ids[*]}, latest close $close"

# 1. Past the close, the clock unchanged.
t0=$(date +%s)
# In steps of 50,000 blocks (about 7 s each): one call for all would outlast cast's HTTP timeout.
left=$((close - fork_head + 1))
while [ "$left" -gt 0 ]; do
  step=$((left < 50000 ? left : 50000))
  c rpc anvil_mine "$(printf '0x%x' "$step")" 0x0 >/dev/null
  left=$((left - step))
done
start_block=$(c block-number)
echo "mined $((close - fork_head + 1)) blocks in $(( $(date +%s) - t0 )) s: head $start_block, its time $(c block latest --field timestamp) (wall clock $(date +%s))"
[ "$start_block" -gt "$close" ] || fail "the head is not past the close"

rs() {
  local sub=$1
  shift
  scripts/recheck-sepolia.sh "$sub" "$manifest" "$date" "$work/export" --state "$work/state" "$@"
}
# 2. Fund the signers the pre-flight names (fork only).
rs check >"$work/check0.log" 2>&1 || true
signers=$(grep -oE '^(PASS|FAIL) balance 0x[0-9a-f]{40}' "$work/check0.log" | awk '{print $3}' | sort -u)
[ -n "$signers" ] || { cat "$work/check0.log"; fail "the pre-flight named no signer"; }
for a in $signers; do c rpc anvil_setBalance "$a" 0x2386f26fc10000 >/dev/null; done # 0.01 ETH
echo "funded $(wc -w <<<"$signers") signers with 0.01 test ETH on the fork"

# Before: the tONYX of every signer, the treasury and the contract; the markets and their bettors.
token=$(c call "$C" 'token()(address)' | tr '[:upper:]' '[:lower:]')
treasury=$(c call "$C" 'treasury()(address)' | tr '[:upper:]' '[:lower:]')
fee_ppm=$(c call "$C" 'feePpm()(uint32)' | awk '{print $1}')
bal() { c call "$token" 'balanceOf(address)(uint256)' "$1" | awk '{print $1}'; }
balances() { for a in $signers "$treasury" "$C"; do echo "$a $(bal "$a")"; done >"$1"; }
balances "$work/tonyx.before"
for i in "${open_ids[@]}"; do
  mjson "$i" >"$work/market-$i.before.json"
  c call "$C" 'bettorsOf(uint256,uint256,uint256)(address[],uint256)' "$i" 0 200 | awk 'NR == 1' | tr -d '[],' \
    | tr '[:upper:]' '[:lower:]' >"$work/bettors-$i.txt"
done

# 3. The pre-flight, the dry run, the run, the run again.
echo "--- check"
rs check | tee "$work/check.log" || fail "the pre-flight on the fork"
nonces() { for a in $signers; do c nonce "$a"; done | paste -sd' '; }
n0=$(nonces)
echo "--- run (dry)"
rs run | tee "$work/dry.log" || fail "the dry run"
[ "$(nonces)" = "$n0" ] || fail "the dry run sent a transaction"
grep -q "DRY RUN DONE: nothing sent" "$work/dry.log" || fail "the dry run did not finish"
# The recheck fact's draws on the fork, and every window a run used.
recheck_fact=$(sed -n 's/^plan: //p' "$work/dry.log" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["recheckFact"])')
registry=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["registry"])' "$work/export/deployment.json")
draws() { c call "$registry" 'drawCount(bytes32)(uint32)' "$recheck_fact" | awk '{print $1}'; }
utc() { python3 -I -c 'import datetime as d, sys; print(d.datetime.fromtimestamp(int(sys.argv[1]), d.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"))' "$1"; }
windows() { grep -ohE 'window [0-9T:-]+Z \((a new draw|resuming the draw the book holds)\)' "$@" || true; }
# Started straight in the background (no function, no subshell), so $! is the runner itself; as
# from a terminal, SIGINT is not ignored (a background job's is), so the runner's trap runs.
runner=(env --default-signal=INT scripts/recheck-sepolia.sh run "$manifest" "$date" "$work/export" --state "$work/state" --yes)
t1=$(date +%s)
dead=""
case $FAULT in
interrupt)
  echo "--- fault: SIGINT to the runner once its coordinator started, before the draw"
  "${runner[@]}" >"$work/run-fault.log" 2>&1 &
  rpid=$!
  for _ in $(seq 6000); do
    [ -s "$work/state/coordinator.pid" ] && break
    kill -0 "$rpid" 2>/dev/null || break
    sleep 0.1
  done
  [ -s "$work/state/coordinator.pid" ] || { cat "$work/run-fault.log"; fail "the coordinator did not start"; }
  cpid=$(cat "$work/state/coordinator.pid")
  kill -INT "$rpid"
  rc=0
  wait "$rpid" || rc=$?
  for _ in $(seq 100); do kill -0 "$cpid" 2>/dev/null || break; sleep 0.1; done
  ! kill -0 "$cpid" 2>/dev/null || fail "the coordinator $cpid outlived its runner"
  dead=$(cat "$work/state/window-start")
  echo "the runner stopped (exit $rc) with window $(utc "$dead") saved; the coordinator $cpid is gone; draws $(draws)"
  [ "$(draws)" = 0 ] || fail "the draw landed before the SIGINT: not the case to rehearse"
  echo "waiting until that window's commit close has passed ($(utc $((dead + 360))))"
  while [ "$(date +%s)" -le $((dead + 365)) ]; do sleep 5; done
  ;;
observers)
  echo "--- fault: every observer stopped once the draw is announced and opened"
  "${runner[@]}" >"$work/run-fault.log" 2>&1 &
  rpid=$!
  for _ in $(seq 1200); do
    grep -qsE "drawn in round|not drawn" "$work/state/logs/latest/"observer-*.log 2>/dev/null && break
    kill -0 "$rpid" 2>/dev/null || break
    sleep 0.5
  done
  grep -qsE "drawn in round|not drawn" "$work/state/logs/latest/"observer-*.log || { cat "$work/run-fault.log"; fail "no draw opened"; }
  dead=$(cat "$work/state/window-start")
  stopped=$(cat "$work/state/observers.pids")
  for p in $stopped; do kill "$p" 2>/dev/null || true; done
  echo "stopped the observers $(paste -sd' ' <<<"$stopped") before window $(utc "$dead") opened; draws $(draws)"
  rc=0
  wait "$rpid" || rc=$?
  tail -6 "$work/run-fault.log" | cut -c1-300
  [ "$rc" != 0 ] || fail "a round without observers passed"
  echo "the run stopped (exit $rc); draws $(draws)"
  ;;
lost)
  echo "--- fault: the coordinator's announceDraw dropped, the run stopped while the book holds it"
  touch "$work/drop-next-send"
  "${runner[@]}" >"$work/run-fault.log" 2>&1 &
  rpid=$!
  for _ in $(seq 2400); do
    [ -e "$work/dropped" ] && break
    kill -0 "$rpid" 2>/dev/null || break
    sleep 0.5
  done
  [ -e "$work/dropped" ] || { cat "$work/run-fault.log"; fail "no send was dropped"; }
  rm -f "$work/drop-next-send"
  sleep 5 # the coordinator now waits for the draw to land (confirm_announced)
  cpid=$(cat "$work/state/coordinator.pid")
  kill -INT "$rpid"
  rc=0
  wait "$rpid" || rc=$?
  for _ in $(seq 100); do kill -0 "$cpid" 2>/dev/null || break; sleep 0.1; done
  ! kill -0 "$cpid" 2>/dev/null || fail "the coordinator $cpid outlived its runner"
  dead=$(cat "$work/state/window-start")
  echo "the runner stopped (exit $rc) after the dropped announcement; window $(utc "$dead") saved; draws $(draws)"
  [ "$(draws)" = 0 ] || fail "the dropped announcement landed"
  echo "waiting until that window's commit close has passed ($(utc $((dead + 360))))"
  while [ "$(date +%s)" -le $((dead + 365)) ]; do sleep 5; done
  ;;
esac
echo "--- run --yes"
done_ok=""
for k in 1 2 3; do
  d0=$(draws)
  ok=""
  if rs run --yes 2>&1 | tee "$work/run-$k.log"; then ok=1; fi
  d1=$(draws)
  echo "run $k: draws $d0 -> $d1"
  # The dead window never gets a draw, whatever the run printed (review H1).
  if [ -n "$dead" ] && grep -qE "^2\. the recheck round .* window $(utc "$dead") \(" "$work/run-$k.log" \
    && [ "$d1" -gt "$d0" ]; then
    fail "run $k announced a draw for the dead window $(utc "$dead")"
  fi
  if [ -n "$ok" ]; then done_ok=$k; break; fi
  echo "run $k stopped; running again"
done
[ -n "$done_ok" ] || fail "run --yes did not finish in three runs"
cp "$work/run-$done_ok.log" "$work/run.log"
echo "run --yes took $(( $(date +%s) - t1 )) s in all ($done_ok run(s) after the fault: $FAULT)"
end_block=$(c block-number)
used=$(windows "$work"/run-[0-9].log)
echo "windows the runs used: $(paste -sd';' <<<"$used")"
if [ -n "$dead" ]; then
  ! grep -q "window $(utc "$dead") (a new draw)" <<<"$used" \
    || fail "a run announced a new draw for the dead window $(utc "$dead")"
  grep -q "(a new draw)" <<<"$used" || fail "no run picked a new window"
fi
if [ "$FAULT" = lost ]; then
  grep -q "window $(utc "$dead") (resuming the draw the book holds)" "$work/run-1.log" \
    || fail "run 1 did not resume the held draw of the dead window"
  grep -q "no new draw" "$work/run-1.log" || fail "run 1 was not refused a new draw for the dead window"
fi
want_draws=1
[ "$FAULT" != observers ] || want_draws=2
[ "$(draws)" = "$want_draws" ] || fail "the recheck fact has $(draws) draws, want $want_draws"
echo "the recheck fact has $(draws) draw(s), as it should after the fault ($FAULT)"
echo "--- run --yes again"
n1=$(nonces)
rs run --yes | tee "$work/run2.log" || fail "the second run --yes"
[ "$(nonces)" = "$n1" ] || fail "the second run sent a transaction"
echo "the second run sent nothing"

# 4. The checks: each market resolved by the contract's rule; each held winner paid
# OnyxPayout.share(stake, distributable, winningPool) = stake + stake * distributable /
# winningPool (rounded down) to the wei; every tONYX the contract paid went to a held key or the
# treasury; the treasury owed nothing; the contract's balance its liabilities.
balances "$work/tonyx.after"
for i in "${open_ids[@]}"; do mjson "$i" >"$work/market-$i.after.json"; done
for i in "${open_ids[@]}"; do
  python3 -I - "$work" "$i" "$fee_ppm" <<'PY' || fail "market $i"
import json, sys
work, i, fee_ppm = sys.argv[1], sys.argv[2], int(sys.argv[3])
b = json.load(open(f"{work}/market-{i}.before.json"))
a = json.load(open(f"{work}/market-{i}.after.json"))
assert int(a[8]) == 2, f"market {i} is not resolved: state {a[8]}"
value, recheck, tol = int(a[13]), int(a[14]), int(a[15])
w = 1 if abs(recheck - value) <= tol else 2
assert int(a[9]) == w, f"market {i}: winner {a[9]}, the rule says {w}"
pools = [int(x) for x in b[21]]
losers = sum(pools) - pools[w - 1]
fee = losers * fee_ppm // 1_000_000
assert int(a[19]) == losers - fee and int(a[18]) == pools[w - 1], a
print(f"market {i}: resolved; value {value}, recheck {recheck}, tolerance {tol}: {'Yes' if w == 1 else 'No'} won;"
      f" pools Yes {pools[0] / 1e18:g} / No {pools[1] / 1e18:g} tONYX, trading fee {fee / 1e18:g}")
PY
  read -r w dist wp <<<"$(python3 -I -c 'import json,sys; m=json.load(open(sys.argv[1])); print(m[9], m[19], m[18])' "$work/market-$i.after.json")"
  for a in $(cat "$work/bettors-$i.txt"); do
    grep -qx "$a" <<<"$signers" || { echo "market $i: $a holds no key here: it withdraws itself"; continue; }
    stake=$(c call "$C" 'betsOf(uint256,address)(uint128,uint128)' "$i" "$a" | awk '{print $1}' | sed -n "${w}p")
    share=$(python3 -I -c 'import sys; s, d, p = map(int, sys.argv[1:]); print(s + s * d // p if p else 0)' "$stake" "$dist" "$wp")
    got=$(python3 -I -c 'import sys; b = dict(l.split() for l in open(sys.argv[1])); c = dict(l.split() for l in open(sys.argv[2])); print(int(c[sys.argv[3]]) - int(b[sys.argv[3]]))' \
      "$work/tonyx.before" "$work/tonyx.after" "$a")
    [ "$got" = "$share" ] || fail "market $i: $a got $got wei, its share is $share"
    echo "market $i: $a was paid $(python3 -I -c 'import sys; print(int(sys.argv[1]) / 1e18)' "$share") tONYX ($share wei, its share to the wei)"
    [ "$(c call "$C" 'owed(uint256,address)(uint256)' "$i" "$a" | awk '{print $1}')" = 0 ] || fail "market $i: $a is still owed"
  done
done
python3 -I - "$work/tonyx.before" "$work/tonyx.after" "$treasury" "$C" <<'PY' || fail "the money"
import sys
b = {k: int(v) for k, v in (l.split() for l in open(sys.argv[1]))}
a = {k: int(v) for k, v in (l.split() for l in open(sys.argv[2]))}
treasury, contract = sys.argv[3], sys.argv[4]
d = {k: a[k] - b[k] for k in a}
t, out = d[treasury], -d[contract]
others = sum(v for k, v in d.items() if k not in (treasury, contract))
print(f"tONYX: the contract paid {out / 1e18:g}: {others / 1e18:g} to held winners, {t / 1e18:g} to the treasury")
assert t > 0, "the treasury was not paid"
assert out == t + others, "the contract paid someone else"
PY
[ "$(c call "$C" 'treasuryOwed()(uint256)' | awk '{print $1}')" = 0 ] || fail "the treasury is still owed"
[ "$(bal "$C")" = "$(c call "$C" 'liabilities()(uint256)' | awk '{print $1}')" ] || fail "the contract's balance is not its liabilities"
echo "the contract holds exactly its liabilities; the treasury is owed nothing"

# The gas of every transaction on the fork since the close, by step.
abi() { python3 -I -c 'import json,sys; print(json.dumps(json.load(open(sys.argv[1]))["methodIdentifiers"]))' "$REPO/contracts/out/$1"; }
[ -f "$REPO/contracts/out/OnyxRegistry.sol/OnyxRegistry.json" ] || (cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >/dev/null)
python3 -I - "$RPC" "$start_block" "$end_block" "$(abi OnyxRegistry.sol/OnyxRegistry.json)" "$(abi OnyxCorrections.sol/OnyxCorrections.json)" "$C" <<'PY'
import collections, json, sys, urllib.request
rpc, a, b, reg_abi, cor_abi, corrections = sys.argv[1:]
names = {v: k.split("(")[0] for k, v in {**json.loads(reg_abi), **json.loads(cor_abi)}.items()}
def call(method, *params):
    r = urllib.request.Request(rpc, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": list(params)}).encode(), headers={"content-type": "application/json"})
    return json.load(urllib.request.urlopen(r))["result"]
rows = collections.defaultdict(lambda: [0, 0])
total = 0
for n in range(int(a), int(b) + 1):
    blk = call("eth_getBlockByNumber", hex(n), True)
    for tx in blk["transactions"]:
        rc = call("eth_getTransactionReceipt", tx["hash"])
        gas = int(rc["gasUsed"], 16)
        sel = (tx.get("input") or "0x")[2:10]
        step = "resolve/withdraw/payTreasury" if (tx.get("to") or "").lower() == corrections.lower() else "the recheck round"
        k = (step, names.get(sel, "0x" + sel), tx["from"].lower())
        rows[k][0] += 1
        rows[k][1] += gas
        total += gas
print(f"gas on the fork, blocks {a} to {b} (every transaction):")
for (step, fn, frm), (count, gas) in sorted(rows.items()):
    print(f"  {step:30} {fn:24} from {frm}: {count} tx, {gas} gas")
print(f"  total {total} gas")
PY
echo "RECHECK FORK PASSED"
