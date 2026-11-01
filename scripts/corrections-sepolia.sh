#!/usr/bin/env bash
# CL7 (ADR-0057, docs/ops/runbooks/corrections-base-sepolia.md): correction markets on Base Sepolia,
# step by step. LEAD ONLY, test network only. The deployer key is BASE_SEPOLIA_PRIVATE_KEY from
# .env and the registry owner's is .onyx-testnet/keys/registry-owner.key (both passed to forge in
# the environment, never on a command line, never printed). The deployer owns the new contract
# (it pairs metrics) and is its treasury; the registry's owner registers the recheck metric.
# State (the address and deploy block, public on chain) goes to .onyx-testnet/corrections-sepolia.json.
#
#   scripts/corrections-sepolia.sh plan                       dry run of the deploy (nothing sent)
#   scripts/corrections-sepolia.sh deploy --yes               deploy OnyxCorrections
#   scripts/corrections-sepolia.sh pair MANIFEST DATE [--yes] register the manifest's `.recheck`
#                                                             metric and pair it (dry run without --yes)
#   scripts/corrections-sepolia.sh status                     the contract's settings and pairs
#
# Every step can be run again: a deploy that was sent but not recorded is found by its address, and
# `pair` sends nothing for what is already registered and paired (DeployCorrections.pair).
# Env: CORRECTIONS_AFTER_DAYS (7), CORRECTIONS_SPAN_DAYS (7): the recheck placeholders.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
state=$REPO/.onyx-testnet/corrections-sepolia.json
after=${CORRECTIONS_AFTER_DAYS:-7}
span=${CORRECTIONS_SPAN_DAYS:-7}
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
  printf '%s\n' "$out" | grep -E 'Contract Address|Estimated|ONCHAIN EXECUTION|Total Paid|SUCCESS|SIMULATION' || true
}
# The first block from $2 on where $1 has code.
code_block() {
  local b=$2
  while [ "$(cast_ code "$1" --block "$b")" = 0x ]; do b=$((b + 1)); done
  echo "$b"
}
need_contract() { [ -n "$(get contract)" ] || { echo "no contract yet: run deploy first" >&2; exit 1; }; }

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
status)
  need_contract
  a=$(get contract)
  echo "OnyxCorrections $a (block $(get block))"
  for f in 'registry()(address)' 'token()(address)' 'owner()(address)' 'treasury()(address)' 'openFee()(uint128)' \
    'minTradingBlocks()(uint32)' 'minRecheckBlocks()(uint32)' 'maxMarketBlocks()(uint32)' 'marketCount()(uint256)' 'liabilities()(uint256)'; do
    echo "  ${f%%(*}: $(cast_ call "$a" "$f" | awk '{print $1}')"
  done
  ;;
*)
  sed -n '8,13p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
  ;;
esac
