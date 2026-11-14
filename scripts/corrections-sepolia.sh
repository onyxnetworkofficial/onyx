#!/usr/bin/env bash
# CL7 (ADR-0057, docs/ops/runbooks/corrections-base-sepolia.md): correction markets on Base Sepolia,
# step by step. LEAD ONLY, test network only. The deployer key is BASE_SEPOLIA_PRIVATE_KEY from
# .env and the registry owner's is .onyx-testnet/keys/registry-owner.key (both passed to forge in
# the environment, never on a command line, never printed). The deployer owns the new contract
# (it pairs metrics) and is its treasury; the registry's owner registers the recheck metric.
# A market's opener and its bettors are operator test keys, never an insider (the deployer) or a
# level-2 observer (validator-9..15): the opener .onyx-testnet/keys/miner-0.key by default, a
# bettor `--key NAME` (.onyx-testnet/keys/NAME.key, or a key file's path). The deployer, the tONYX
# minter, gives each the gas and tONYX it lacks, as markets-sepolia.sh does a bettor.
# State (the address, deploy block and markets, public on chain) goes to
# .onyx-testnet/corrections-sepolia.json.
#
#   scripts/corrections-sepolia.sh plan                       dry run of the deploy (nothing sent)
#   scripts/corrections-sepolia.sh deploy --yes               deploy OnyxCorrections
#   scripts/corrections-sepolia.sh pair MANIFEST DATE [--yes] register the manifest's `.recheck`
#                                                             metric and pair it (dry run without --yes)
#   scripts/corrections-sepolia.sh open MANIFEST DATE [--close-at ISO-UTC] [--new-market] [--yes]
#                                                             open the market of DATE's value (dry run
#                                                             without --yes; DeployCorrections.openMarket)
#   scripts/corrections-sepolia.sh bet MARKET yes|no TONYX [--key NAME] [--yes]
#                                                             bet whole tONYX (dry run without --yes)
#   scripts/corrections-sepolia.sh status                     the contract's settings and markets
#
# Every step can be run again: a deploy that was sent but not recorded is found by its address,
# `pair` sends nothing for what is already registered and paired (DeployCorrections.pair), and
# `open` sends nothing when the value has a market, open or not (it records that market;
# `--new-market` opens another). `open` reads the original round (roundId, windowEnd, value) from
# the node's /v1/observations answer and checks the contract's pair and the registry's recheck
# template against the manifest's; the close is the earliest allowed or `--close-at`, the deadline
# noon UTC the day after the last recheck day (scripts/corrections-blocks.py), counted at the
# block time measured over the last day. `bet` refuses a closed market and an insider.
# Env: CORRECTIONS_AFTER_DAYS (7), CORRECTIONS_SPAN_DAYS (7): the recheck placeholders;
# CORRECTIONS_OPENER_KEY_FILE (the opener's key file); CORRECTIONS_CLOSE_MARGIN_BLOCKS (9000: 30
# minutes for the build and the sends to land); ONYX_API_URL (https://onyx.cryptagraph.com).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
state=$REPO/.onyx-testnet/corrections-sepolia.json
after=${CORRECTIONS_AFTER_DAYS:-7}
span=${CORRECTIONS_SPAN_DAYS:-7}
opener_file=${CORRECTIONS_OPENER_KEY_FILE:-$REPO/.onyx-testnet/keys/miner-0.key}
margin=${CORRECTIONS_CLOSE_MARGIN_BLOCKS:-9000}
api=${ONYX_API_URL:-https://onyx.cryptagraph.com}
# The live registry and the markets' tONYX (docs/ops/deployment.md).
registry=0x497389e04f7d58e07a79ac8d395c3927aecbdf2b
tonyx=0x1cb5f5816812991ac3b52c63c3fcadcc9f544579
deployer=0x80c91f45b8353ff5c2b1e4c127a2a21746b37972
chain_id=84532
public_rpc=https://sepolia.base.org

env_value() {
  python3 -I - "$REPO/.env" "$1" <<'PY'
import sys
path, name = sys.argv[1], sys.argv[2]
try:
    lines = open(path).read().splitlines()
except FileNotFoundError:
    sys.exit(0)
for line in lines:
    k, sep, v = line.partition("=")
    if sep and k.strip() == name:
        print(v.strip().strip('"').strip("'"))
        break
PY
}
rpc=$(env_value BASE_SEPOLIA_RPC_URL)
rpc=${rpc:-$public_rpc}
# forge and cast read the endpoint from the environment, so it is on no command line (it can carry
# an API key), and their output is scrubbed of it. forge script reads FOUNDRY_ETH_RPC_URL.
export ETH_RPC_URL=$rpc FOUNDRY_ETH_RPC_URL=$rpc
scrub() { RPC_URL=$rpc python3 -I -c 'import os,sys; u=os.environ["RPC_URL"]; [sys.stdout.write(l.replace(u, "<rpc>")) for l in sys.stdin]'; }
cast_() { "$FOUNDRY_BIN/cast" "$@" 2>&1 | scrub; return "${PIPESTATUS[0]}"; }
[ "$(cast_ chain-id)" = "$chain_id" ] || { echo "the RPC is not Base Sepolia" >&2; exit 1; }

# The state file: a corrupt one stops everything (it is never read as "nothing done yet").
get() {
  [ -f "$state" ] || return 0
  python3 -I -c 'import json,sys; v=json.load(open(sys.argv[1])); print(v.get(sys.argv[2], ""))' "$state" "$1" \
    || { echo "$state is not valid JSON: fix it by hand" >&2; exit 1; }
}
put() {
  mkdir -p "$(dirname "$state")"
  python3 -I - "$state" "$1" "$2" <<'PY'
import json, os, sys
p, k, v = sys.argv[1:]
d = json.load(open(p)) if os.path.exists(p) else {}
d[k] = v
tmp = p + ".tmp"
with open(tmp, "w") as f:
    json.dump(d, f, indent=2, sort_keys=True)
    f.write("\n")
os.replace(tmp, p)
PY
}
deployer_key() {
  local k
  k=$(env_value BASE_SEPOLIA_PRIVATE_KEY)
  [ -n "$k" ] || { echo "BASE_SEPOLIA_PRIVATE_KEY is missing from .env" >&2; exit 1; }
  printf '%s' "$k"
}
# A key file's hex as forge reads it (0x-prefixed); the key is never printed.
hex_key() {
  [ -f "$1" ] || { echo "missing key file $1" >&2; exit 1; }
  python3 -I -c 'import sys; k=open(sys.argv[1]).read().strip(); print(k if k.startswith("0x") else "0x" + k)' "$1"
}
# Run a tool; on failure show its (scrubbed) output and stop.
run() {
  local out
  if out=$("$@" 2>&1); then
    printf '%s\n' "$out" | scrub
  else
    printf '%s\n' "$out" | scrub >&2
    echo "failed: $1 $2" >&2
    exit 1
  fi
}
forge_script() {
  # $1 script:contract; the key and parameters travel in the environment only. forge's broadcast
  # record stays in contracts/broadcast (ignored by git).
  local s=$1 out
  shift
  out=$(run bash -c 'cd "$1/contracts" && shift && exec "$@"' _ "$REPO" "$FOUNDRY_BIN/forge" script "$s" "$@")
  printf '%s\n' "$out" | grep -E 'corrections:|Contract Address|Estimated|ONCHAIN EXECUTION|Total Paid|SUCCESS|SIMULATION' || true
}
# The first block from $2 on where $1 has code.
code_block() {
  local b=$2
  while [ "$(cast_ code "$1" --block "$b")" = 0x ]; do b=$((b + 1)); done
  echo "$b"
}
need_contract() { [ -n "$(get contract)" ] || { echo "no contract yet: run deploy first" >&2; exit 1; }; }
# Level-2 observers never bet (THREATS NL6-4); the registry's owner and the receipt key are not
# test bettors. forge refuses the insiders (owner, treasury, coordinator) by address.
refuse_key() {
  case $(basename "$1") in
  validator-9.key | validator-1[0-5].key | registry-owner.key | l2-receipt.key)
    echo "refused: $1 is not an operator test key" >&2
    exit 1
    ;;
  esac
}
# `OnyxCorrections.market(id)`, the `Market` struct (as onyx_level2::corrections reads it).
market_sig='market(uint256)((bytes32,bytes32,bytes32,bytes32,address,uint64,uint64,uint64,uint8,uint8,uint8,uint32,uint32,int128,int128,uint128,uint128,uint128,uint128,uint128,uint128,uint128[2],uint32[2]))'
# The head block, its time and the block time in ms, measured over the last 432,000 blocks (a
# day at 0.2 s): sets head, head_time and block_ms.
clock() {
  local back=432000 then
  head=$(cast_ block-number)
  head_time=$(cast_ block "$head" --field timestamp)
  then=$(cast_ block $((head - back)) --field timestamp)
  block_ms=$(python3 -I -c 'import sys; a, b, n = map(int, sys.argv[1:]); print(round((a - b) * 1000 / n))' "$head_time" "$then" "$back")
}
# Record market $1 in the state: $2.. the metric, subject, record date and round id; stdin the
# market as `cast call --json` decodes it.
put_market() {
  python3 -I -c '
import json, os, sys
p, i, metric, subject, date, round_id = sys.argv[1:7]
m = json.load(sys.stdin)[0]
d = json.load(open(p)) if os.path.exists(p) else {}
d.setdefault("markets", {})[i] = {"metric": metric, "subject": subject, "recordDate": date,
    "roundId": round_id, "windowEnd": m[5], "value": str(m[13]), "opener": m[4].lower(),
    "closeBlock": m[6], "resolveBlock": m[7]}
tmp = p + ".tmp"
with open(tmp, "w") as f:
    json.dump(d, f, indent=2, sort_keys=True)
    f.write("\n")
os.replace(tmp, p)
' "$state" "$@"
}

cmd=${1:-}
shift || true
case "$cmd" in
plan | deploy)
  [ -z "$(get contract)" ] || { echo "already deployed: $(get contract) (block $(get block))" >&2; exit 1; }
  [ "$cmd" = plan ] || [ "${1:-}" = --yes ] || { echo "deploy sends from the deployer: run plan first, then deploy --yes" >&2; exit 2; }
  pending=$(get pending_contract)
  if [ -n "$pending" ] && [ "$(cast_ code "$pending")" != 0x ]; then
    put contract "$pending"
    put block "$(code_block "$pending" "$(get pending_block)")"
    echo "found the earlier deploy: $pending (block $(get block))"
    exit 0
  fi
  args=()
  if [ "$cmd" = deploy ]; then
    args=(--broadcast --slow)
    put pending_contract "$(cast_ compute-address "$deployer" --nonce "$(cast_ nonce "$deployer")" | grep -oE '0x[0-9a-fA-F]{40}' | tr '[:upper:]' '[:lower:]')"
    put pending_block "$(cast_ block-number)"
  fi
  key=$(deployer_key)
  BASE_SEPOLIA_PRIVATE_KEY=$key ONYX_REGISTRY=$registry ONYX_TONYX=$tonyx \
    forge_script script/DeployCorrections.s.sol:DeployCorrections "${args[@]}"
  unset key
  [ "$cmd" = deploy ] || { echo "PLAN ONLY: nothing sent"; exit 0; }
  a=$(get pending_contract)
  [ "$(cast_ code "$a")" != 0x ] || { echo "no code at the expected address $a" >&2; exit 1; }
  put contract "$a"
  put block "$(code_block "$a" "$(get pending_block)")"
  [ "$(cast_ call "$a" 'registry()(address)' | tr '[:upper:]' '[:lower:]')" = "$registry" ] || { echo "the deploy names another registry" >&2; exit 1; }
  [ "$(cast_ call "$a" 'token()(address)' | tr '[:upper:]' '[:lower:]')" = "$tonyx" ] || { echo "the deploy names another token" >&2; exit 1; }
  echo "OnyxCorrections $a at block $(get block) ($(cast_ codesize "$a") bytes); owner and treasury $deployer"
  ;;
pair)
  need_contract
  manifest=${1:?usage: pair MANIFEST DATE [--yes]}
  date=${2:?usage: pair MANIFEST DATE [--yes]}
  ids=$(cargo run --locked -q -p onyx-level2 --example onyx-source -- recheck "$manifest" \
    --after-days "$after" --span-days "$span" --date "$date")
  id() { awk -v k="$1" '$1 == k {print $2}' <<<"$ids"; }
  for k in original_metric_key recheck_metric_key recheck_template_id tolerance; do
    [ -n "$(id "$k")" ] || { echo "onyx-source recheck printed no $k" >&2; exit 1; }
  done
  args=()
  [ "${3:-}" = --yes ] && args=(--broadcast --slow)
  key=$(deployer_key)
  # The registry's owner registers the recheck metric (never the coordinator, REG-R1-F3); its test
  # key is the file scripts/level2-sepolia.sh uses. forge checks it is the owner's before sending.
  owner_key=$(hex_key "$REPO/.onyx-testnet/keys/registry-owner.key")
  BASE_SEPOLIA_PRIVATE_KEY=$key ONYX_REGISTRY_OWNER_KEY=$owner_key ONYX_REGISTRY=$registry ONYX_CORRECTIONS=$(get contract) \
    ONYX_METRIC_KEY=$(id original_metric_key) ONYX_RECHECK_METRIC_KEY=$(id recheck_metric_key) \
    ONYX_RECHECK_TEMPLATE=$(id recheck_template_id) ONYX_RECHECK_TOLERANCE=$(id tolerance) \
    forge_script script/DeployCorrections.s.sol:DeployCorrections --sig 'pairRecheck()' "${args[@]}"
  unset key owner_key
  [ "${3:-}" = --yes ] || { echo "PLAN ONLY: nothing sent"; exit 0; }
  echo "registry: $(id recheck_metric_key) -> $(cast_ call "$registry" 'metrics(bytes32)(uint8,bytes32,uint64)' "$(id recheck_metric_key)" | tr '\n' ' ')"
  echo "pair: $(cast_ call "$(get contract)" 'pairs(bytes32)(bytes32,uint128)' "$(id original_metric_key)" | tr '\n' ' ')"
  ;;
open)
  need_contract
  usage="usage: open MANIFEST DATE [--close-at ISO-UTC] [--new-market] [--yes]"
  manifest=${1:?$usage}
  date=${2:?$usage}
  shift 2
  yes="" close_at=() new_market=false
  while [ $# -gt 0 ]; do
    case $1 in
    --yes) yes=--yes ;;
    --new-market) new_market=true ;;
    --close-at) close_at=("${2:?$usage}"); shift ;;
    *) echo "$usage" >&2; exit 2 ;;
    esac
    shift
  done
  c=$(get contract)
  refuse_key "$opener_file"
  ids=$(cargo run --locked -q -p onyx-level2 --example onyx-source -- recheck "$manifest" \
    --after-days "$after" --span-days "$span" --date "$date")
  id() { awk -v k="$1" '$1 == k {print $2}' <<<"$ids"; }
  for k in original_metric original_template_id original_metric_key recheck_metric_key recheck_template_id \
    subject subject_key recheck_days; do
    [ -n "$(id "$k")" ] || { echo "onyx-source recheck printed no $k" >&2; exit 1; }
  done
  first_day=$(id recheck_days)
  last_day=$(awk '$1 == "recheck_days" {print $4}' <<<"$ids")
  # The original round as the node serves it. forge checks it again on chain: the round and
  # value must hash, with the recorded spec, to the registry's result.
  # The query as the node writes it (`:` unescaped): any other spelling is a 308 to that one.
  q=$(python3 -I -c 'import sys, urllib.parse as u; print(u.quote(sys.argv[1], safe=":"))' "$(id subject)")
  obs=$(curl -fsS -m 60 -L --max-redirs 1 -A onyx-dev "$api/v1/observations/$(id original_metric)?subject=$q") \
    || { echo "the node has no settled round of $(id subject)" >&2; exit 1; }
  round=$(python3 -I -c '
import json, re, sys
metric, subject, template = sys.argv[1:4]
try:
    d = json.load(sys.stdin)
except ValueError:
    sys.exit("the node answered no JSON")
def want(k, v):
    if str(d.get(k)).lower() != v.lower():
        sys.exit(f"the node answers {k} {d.get(k)!r}, want {v!r}")
want("metric", metric); want("subjectText", subject); want("templateId", template)
want("proofLevel", "2")
a = bytes.fromhex(d["answer"].removeprefix("0x"))
if len(a) != 17 or a[0] != 1:
    sys.exit("the answer is not a number")
if not re.fullmatch(r"0x[0-9a-fA-F]{64}", d["roundId"]) or not isinstance(d["windowEnd"], int):
    sys.exit("the answer has no round id or window end")
print(d["roundId"], d["windowEnd"], int.from_bytes(a[1:], "little", signed=True), d["value"])
' "$(id original_metric)" "$(id subject)" "$(id original_template_id)" <<<"$obs") || exit 1
  read -r round_id window_end value value_text <<<"$round"
  clock
  blocks=$(python3 -I "$REPO/scripts/corrections-blocks.py" "$head" "$head_time" "$block_ms" \
    "$(cast_ call "$c" 'minTradingBlocks()(uint32)' | awk '{print $1}')" \
    "$(cast_ call "$c" 'minRecheckBlocks()(uint32)' | awk '{print $1}')" "$margin" "$last_day" "${close_at[@]}")
  b() { awk -v k="$1" '$1 == k {print $2}' <<<"$blocks"; }
  echo "value: $(id original_metric) $(id subject) = $value_text (on chain $value; round $round_id, windowEnd $window_end)"
  echo "head block $head ($(python3 -I -c 'import datetime as d, sys; print(d.datetime.fromtimestamp(int(sys.argv[1]), d.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"))' "$head_time"), ${block_ms} ms blocks); recheck days $first_day to $last_day"
  awk '$1 == "close_block" || $1 == "resolve_block" {b = $2} $1 == "close_time" {print "close block " b " (~" $3 ")"} $1 == "resolve_time" {print "deadline block " b " (~" $3 ")"}' <<<"$blocks"
  args=()
  key=$(env_value BASE_SEPOLIA_PRIVATE_KEY)
  if [ "$yes" = --yes ]; then
    args=(--broadcast --slow)
    # Sending may need the minter's key (a dry run without it simulates the minter).
    key=$(deployer_key)
  fi
  opener_key=$(hex_key "$opener_file")
  out=$(
    [ -z "$key" ] || export BASE_SEPOLIA_PRIVATE_KEY=$key
    ONYX_OPENER_KEY=$opener_key ONYX_CORRECTIONS=$c ONYX_METRIC_KEY=$(id original_metric_key) \
      ONYX_SUBJECT_KEY=$(id subject_key) ONYX_WINDOW_END=$window_end ONYX_ROUND_ID=$round_id \
      ONYX_VALUE=$value ONYX_CLOSE_BLOCK=$(b close_block) ONYX_RESOLVE_BLOCK=$(b resolve_block) \
      ONYX_RECHECK_END=$(b recheck_end) ONYX_BLOCK_MS=$block_ms \
      ONYX_RECHECK_METRIC_KEY=$(id recheck_metric_key) ONYX_RECHECK_TEMPLATE=$(id recheck_template_id) \
      ONYX_NEW_MARKET=$new_market \
      forge_script script/DeployCorrections.s.sol:DeployCorrections --sig 'openMarket()' "${args[@]}"
  )
  unset key opener_key
  printf '%s\n' "$out"
  # The market's id as forge logged it (the one it opened, or the value's existing one).
  m=$(grep -oE 'corrections: market [0-9]+' <<<"$out" | head -n1 | awk '{print $3}')
  [ -n "$m" ] || { echo "forge logged no market id" >&2; exit 1; }
  [ "$yes" = --yes ] || grep -q 'exists; nothing sent' <<<"$out" || { echo "PLAN ONLY: nothing sent"; exit 0; }
  fact=$(cast_ call "$registry" 'factKey(bytes32,bytes32,uint64)(bytes32)' "$(id original_metric_key)" "$(id subject_key)" "$window_end")
  # A market opened here is the opener's (forge logged its address); an existing one, the value's.
  opener=$(grep -oE 'corrections: 0x[0-9a-fA-F]{40} holds' <<<"$out" | head -n1 | awk '{print $2}' || true)
  # The RPC may lag the send: read the market until it is the value's (a few tries).
  for try in 1 2 3 4 5 6 7 8 9 10; do
    json=$(cast_ call "$c" "$market_sig" "$m" --json) && python3 -I -c '
import json, sys
m = json.loads(sys.argv[1])[0]
fact, opener = sys.argv[2:4]
sys.exit(0 if m[2].lower() == fact.lower() and (not opener or m[4].lower() == opener.lower()) else 1)
' "$json" "$fact" "$opener" && break
    [ "$try" = 10 ] && { echo "market $m is not this value's (or not this opener's) after 10 reads: check status, then run open again" >&2; exit 1; }
    sleep 3
  done
  put_market "$m" "$(id original_metric)" "$(id subject)" "$date" "$round_id" <<<"$json"
  echo "market $m of $(id subject) recorded in $state; status shows it"
  ;;
bet)
  need_contract
  usage="usage: bet MARKET yes|no TONYX [--key NAME|FILE] [--yes]"
  market=${1:?$usage}
  side=${2:?$usage}
  whole=${3:?$usage}
  shift 3
  yes="" key_name=miner-0
  while [ $# -gt 0 ]; do
    case $1 in
    --yes) yes=--yes ;;
    --key) key_name=${2:?$usage}; shift ;;
    *) echo "$usage" >&2; exit 2 ;;
    esac
    shift
  done
  [[ $market =~ ^[1-9][0-9]{0,8}$ && $whole =~ ^[1-9][0-9]{0,8}$ ]] || { echo "$usage" >&2; exit 2; }
  case $side in yes | 1) side=1 ;; no | 2) side=2 ;; *) echo "$usage" >&2; exit 2 ;; esac
  case $key_name in
  */*) bettor_file=$key_name ;;
  *)
    [[ $key_name =~ ^[a-z0-9-]+$ ]] || { echo "--key: a key name such as miner-1, or a key file" >&2; exit 2; }
    bettor_file=$REPO/.onyx-testnet/keys/$key_name.key
    ;;
  esac
  refuse_key "$bettor_file"
  c=$(get contract)
  json=$(cast_ call "$c" "$market_sig" "$market" --json)
  python3 -I -c '
import json, sys
m = json.loads(sys.argv[1])[0]
yes, no = (int(x) / 1e18 for x in m[21])
print(f"market {sys.argv[2]}: state {m[8]} (1 open), close block {m[6]}, deadline block {m[7]}, value {m[13]}, Yes {yes:g} / No {no:g} tONYX")
' "$json" "$market"
  args=()
  key=$(env_value BASE_SEPOLIA_PRIVATE_KEY)
  if [ "$yes" = --yes ]; then
    args=(--broadcast --slow)
    key=$(deployer_key)
  fi
  bettor_key=$(hex_key "$bettor_file")
  (
    [ -z "$key" ] || export BASE_SEPOLIA_PRIVATE_KEY=$key
    ONYX_BETTOR_KEY=$bettor_key ONYX_CORRECTIONS=$c ONYX_MARKET_ID=$market ONYX_SIDE=$side \
      ONYX_AMOUNT=${whole}000000000000000000 \
      forge_script script/DeployCorrections.s.sol:DeployCorrections --sig 'placeBet()' "${args[@]}"
  )
  unset key bettor_key
  [ "$yes" = --yes ] || { echo "PLAN ONLY: nothing sent"; exit 0; }
  echo "market $market after the bet (the RPC may lag a few seconds): $(cast_ call "$c" "$market_sig" "$market" --json | python3 -I -c 'import json, sys; m = json.load(sys.stdin)[0]; print("Yes %g / No %g tONYX, %d bettors" % (int(m[21][0]) / 1e18, int(m[21][1]) / 1e18, m[11]))')"
  ;;
status)
  need_contract
  a=$(get contract)
  echo "OnyxCorrections $a (block $(get block))"
  for f in 'registry()(address)' 'token()(address)' 'owner()(address)' 'treasury()(address)' 'openFee()(uint128)' \
    'minTradingBlocks()(uint32)' 'minRecheckBlocks()(uint32)' 'maxMarketBlocks()(uint32)' 'marketCount()(uint256)' 'liabilities()(uint256)'; do
    echo "  ${f%%(*}: $(cast_ call "$a" "$f" | awk '{print $1}')"
  done
  n=$(cast_ call "$a" 'marketCount()(uint256)' | awk '{print $1}')
  [ "$n" -gt 0 ] || exit 0
  clock
  # Every market on the contract (the first 50), with the metric and subject of those opened here.
  for ((i = 1; i <= n && i <= 50; i++)); do
    cast_ call "$a" "$market_sig" "$i" --json | python3 -I -c '
import datetime, json, sys
p, i = sys.argv[1:3]
head, head_time, block_ms = map(int, sys.argv[3:6])
m = json.load(sys.stdin)[0]
try:
    known = json.load(open(p)).get("markets", {}).get(i)
except (OSError, ValueError):
    known = None
def at(b):
    t = head_time + (int(b) - head) * block_ms // 1000
    return datetime.datetime.fromtimestamp(t, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%MZ")
state = {1: "open", 2: "resolved", 3: "void"}.get(m[8], f"state {m[8]}")
what = " ".join((known["metric"], known["subject"])) if known else "fact " + m[2]
yes, no = (int(x) / 1e18 for x in m[21])
line = (f"  market {i}: {state}, {what} = {m[13]}, opener {m[4]}, close block {m[6]} (~{at(m[6])}),"
        f" deadline block {m[7]} (~{at(m[7])}), Yes {yes:g} / No {no:g} tONYX, {m[11]} bettors")
if m[8] == 2:
    line += f"; recheck {m[14]}: " + ("Yes" if m[9] == 1 else "No") + " won"
elif m[8] == 3:
    line += "; void: " + ("the winning side had no bet" if m[10] == 1 else "no valid recheck")
print(line)
' "$state" "$i" "$head" "$head_time" "$block_ms"
  done
  ;;
*)
  sed -n '14,23p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
  ;;
esac
