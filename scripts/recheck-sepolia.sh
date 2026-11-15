#!/usr/bin/env bash
# CL7 (ADR-0057 item 7, docs/ops/runbooks/corrections-base-sepolia.md steps 7 and 8): the recheck
# runner of the correction markets of one value on Base Sepolia (TEST NETWORK ONLY), from the
# plan to the treasury's payment:
#   1. `level2-manifest recheck-plan` reads the value's markets and bettors from OnyxCorrections
#      and stops cleanly on `none` (no market), `wait` (trading open: the close block and its
#      estimated UTC time) or `void-only` (no recheck day left after the close).
#   2. On `run`: the recheck round of the paired `.recheck` metric, run as scripts/level2-sepolia.sh
#      runs a manifest round (the coordinator and one observer process per pool member, each with
#      `--recheck A:S`), the coordinator with `--pool <the plan's pool> --window-start W`.
#      Observers come ONLY from the plan's pool: the candidates (RECHECK_POOL_KEYS) without any
#      bettor of the value's markets and any observer of the original round (THREATS CL7-3).
#      The window (THREATS CL7-24, CL7-25): when the round book holds a draw (`level2-manifest
#      book-held`), the round resumes on that draw's own window, whatever the time, so no second
#      draw is announced; with none held, a new window: the plan's `windowStart`, or
#      ONYX_L2_LEAD seconds from now when that is later (a live round is observed in real time),
#      on a recheck day, and refused when about 40 minutes of blocks from now reach the deadline.
#   3. `recheck-resolve` sends `resolve` for every market the round serves; `recheck-payout`
#      sends `withdraw` for every key of RECHECK_BETTOR_KEYS owed something, then `payTreasury()`.
# Every step reads the chain first and sends nothing for what is done. A run that stopped is
# finished by running it again with the same state dir. One run at a time per value holds a lock
# (flock, outside the state dir, in RECHECK_LOCK_DIR, default ~/.cache/onyx-b6/locks); each run
# logs to its own state/logs/<UTC time>/ (state/logs/latest names the last).
# `run --yes` takes about 35 minutes: run it where a closed terminal does not stop it, e.g.
# `systemd-run --user --scope -p MemoryMax=12G scripts/recheck-sepolia.sh ...` inside tmux.
#
#   scripts/recheck-sepolia.sh check MANIFEST DATE EXPORT_DIR [--state DIR]
#       the pre-flight, read-only: PASS/FAIL per line, exit 1 on any FAIL
#   scripts/recheck-sepolia.sh run MANIFEST DATE EXPORT_DIR [--state DIR] [--yes]
#       the runner; without --yes a dry run (every transaction, its sender and key path, gas
#       estimates; nothing sent)
#
# EXPORT_DIR: the node export of the testnet run (as scripts/level2-sepolia.sh): deployment.json
# names the registry (it must be OnyxCorrections.registry()) and OnyxSettlement;
# observations/<source>.json is the original round's results store (only read; its round must
# be the one the registry recorded); the recheck's store is observations/<source>.recheck.json
# and its pin goes to observations/pins (scripts/deploy-api.sh ships both). The state dir
# defaults to ~/.cache/onyx-b6/<source>.recheck-<DATE>.
#
# Who signs (keys are read by the programs, never printed or put on a command line):
# - the coordinator: BASE_SEPOLIA_PRIVATE_KEY from ONYX_ENV_FILE (the deployer), read here and
#   passed to the coordinator process only, in its environment (any such variable inherited by
#   this script is dropped first); it must be OnyxSettlement.coordinator()
# - observers: the plan's pool members, keys/<name>.key under ONYX_TESTNET_DIR
# - the receipt key: keys/l2-receipt.key (signs receipts, sends nothing)
# - resolve and payTreasury: keys/<RECHECK_SENDER_KEY>.key (default miner-0, the market's opener)
# - withdraw: each key of RECHECK_BETTOR_KEYS that the contract owes something
#
# The chain: a public RPC must be Base Sepolia (chain id 84532) and must serve archive state
# (the coordinator reads code at past blocks). A loopback RPC is the fork of the rehearsal
# (scripts/recheck-fork.sh) only: its chain id must be RECHECK_FORK_CHAIN_ID (845320), never
# 84532, so nothing signed there is valid on Base Sepolia; any other id there is refused.
# Env: ONYX_L2_RPC (https://sepolia.base.org), ONYX_TESTNET_DIR (.onyx-testnet), ONYX_ENV_FILE
# (.env), ONYX_CORRECTIONS (default: `contract` in ONYX_TESTNET_DIR/corrections-sepolia.json),
# ONYX_L2_LEAD (150), CORRECTIONS_AFTER_DAYS and CORRECTIONS_SPAN_DAYS (7 and 7),
# RECHECK_POOL_KEYS (validator-0 .. validator-15), RECHECK_BETTOR_KEYS (miner-0 miner-1 customer
# validator-0 .. validator-8), RECHECK_SENDER_KEY (miner-0), RECHECK_MARGIN_BLOCKS (the plan's
# default), RECHECK_BLOCK_MS (the block time for time estimates; default measured over the last
# 432,000 blocks), RECHECK_FORK_CHAIN_ID (845320), RECHECK_LOCK_DIR (~/.cache/onyx-b6/locks),
# FOUNDRY_BIN (~/.foundry/bin), CARGO_TARGET_DIR.
set -euo pipefail
# The coordinator's key reaches it only through this script's own read of ONYX_ENV_FILE.
unset ETH_PRIVATE_KEY BASE_SEPOLIA_PRIVATE_KEY ONYX_L2_COORDINATOR_KEY
cd "$(dirname "$0")/.."
REPO=$PWD
usage() {
  sed -n '/^#   scripts\/recheck-sepolia.sh check/,/estimates; nothing sent)/p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
}
cmd=${1:-}
case "$cmd" in check | run) ;; *) usage ;; esac
[ $# -ge 4 ] || usage
manifest=$(realpath -e "$2")
date=$3
export_dir=$(cd "$4" && pwd)
shift 4
yes="" state=""
while [ $# -gt 0 ]; do
  case $1 in
  --yes) [ "$cmd" = run ] || usage; yes=1 ;;
  --state) state=${2:-}; [ -n "$state" ] || usage; shift ;;
  *) usage ;;
  esac
  shift
done
[[ $date =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || { echo "DATE: YYYY-MM-DD" >&2; exit 2; }
name=$(basename "$(dirname "$manifest")")
state=${state:-${ONYX_RECHECK_STATE:-$HOME/.cache/onyx-b6/$name.recheck-$date}}
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
CAST=$FOUNDRY_BIN/cast
RPC=${ONYX_L2_RPC:-https://sepolia.base.org}
LEAD=${ONYX_L2_LEAD:-150}
FORK_CHAIN_ID=${RECHECK_FORK_CHAIN_ID:-845320}
TESTNET=${ONYX_TESTNET_DIR:-$REPO/.onyx-testnet}
KEYS=$TESTNET/keys
ENV_FILE=${ONYX_ENV_FILE:-$REPO/.env}
after=${CORRECTIONS_AFTER_DAYS:-7}
span=${CORRECTIONS_SPAN_DAYS:-7}
recheck=$after:$span
read -r -a pool_keys <<<"${RECHECK_POOL_KEYS:-$(printf 'validator-%d ' $(seq 0 15))}"
read -r -a bettor_keys <<<"${RECHECK_BETTOR_KEYS:-miner-0 miner-1 customer $(printf 'validator-%d ' $(seq 0 8))}"
sender_name=${RECHECK_SENDER_KEY:-miner-0}
for n in "${pool_keys[@]}" "${bettor_keys[@]}" "$sender_name"; do
  [[ $n =~ ^[a-z0-9-]+$ ]] || { echo "key names: lowercase letters, digits and dashes ($n)" >&2; exit 2; }
done
orig_store=$export_dir/observations/$name.json
rstore=$export_dir/observations/$name.recheck.json
receipt=$KEYS/l2-receipt.key
sender=$KEYS/$sender_name.key
margin=()
[ -z "${RECHECK_MARGIN_BLOCKS:-}" ] || margin=(--margin-blocks "$RECHECK_MARGIN_BLOCKS")

# Gas per transaction, measured on the fork rehearsal (scripts/recheck-fork.sh, 2026-11-15) and
# rounded up by about half: the budgets the pre-flight checks balances against and the dry run
# shows where no estimate is possible yet (nothing to resolve or withdraw before the round).
GAS_COORDINATOR=1500000 # the coordinator's round: announceDraw 161,684, fixBeacon 50,424, five
                        # postReceipt 563,451, recordObservation 202,295 (977,854 in all)
GAS_COMMIT=270000       # one drawn observer's postCommit: 179,667
GAS_RESOLVE=230000      # 147,499
GAS_WITHDRAW=150000     # 97,394
GAS_PAY_TREASURY=80000  # 53,314
# A balance passes when it covers its budget at three times the fee a signer pays now
# (`KeySigner`: twice the base fee plus the tip).
FEE_HEADROOM=3
# A live round from its draw to its record: the lead, the 300 s window, commit and reveal, then
# about 20 minutes until the commit phase is final on Base Sepolia (CL7-24).
ROUND_SECS=2400

work=$(mktemp -d)
chmod 700 "$work"
pids=()
cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP
# cast runs outside the repo: Foundry loads a .env from its working directory (M8-L55-F4).
castw() { (cd "$work" && ETH_RPC_URL=$RPC "$CAST" "$@"); }
lower() { tr '[:upper:]' '[:lower:]'; }
first() { awk '{print $1}'; }
line1() { awk 'NR == 1'; }

cid=$(castw chain-id)
fork=""
if [[ $RPC =~ ^http://(127\.0\.0\.1|localhost):[0-9]+/?$ ]]; then
  case $FORK_CHAIN_ID in 84532 | 31337 | *[!0-9]*) echo "RECHECK_FORK_CHAIN_ID: a number, not 84532 or 31337" >&2; exit 2 ;; esac
  [ "$cid" = "$FORK_CHAIN_ID" ] \
    || { echo "a loopback RPC is the rehearsal's fork only: its chain id must be $FORK_CHAIN_ID (RECHECK_FORK_CHAIN_ID), not $cid; refused" >&2; exit 1; }
  fork=$cid
  net="the local fork (chain $cid; nothing reaches Base Sepolia)"
else
  [ "$cid" = 84532 ] || { echo "chain $cid is not Base Sepolia (84532): refused" >&2; exit 1; }
  net="Base Sepolia"
fi

# Private copies of the programs (a build during the round must not swap them).
bin=$work/bin
mkdir "$bin"
CARGO_INCREMENTAL=0 cargo build --locked -q -p onyx-level2 --example level2-manifest --example onyx-source
cp "${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/level2-manifest" \
  "${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/onyx-source" "$bin/"
lm() { "$bin/level2-manifest" "$@" --manifest "$manifest" --repo "$REPO"; }
for role in coordinator observer; do
  printf '#!/bin/sh\nexec %q %s --manifest %q --repo %q --recheck %q "$@"\n' \
    "$bin/level2-manifest" "$role" "$manifest" "$REPO" "$recheck" >"$bin/level2-$role"
  chmod 700 "$bin/level2-$role"
done
addr_of() { "$bin/level2-observer" --key-file "$1" --address | lower; }

# The value's ids (the recheck metric, template, subject and recheck days; ADR-0057 item 2).
"$bin/onyx-source" recheck "$manifest" --after-days "$after" --span-days "$span" --date "$date" >"$work/ids.txt"
id() { awk -v k="$1" '$1 == k {print $2}' "$work/ids.txt"; }
for k in original_metric_key recheck_metric recheck_metric_key recheck_template_id tolerance subject subject_key recheck_days; do
  [ -n "$(id "$k")" ] || { echo "onyx-source recheck printed no $k" >&2; exit 1; }
done
first_day=$(id recheck_days)
last_day=$(awk '$1 == "recheck_days" {print $4}' "$work/ids.txt")
day_unix() { python3 -I -c 'import datetime as d, sys; print(int(d.datetime.strptime(sys.argv[1], "%Y-%m-%d").replace(tzinfo=d.timezone.utc).timestamp()))' "$1"; }
utc() { python3 -I -c 'import datetime as d, sys; print(d.datetime.fromtimestamp(int(sys.argv[1]), d.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"))' "$1"; }
recheck_end=$(( $(day_unix "$last_day") + 86400 ))

# The deployment: the export's registry must be the one the contract settles from (M1).
json_get() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1])).get(sys.argv[2], ""))' "$1" "$2"; }
[ -f "$export_dir/deployment.json" ] || { echo "$export_dir/deployment.json is missing" >&2; exit 1; }
REG=$(json_get "$export_dir/deployment.json" registry | lower)
SETTLEMENT=$(json_get "$export_dir/deployment.json" settlement)
REVENUE=$(json_get "$export_dir/deployment.json" revenue)
C=${ONYX_CORRECTIONS:-}
if [ -z "$C" ] && [ -f "$TESTNET/corrections-sepolia.json" ]; then
  C=$(json_get "$TESTNET/corrections-sepolia.json" contract)
fi
[[ $C =~ ^0x[0-9a-fA-F]{40}$ ]] || { echo "no OnyxCorrections address: set ONYX_CORRECTIONS or deploy it (scripts/corrections-sepolia.sh)" >&2; exit 1; }
C=$(lower <<<"$C")
REG_C=$(castw call "$C" 'registry()(address)' | lower)
if [ "$REG_C" != "$REG" ] && [ "$cmd" = run ]; then
  echo "OnyxCorrections $C settles from registry $REG_C, but $export_dir/deployment.json names $REG: another deployment's export; refused" >&2
  exit 1
fi
COORD=$(castw call "$SETTLEMENT" 'coordinator()(address)' | lower)
REPORTER=$(castw call "$REVENUE" 'reporter()(address)')
# A struct `cast call --json` returned, as a JSON list (some cast versions quote it).
tuple() { python3 -I -c 'import json,sys; m=json.load(sys.stdin)[0]; m = m if isinstance(m, list) else json.loads(m); json.dump(m, sys.stdout)'; }
MARKET_SIG='market(uint256)((bytes32,bytes32,bytes32,bytes32,address,uint64,uint64,uint64,uint8,uint8,uint8,uint32,uint32,int128,int128,uint128,uint128,uint128,uint128,uint128,uint128,uint128[2],uint32[2]))'
OBS_SIG='observationOf(bytes32,bytes32,uint64)((bytes32,bytes32,bytes32,uint64,bytes32,bytes32,uint64))'
bettors_of() { castw call "$C" 'bettorsOf(uint256,uint256,uint256)(address[],uint256)' "$1" 0 200 | line1 | tr -d '[],' | lower; }

# The candidates' addresses and key files; the pool snapshot the plan filters.
declare -A key_of name_of
need_key() { [ -f "$1" ] || { echo "missing key file $1" >&2; exit 1; }; }
for n in "${pool_keys[@]}"; do
  need_key "$KEYS/$n.key"
  a=$(addr_of "$KEYS/$n.key")
  key_of[$a]=$KEYS/$n.key
  name_of[$a]=$n
done
python3 -I -c 'import json,sys; json.dump([{"address": a, "bond": 1000} for a in sorted(sys.argv[2:])], open(sys.argv[1], "w"))' \
  "$work/candidates.json" "${!key_of[@]}"
declare -A bettor_key_of
for n in "${bettor_keys[@]}"; do
  [ -f "$KEYS/$n.key" ] || { echo "note: no key file $KEYS/$n.key: left out of the withdrawals" >&2; continue; }
  a=$(addr_of "$KEYS/$n.key")
  bettor_key_of[$a]=$KEYS/$n.key
  name_of[$a]=${name_of[$a]:-$n}
done
need_key "$sender"
sender_addr=$(addr_of "$sender")
name_of[$sender_addr]=${name_of[$sender_addr]:-$sender_name}

# The head block, its time and the block time in ms over the last 432,000 blocks (as
# corrections-sepolia.sh); at BLOCK prints the block's estimated UTC time.
head=$(castw block-number)
head_time=$(castw block "$head" --field timestamp)
then_time=$(castw block $((head - 432000)) --field timestamp)
block_ms=${RECHECK_BLOCK_MS:-$(python3 -I -c 'import sys; a, b = map(int, sys.argv[1:]); print(max(1, round((a - b) * 1000 / 432000)))' "$head_time" "$then_time")}
# A past block's own time; a later one's estimated from the block time.
block_unix() {
  if [ "$1" -le "$head" ]; then castw block "$1" --field timestamp; else echo $((head_time + ($1 - head) * block_ms / 1000)); fi
}
at() { utc "$(block_unix "$1")"; }

plan() {
  lm recheck-plan --recheck "$recheck" --record-date "$date" --rpc "$RPC" --cast "$CAST" \
    --corrections "$C" --pool "$work/candidates.json" --original-store "$orig_store" \
    --out-pool "$work/plan-pool.json" "${margin[@]}"
}
pj() { python3 -I -c 'import json,sys; v=json.loads(sys.argv[1]).get(sys.argv[2]); print("" if v is None else json.dumps(v) if isinstance(v, (list, dict, bool)) else v)' "$p" "$1"; }
# The plan's pool members (written by recheck-plan), as addresses.
pool_members() { python3 -I -c 'import json,sys; print(" ".join(m["address"].lower() for m in json.load(open(sys.argv[1]))))' "$1"; }
min_observers=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["template"]["min_observers"])' "$manifest")

# The recheck of the value in the contract's registry: sets recorded (its result or empty) and
# draws (the recheck fact's draw count).
recheck_state() {
  local m we
  m=$(castw call "$C" "$MARKET_SIG" "$1" --json)
  we=$(tuple <<<"$m" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)[5])')
  recorded=$(castw call "$REG_C" "$OBS_SIG" "$(id recheck_metric_key)" "$(id subject_key)" "$we" --json \
    | tuple | python3 -I -c 'import json,sys; o=json.load(sys.stdin); print("" if int(o[5], 16) == 0 else o[5])')
  draws=$(castw call "$REG_C" 'drawCount(bytes32)(uint32)' "$(pj recheckFact)" | first)
}
# Fee per gas a KeySigner pays now (twice the base fee plus the tip), with the headroom.
fee_wei() {
  local base tip
  base=$(castw base-fee)
  tip=$(castw rpc eth_maxPriorityFeePerGas | tr -d '"')
  python3 -I -c 'import sys; print((2 * int(sys.argv[1]) + int(sys.argv[2], 16)) * int(sys.argv[3]))' "$base" "$tip" "$FEE_HEADROOM"
}
eth() { python3 -I -c 'import sys; print(f"{int(sys.argv[1]) / 1e18:.8f}")' "$1"; }
# Whether the round book holds a draw of the recheck fact, and for window $1: sets held (1 or
# empty), held_same (1 when for that window) and book_draws (the draws the book counts; empty
# once its fact is settled). The book is read by the coordinator's own code.
book_held() {
  local j
  j=$(lm book-held --recheck "$recheck" --book "$state/book.json" --record-date "$date" --window-start "$1") || return 1
  read -r held held_same book_draws <<<"$(python3 -I -c 'import json,sys; j=json.loads(sys.argv[1]); d=j.get("bookDraws"); print("1" if j["held"] else "-", "1" if j.get("sameWindow") else "-", "-" if d is None else d)' "$j")"
  [ "$held" = 1 ] || held=""
  [ "$held_same" = 1 ] || held_same=""
  [ "$book_draws" != - ] || book_draws=""
}
# The process ids in the files given that are still alive.
live_pids() { for f in "$@"; do [ -s "$f" ] && cat "$f"; done 2>/dev/null | while read -r q; do kill -0 "$q" 2>/dev/null && echo "$q"; done | paste -sd' '; }

p=$(plan) || exit 1
st=$(pj state)
echo "$net, head block $head (~$(utc "$head_time"), ${block_ms} ms blocks); OnyxCorrections $C"
echo "value: $(id subject) ($(id recheck_metric) template $(id recheck_template_id), recheck days $first_day to $last_day)"
echo "plan: $(python3 -I -c 'import json,sys; print(json.dumps(json.loads(sys.argv[1]), separators=(",", ":")))' "$p")"

########## check: the pre-flight (read-only) ##########
if [ "$cmd" = check ]; then
  fails=0
  ok() { echo "PASS $*"; }
  bad() { echo "FAIL $*"; fails=$((fails + 1)); }
  if [ -n "$fork" ]; then ok "chain: the local fork, chain id $cid (rehearsal)"; else ok "chain: Base Sepolia (84532)"; fi
  if [ "$REG_C" = "$REG" ]; then ok "OnyxCorrections $C reads the registry $REG of deployment.json"
  else bad "OnyxCorrections $C reads registry $REG_C, deployment.json names $REG"; fi
  row=$(castw call "$REG_C" 'metrics(bytes32)(uint8,bytes32,uint64)' "$(id recheck_metric_key)" | first | paste -sd' ')
  read -r kind tpl _ <<<"$row"
  if [ "$kind" = 3 ] && [ "$(lower <<<"$tpl")" = "$(id recheck_template_id)" ]; then
    ok "the recheck metric $(id recheck_metric) is registered: kind 3 (dated), template $(id recheck_template_id)"
  else
    bad "the recheck metric $(id recheck_metric): the registry holds kind $kind template $tpl, want 3 $(id recheck_template_id)"
  fi
  pair=$(castw call "$C" 'pairs(bytes32)(bytes32,uint128)' "$(id original_metric_key)" | first | paste -sd' ')
  read -r pm ptol <<<"$pair"
  if [ "$(lower <<<"$pm")" = "$(id recheck_metric_key)" ] && [ "$ptol" = "$(id tolerance)" ]; then
    ok "the pair: $(id original_metric_key) -> $(id recheck_metric_key), tolerance $ptol"
  else
    bad "the pair: the contract holds $pm tolerance $ptol, want $(id recheck_metric_key) tolerance $(id tolerance)"
  fi
  if [ -f "$ENV_FILE" ]; then
    # The deployer's key goes to the program in its environment only; the address comes back.
    key_addr=$(ONYX_L2_COORDINATOR_KEY=$(python3 -I - "$ENV_FILE" <<'PY'
import sys
for line in open(sys.argv[1]).read().splitlines():
    k, sep, v = line.partition("=")
    if sep and k.strip() == "BASE_SEPOLIA_PRIVATE_KEY":
        print(v.strip().strip('"').strip("'"))
        break
PY
) lm coordinator-address 2>&1 | lower) || true
    if [ "$key_addr" = "$COORD" ]; then
      ok "coordinator: BASE_SEPOLIA_PRIVATE_KEY in $ENV_FILE is OnyxSettlement.coordinator() $COORD"
    else
      bad "coordinator: BASE_SEPOLIA_PRIVATE_KEY in $ENV_FILE gives ${key_addr:-no key}, OnyxSettlement.coordinator() is $COORD"
    fi
  else
    bad "coordinator: $ENV_FILE is missing (BASE_SEPOLIA_PRIVATE_KEY)"
  fi
  case $st in
  none)
    if [ "$(pj markets)" = "[]" ]; then bad "markets: $(id subject) has no market on $C"
    else ok "markets: $(pj markets), none open (settled: run pays out what is owed)"; fi
    ;;
  void-only) bad "markets: $(pj markets): void-only ($(pj reason))" ;;
  wait | run)
    after_block=$(pj afterBlock)
    deadline=$(pj deadlineBlock)
    ok "markets: $(pj markets) (served $(pj served), left to void $(pj left)); state $st: trading closes at block $after_block (~$(at "$after_block")), recheck recorded by block $deadline (~$(at "$deadline"))"
    # A round starts at the close or now, whichever is later, and needs about 40 minutes.
    start_est=$(block_unix "$after_block")
    [ "$start_est" -ge "$head_time" ] || start_est=$head_time
    if [ $(( start_est + LEAD + ROUND_SECS )) -lt "$recheck_end" ] \
      && [ $(( start_est + LEAD + ROUND_SECS )) -lt "$(block_unix "$deadline")" ]; then
      ok "time: a round from ~$(utc "$start_est") fits the recheck days ($first_day to $last_day) and the deadline"
    else
      bad "time: a round from ~$(utc "$start_est") does not fit the recheck days ($first_day to $last_day) or the deadline"
    fi
    pool_n=$(pj pool)
    if [ -n "$pool_n" ] && [ "$pool_n" -ge "$min_observers" ]; then
      ok "recheck pool: $pool_n of ${#pool_keys[@]} candidates (left out $(pj excluded): bettors and original observers checked against the registry's recorded round; $( [ "$st" = wait ] && echo "more bettors may join until the close; ")the round draws $min_observers)"
    else
      bad "recheck pool: ${pool_n:-no} members, the round draws $min_observers"
    fi
    ;;
  esac
  if [ -f "$orig_store" ]; then ok "original round's store: $orig_store"; else bad "original round's store: $orig_store is missing"; fi
  if [ -f "$receipt" ]; then ok "receipt key: $receipt (signs receipts, sends nothing)"; else bad "receipt key: $receipt is missing"; fi
  # Balances: each signer's transactions at three times today's fee.
  fee=$(fee_wei)
  declare -A need why
  addneed() { need[$1]=$(( ${need[$1]:-0} + $2 * fee )); why[$1]="${why[$1]:+${why[$1]}, }$3"; }
  addneed "$COORD" "$GAS_COORDINATOR" "coordinator (BASE_SEPOLIA_PRIVATE_KEY in $ENV_FILE)"
  if [ -s "$work/plan-pool.json" ]; then
    for a in $(pool_members "$work/plan-pool.json"); do addneed "$a" "$GAS_COMMIT" "observer, ${key_of[$a]}"; done
  fi
  addneed "$sender_addr" $((GAS_RESOLVE + GAS_PAY_TREASURY)) "resolve and payTreasury, $sender"
  bettors=" "
  for m in $(pj markets | tr -d '[],'); do bettors+="$(bettors_of "$m" | paste -sd' ') "; done
  for a in "${!bettor_key_of[@]}"; do
    case $bettors in *" $a "*) addneed "$a" "$GAS_WITHDRAW" "withdraw if it wins, ${bettor_key_of[$a]}" ;; esac
  done
  for a in $(printf '%s\n' "${!need[@]}" | sort); do
    bal=$(castw balance "$a")
    line="balance $a (${name_of[$a]:-deployer}): $(eth "$bal") ETH, needs $(eth "${need[$a]}") for ${why[$a]}"
    if python3 -I -c 'import sys; sys.exit(0 if int(sys.argv[1]) >= int(sys.argv[2]) else 1)' "$bal" "${need[$a]}"; then ok "$line"; else bad "$line"; fi
  done
  echo "fee: $(python3 -I -c 'import sys; print(int(sys.argv[1]) / 1e9)' "$fee") gwei per gas ($FEE_HEADROOM x today's); budgets: coordinator $GAS_COORDINATOR, commit $GAS_COMMIT, resolve $GAS_RESOLVE, withdraw $GAS_WITHDRAW, payTreasury $GAS_PAY_TREASURY gas"
  if [ "$fails" -gt 0 ]; then echo "PRE-FLIGHT FAILED: $fails check(s)"; exit 1; fi
  echo "PRE-FLIGHT PASSED"
  exit 0
fi

########## run ##########
mode="DRY RUN (nothing is sent; --yes sends)"
[ -z "$yes" ] || mode="SENDING on $net"
echo "$mode"
case $st in
wait)
  echo "WAIT: trading on market(s) $(pj served) is open until block $(pj afterBlock) (~$(at "$(pj afterBlock)")); the recheck round may start after it, on a recheck day ($first_day to $last_day), and must be recorded by block $(pj deadlineBlock) (~$(at "$(pj deadlineBlock)")). Run again then."
  ;;
void-only)
  echo "VOID-ONLY: market(s) $(pj open) can only void ($(pj reason)); after the deadline anyone may send void(id), which refunds every bet."
  exit 0
  ;;
none)
  [ "$(pj markets)" != "[]" ] || { echo "NONE: $(id subject) has no market on $C: nothing to run."; exit 0; }
  echo "no open market: $(pj markets) settled; only the withdrawals and the treasury's payment are left"
  ;;
run) ;;
*) echo "unknown plan state $st" >&2; exit 1 ;;
esac

# The keys the payout uses: every held key that bet on one of the value's markets.
payout_keys=()
for a in "${!bettor_key_of[@]}"; do payout_keys+=(--key-file "${bettor_key_of[$a]}"); done
chain_flags=(--recheck "$recheck" --record-date "$date" --rpc "$RPC" --cast "$CAST" --corrections "$C")

if [ "$st" = wait ]; then
  # What `run --yes` sends after the close; nothing can be estimated on chain before it.
  fee=$(fee_wei)
  echo "after the close, run --yes sends (gas: the fork rehearsal's budgets; ETH at $FEE_HEADROOM x today's fee):"
  echo "  1. the recheck round (about 35 minutes): the coordinator $COORD (key: BASE_SEPOLIA_PRIVATE_KEY in $ENV_FILE)"
  echo "     announces the draw, fixes its beacon, posts the receipts and records the value: <= $GAS_COORDINATOR gas, $(eth $((GAS_COORDINATOR * fee))) ETH"
  echo "     the receipt key $receipt signs receipts (sends nothing)"
  echo "     $(pj minObservers) observers drawn from the pool (provisional, $(pj pool) members) each send one commit: <= $GAS_COMMIT gas, $(eth $((GAS_COMMIT * fee))) ETH:"
  for a in $(pool_members "$work/plan-pool.json"); do echo "       $a ${key_of[$a]}"; done
  echo "  2. resolve(market, recheckRound, recheckValue) for market(s) $(pj served) from $sender_addr (key: $sender): <= $GAS_RESOLVE gas each"
  echo "  3. withdraw(market) from each held key the market owes (the winners), <= $GAS_WITHDRAW gas each; held keys that bet:"
  for m in $(pj markets | tr -d '[],'); do
    for a in $(bettors_of "$m"); do
      bets=$(castw call "$C" 'betsOf(uint256,address)(uint128,uint128)' "$m" "$a" | first | paste -sd' ')
      echo "       market $m: $a ${bettor_key_of[$a]:-(no key held: withdraws itself)}, Yes/No wei: $bets"
    done
  done
  echo "  4. payTreasury() from $sender_addr (key: $sender): <= $GAS_PAY_TREASURY gas"
  exit 0
fi

# One run at a time per value, whatever its state dir (the coordinator also locks its book): the
# lock is per chain, contract and value, outside the state dir (RECHECK_LOCK_DIR).
if [ -n "$yes" ]; then
  mkdir -p "$state"
  chmod 700 "$state"
  lock_dir=${RECHECK_LOCK_DIR:-$HOME/.cache/onyx-b6/locks}
  mkdir -p -m 700 "$lock_dir"
  lock=$lock_dir/$cid-$C-$name-$date.lock
  exec 9>>"$lock"
  if ! flock -n 9; then
    holder=$(cat "$lock.holder" 2>/dev/null || true)
    hstate=$(sed -n 's/^state=//p' <<<"$holder")
    echo "another run of $(id subject) holds $lock (${holder:-no holder recorded}): refused" >&2
    if [ -n "$hstate" ]; then
      echo "  still alive there: coordinator ${hstate:+$(live_pids "$hstate/coordinator.pid")}; observers $(live_pids "$hstate/observers.pids")" >&2
      echo "  (orphans of a killed run keep the lock until they exit; stop them by these PIDs)" >&2
    fi
    exit 1
  fi
  printf 'state=%s\npid=%s\n' "$state" "$$" >"$lock.holder"
fi

served_first=""
[ "$st" != run ] || served_first=$(pj served | tr -d '[]' | cut -d, -f1)
nonces() { for a in "$@"; do castw nonce "$a"; done | paste -sd' '; }
balances() { for a in "$@"; do castw balance "$a"; done | paste -sd' '; }
actors=("$COORD" "$sender_addr")
members=()
[ ! -s "$work/plan-pool.json" ] || read -r -a members <<<"$(pool_members "$work/plan-pool.json")"
actors+=("${members[@]}" "${!bettor_key_of[@]}")
read -r -a actors <<<"$(printf '%s\n' "${actors[@]}" | sort -u | paste -sd' ')"
n0=$(nonces "${actors[@]}")
b0=$(balances "${actors[@]}")
rc_all=0

# The recheck's store holds the settled round of this value (read as recheck-resolve reads it):
# store_err holds why not.
store_ok() {
  store_err=""
  [ -s "$rstore" ] || { store_err="no recheck store $rstore yet"; return 1; }
  store_err=$(lm recheck-resolve "${chain_flags[@]}" "${margin[@]}" --store "$rstore" --key-file "$sender" --dry-run 2>&1 >/dev/null)
}
# The node's pin for the recheck's store, whenever the store exists (as level2-sepolia.sh writes
# the original's): a store with only a failed attempt is served too.
write_pin() {
  [ -s "$rstore" ] || return 0
  local pin pins=$export_dir/observations/pins
  pin=$(lm pin --recheck "$recheck")
  touch "$pins"
  (
    flock -w 60 8 || { echo "$pins.lock is held: the pin was not written" >&2; exit 1; }
    tmp=$(mktemp "$pins.XXXXXX")
    { grep -v "^${pin%%=*}=" "$pins" || true; echo "$pin"; } >"$tmp"
    chmod --reference="$pins" "$tmp"
    mv "$tmp" "$pins"
  ) 8>>"$pins.lock"
  echo "   pin $pin in $pins"
}

########## 2. the recheck round ##########
if [ "$st" = run ]; then
  recheck_state "$served_first"
  if [ -n "$recorded" ] && store_ok; then
    echo "2. the recheck round: recorded on chain (result $recorded) and in $rstore: nothing to run"
  else
    [ -z "$recorded" ] || echo "the recheck is recorded on chain but the store lacks it ($store_err): running the round again on its book to finish it"
    # The window (CL7-25): a draw the book holds is resumed on its own window, whatever the time
    # (it may need only its receipts and record); with none held, a new one (the plan's start, or
    # LEAD seconds from now when that is later), checked against the deadline and the recheck days.
    plan_start=$(pj windowStart)
    window_start="" resumed_round=""
    book_held "$plan_start" || { echo "the round book $state/book.json could not be read: fix it by hand" >&2; exit 1; }
    if [ -z "$held" ]; then
      # Another book's draw (a lost book, another --state): the chain counts more than this book.
      if [ -n "$book_draws" ] && [ "$draws" -gt "$book_draws" ]; then
        echo "the chain holds $draws draw(s) of the recheck fact, the book in $state counts $book_draws and holds none: another state dir's (or a lost book's) draw; run with that state dir; refused" >&2
        exit 1
      fi
      # Too late: a new round could not be recorded by the deadline (CL7-24).
      need_blocks=$(( (LEAD + ROUND_SECS) * 1000 / block_ms ))
      [ $(( head + need_blocks )) -lt "$(pj deadlineBlock)" ] \
        || { echo "head block $head plus about $(( (LEAD + ROUND_SECS) / 60 )) minutes of blocks ($need_blocks) reaches the deadline block $(pj deadlineBlock): a new round cannot be recorded in time; the market(s) can only void" >&2; exit 1; }
      fresh=$(( $(date +%s) + LEAD ))
      [ "$plan_start" -le "$fresh" ] || fresh=$plan_start
      [ "$fresh" -lt $(( recheck_end - 300 )) ] \
        || { echo "a new window $(utc "$fresh") is past the recheck days ($first_day to $last_day): the market(s) can only void" >&2; exit 1; }
      window_start=$fresh
      if [ -e "$state/window-start" ]; then
        echo "   the saved window $(utc "$(cat "$state/window-start")") holds no draw (stopped before its draw, or its draw was released): a new window"
      fi
    else
      # Every window this state dir used, newest first: the one the held draw was announced for.
      for w in $(cat "$state/window-start" 2>/dev/null) $(tac "$state/windows" 2>/dev/null); do
        book_held "$w" || continue
        if [ -n "$held_same" ]; then window_start=$w; resumed_round=1; break; fi
      done
      [ -n "$window_start" ] || { echo "the round book holds a draw announced for a window this state dir did not record: restore its window-start; refused" >&2; exit 1; }
    fi
    # The draw commits to its pool: a resumed round keeps its own, and the plan must agree with it.
    if [ -e "$state/pool.json" ] && ! python3 -I -c 'import json,sys; a,b=(sorted(m["address"].lower() for m in json.load(open(f))) for f in sys.argv[1:]); sys.exit(a != b)' "$state/pool.json" "$work/plan-pool.json"; then
      echo "the plan's pool differs from the pool of the round in $state (its draw is committed to it): refused" >&2
      exit 1
    fi
    echo "2. the recheck round of $(id recheck_metric) $(id subject): window $(utc "$window_start")$( [ -n "$resumed_round" ] && echo " (resuming the draw the book holds)" || echo " (a new draw)"), pool $(pj pool) members (left out $(pj excluded)); the coordinator draws $min_observers; draws so far $draws"
    echo "   coordinator $COORD (key: BASE_SEPOLIA_PRIVATE_KEY in $ENV_FILE): announceDraw, fixBeacon, postReceipt per reveal, recordObservation, <= $GAS_COORDINATOR gas"
    echo "   receipt key $receipt (signs receipts, sends nothing)"
    for a in "${members[@]}"; do echo "   observer $a (key: ${key_of[$a]}): one commit if drawn, <= $GAS_COMMIT gas"; done
    echo "   results store $rstore, its pin in $export_dir/observations/pins, state $state"
    if [ -z "$yes" ]; then
      echo "   (dry run: no round started)"
    else
      [ -f "$ENV_FILE" ] || { echo "$ENV_FILE is missing" >&2; exit 1; }
      need_key "$receipt"
      mkdir -p -m 700 "$state/spool" "$state/observers" "$state/logs"
      logs=$state/logs/$(date -u +%Y%m%dT%H%M%SZ)
      mkdir -m 700 "$logs"
      ln -sfn "$(basename "$logs")" "$state/logs/latest"
      [ -e "$state/pool.json" ] || cp "$work/plan-pool.json" "$state/pool.json"
      echo "$window_start" >"$state/window-start"
      grep -qx "$window_start" "$state/windows" 2>/dev/null || echo "$window_start" >>"$state/windows"
      new_store=()
      [ -e "$rstore" ] || new_store=(--new-store)
      feed=(--record-date "$date" --window-start "$window_start")
      chain=(--rpc "$RPC" --registry "$REG_C" --cast "$CAST")
      opids=()
      : >"$state/observers.pids"
      for a in "${members[@]}"; do
        n=${name_of[$a]}
        "$bin/level2-observer" --key-file "${key_of[$a]}" --pool "$state/pool.json" --spool "$state/spool" \
          --state "$state/observers/$n" --receipt-wait 3600 "${feed[@]}" "${chain[@]}" \
          >"$logs/observer-$n.log" 2>&1 &
        pids+=($!)
        opids+=("$!:$n")
        echo "$!" >>"$state/observers.pids"
      done
      echo "   ${#members[@]} observers started; the window opens in $(( window_start - $(date +%s) )) s; logs $logs"
      key=$(python3 -I - "$ENV_FILE" <<'PY'
import sys
for line in open(sys.argv[1]).read().splitlines():
    k, sep, v = line.partition("=")
    if sep and k.strip() == "BASE_SEPOLIA_PRIVATE_KEY":
        print(v.strip().strip('"').strip("'"))
        break
PY
)
      [ -n "$key" ] || { echo "BASE_SEPOLIA_PRIVATE_KEY is missing from $ENV_FILE" >&2; exit 1; }
      # In the background so a signal stops it at once (the trap stops every process started here).
      ONYX_L2_COORDINATOR_KEY=$key "$bin/level2-coordinator" "${chain[@]}" --coordinator "$COORD" \
        --reporter "$REPORTER" --book "$state/book.json" --store "$rstore" "${new_store[@]}" \
        --pool "$state/pool.json" --spool "$state/spool" --receipt-key "$receipt" "${feed[@]}" \
        >"$logs/round.json" 2>"$logs/coordinator.log" &
      cpid=$!
      unset key
      pids+=("$cpid")
      echo "$cpid" >"$state/coordinator.pid"
      rc=0
      wait "$cpid" || rc=$?
      rm -f "$state/coordinator.pid"
      if [ $rc != 0 ]; then
        echo "the coordinator stopped (exit $rc):" >&2
        tail -5 "$logs/coordinator.log" >&2
        # CL7-25 (a): keep the window only while the book holds a draw announced for it.
        if book_held "$window_start" && [ -n "$held_same" ]; then
          echo "the book holds the draw announced for $(utc "$window_start"): run again with the same state dir ($state) to finish the round on that draw" >&2
        else
          # Released (it never landed) or never made: the next draw takes a new window and pool.
          rm -f "$state/window-start" "$state/pool.json"
          echo "the book holds no draw for $(utc "$window_start"): the next run picks a new window (state dir $state)" >&2
        fi
        write_pin
        exit 1
      fi
      cat "$logs/round.json"
      valued=""
      if python3 -I -c 'import json,sys; r=json.load(open(sys.argv[1])); sys.exit(0 if r["outcome"].startswith("Value(") and r["level2_claim"] is True else 1)' "$logs/round.json"; then
        valued=1
      fi
      receipted=0 undrawn=0 failed=0 resumed=0
      for e in "${opids[@]}"; do
        pid=${e%%:*} n=${e#*:}
        if wait "$pid"; then
          if grep -q "receipt on chain" "$logs/observer-$n.log"; then receipted=$((receipted + 1))
          elif grep -q "not drawn" "$logs/observer-$n.log"; then undrawn=$((undrawn + 1))
          else failed=$((failed + 1)); fi
        elif grep -q "resuming the saved commit and reveal" "$logs/observer-$n.log" \
          && grep -q "past reveal close; nothing sent" "$logs/observer-$n.log"; then
          resumed=$((resumed + 1)) # revealed in an earlier run of this round (as level2-sepolia.sh, NL25)
        else
          failed=$((failed + 1)); echo "observer $n failed:"; tail -3 "$logs/observer-$n.log"
        fi
      done
      pids=()
      rm -f "$state/window-start" "$state/observers.pids"
      echo "observers: $receipted receipted on chain, $resumed revealed in an earlier run of this round, $undrawn not drawn, $failed failed"
      write_pin
      if [ -z "$valued" ]; then
        # The next draw rebuilds its pool from the plan (CL7-25).
        rm -f "$state/pool.json"
        recheck_state "$served_first"
        echo "the round recorded no value: $draws of the subject's 3 draws used (THREATS CL7-6); run again for a new draw while the recheck days and the deadline allow" >&2
        exit 1
      fi
    fi
  fi
fi

########## 3. resolve ##########
if [ "$st" = run ]; then
  recheck_state "$served_first"
  if [ -z "$recorded" ] || ! store_ok; then
    echo "3. resolve(market, recheckRound, recheckValue) for market(s) $(pj served) from $sender_addr (key: $sender): after the round, <= $GAS_RESOLVE gas each"
    [ -z "$recorded" ] || echo "   the recheck is recorded on chain, but its store cannot be read: $store_err" >&2
    if [ -n "$yes" ]; then
      echo "   resolve was not sent" >&2
      rc_all=1
    fi
  else
    dry=()
    [ -n "$yes" ] || dry=(--dry-run)
    r=$(lm recheck-resolve "${chain_flags[@]}" "${margin[@]}" --store "$rstore" --key-file "$sender" "${dry[@]}") || exit 1
    python3 -I - "$r" "$sender" <<'PY' || rc_all=1
import json, sys
r, sender = json.loads(sys.argv[1]), sys.argv[2]
print(f"3. resolve from {r['from']} (key: {sender}) with the recheck round {r['round']}, value {r['value']}:")
for m in r["markets"]:
    if "error" in m:
        print(f"   market {m['market']}: REFUSED: {m['error']}")
    elif r["dryRun"]:
        print(f"   market {m['market']}: would send resolve({m['market']}, {r['round']}, {r['value']}): estimate {m['gas']} gas")
    else:
        print(f"   market {m['market']}: resolved")
for i in r.get("leftToVoid", []):
    print(f"   market {i}: left to void (it closes too late for this round)")
if not r["markets"]:
    print("   no open market the round serves: nothing to resolve")
sys.exit(1 if any("error" in m for m in r["markets"]) else 0)
PY
  fi
fi

########## 4. withdraw and payTreasury ##########
settled_any=0
for m in $(pj markets | tr -d '[],'); do
  s=$(castw call "$C" "$MARKET_SIG" "$m" --json | tuple | python3 -I -c 'import json,sys; print(json.load(sys.stdin)[8])') \
    || { echo "reading market $m failed" >&2; exit 1; }
  [ "$s" = 1 ] || settled_any=$((settled_any + 1))
done
if [ "$settled_any" = 0 ] && [ -z "$yes" ]; then
  echo "4. withdraw(market) from each held key the contract owes after resolve (<= $GAS_WITHDRAW gas each), then payTreasury() from $sender_addr (key: $sender, <= $GAS_PAY_TREASURY gas)"
else
  dry=()
  [ -n "$yes" ] || dry=(--dry-run)
  r=$(lm recheck-payout "${chain_flags[@]}" "${payout_keys[@]}" --treasury-key-file "$sender" "${dry[@]}") || exit 1
  python3 -I - "$r" <<'PY' || rc_all=1
import json, sys
r = json.loads(sys.argv[1])
dry = r["dryRun"]
print("4. withdraw and payTreasury" + (" (dry run)" if dry else "") + ":")
for m in r["markets"]:
    w = {0: "-", 1: "Yes", 2: "No"}.get(int(m["winner"]), m["winner"])
    print(f"   market {m['market']}: {m['state']}, winner {w}")
bad = False
for w in r["withdrawals"]:
    tonyx = int(w.get("owed", 0)) / 1e18
    if w.get("paidBefore"):
        print(f"   market {w['market']}: {w['who']} was paid before: nothing sent")
    elif "error" in w:
        bad = True
        print(f"   market {w['market']}: withdraw from {w['who']} ({tonyx:g} tONYX) FAILED: {w['error']}")
    elif dry:
        print(f"   market {w['market']}: would send withdraw({w['market']}) from {w['who']} ({tonyx:g} tONYX): estimate {w['gas']} gas")
    else:
        print(f"   market {w['market']}: {w['who']} withdrew {tonyx:g} tONYX")
t = r["treasury"]
owed = int(t["owed"]) / 1e18
if "error" in t:
    bad = True
    print(f"   payTreasury ({owed:g} tONYX) from {t.get('from')} FAILED: {t['error']}")
elif int(t["owed"]) == 0:
    print("   the treasury is owed nothing: no payTreasury")
elif dry:
    print(f"   would send payTreasury() from {t['from']} ({owed:g} tONYX to the treasury): estimate {t['gas']} gas")
else:
    print(f"   payTreasury(): {owed:g} tONYX to the treasury")
sys.exit(1 if bad else 0)
PY
fi

if [ -n "$yes" ]; then
  write_pin
  n1=$(nonces "${actors[@]}")
  b1=$(balances "${actors[@]}")
  python3 -I -c '
import sys
n0, n1, b0, b1 = (list(map(int, a.split())) for a in sys.argv[1:5])
print(f"chain: {sum(n1) - sum(n0)} transactions from the coordinator, the sender, the pool and the held bettors; {(sum(b0) - sum(b1)) / 1e18:.9f} ETH")
' "$n0" "$n1" "$b0" "$b1"
  for m in $(pj markets | tr -d '[],'); do
    castw call "$C" "$MARKET_SIG" "$m" --json | tuple | python3 -I -c '
import json, sys
m = json.load(sys.stdin)
state = {1: "open", 2: "resolved", 3: "void"}.get(m[8], m[8])
print(f"market {sys.argv[1]}: {state}" + (f", recheck value {m[14]}, " + ("Yes" if m[9] == 1 else "No") + " won" if m[8] == 2 else ""))
' "$m"
  done
fi
[ "$rc_all" = 0 ] || { echo "RECHECK RUN: some step failed or was skipped (above); run again to retry it" >&2; exit 1; }
if [ -n "$yes" ]; then echo "RECHECK RUN DONE ($net)"; else echo "DRY RUN DONE: nothing sent"; fi
