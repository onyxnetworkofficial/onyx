#!/usr/bin/env bash
# ROADMAP S15 (ADR-0060, docs/ops/runbooks/deploy-storage.md): the storage network on Base Sepolia,
# step by step. LEAD ONLY, test network only. The deployer key is BASE_SEPOLIA_PRIVATE_KEY from
# .env (passed to forge in the environment, never on a command line, never printed); every other
# key is a fresh test key in .onyx-testnet/keys (0600, never printed). State (addresses, the deploy
# block, node and deal ids: all public on chain) goes to .onyx-testnet/storage-sepolia.json.
#
#   scripts/storage-sepolia.sh plan                 dry run of the contract deploy (nothing sent)
#   scripts/storage-sepolia.sh deploy --yes         deploy OnyxStorage
#   scripts/storage-sepolia.sh keys                 create the node and payer test keys (if missing)
#   scripts/storage-sepolia.sh fund --yes           top up test ETH and tONYX from the deployer
#   scripts/storage-sepolia.sh register             bond, register and activate each node
#   scripts/storage-sepolia.sh install              each node as onyx-storage@<name> on the web host
#   scripts/storage-sepolia.sh deal DIR [DIR ...]   pay for 3 copies of each anchored dataset
#   scripts/storage-sepolia.sh status               every node, this epoch's challenges, the books
#   scripts/storage-sepolia.sh close                close every recorded deal that has ended (the
#                                                   unpaid deposit goes back to the payer) and
#                                                   withdraw each node's earnings
#
# Every step can be run again: a deploy that was sent is found by its address, a node or deal
# already on chain is never registered or paid twice (review STORAGE-R3-3), funding tops up to a
# target. `install` uses the onyx-node that scripts/deploy-api.sh put on the host: deploy the API
# from the same commit first.
# Env: ONYX_STORAGE_BIN_DIR (onyx-storage and onyx-publish, default target/release),
# ONYX_SITE_HOST (onyx-web), ONYX_SITE_DOMAIN (onyx.cryptagraph.com), STORAGE_NODES (5),
# STORAGE_EPOCH_BLOCKS (7200: 24 minutes at Base Sepolia's 0.2 s blocks, THREATS STORAGE-S15-5),
# STORAGE_DEAL_EPOCHS (42: about 17 hours at 7,200-block epochs),
# STORAGE_CAPACITY_GIB (6 per node: five nodes fit the host's disk), STORAGE_FUND_NODE_WEI and
# STORAGE_FUND_PAYER_WEI (the test ETH `fund` tops each node and the payer up to: 120000000000000
# and 30000000000000; 0 sends no ETH, for a run on what the keys already hold).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
bin=${ONYX_STORAGE_BIN_DIR:-${CARGO_TARGET_DIR:-$REPO/target}/release}
host=${ONYX_SITE_HOST:-onyx-web}
domain=${ONYX_SITE_DOMAIN:-onyx.cryptagraph.com}
nodes=${STORAGE_NODES:-5}
epoch_blocks=${STORAGE_EPOCH_BLOCKS:-7200}
deal_epochs=${STORAGE_DEAL_EPOCHS:-42}
capacity=${STORAGE_CAPACITY_GIB:-6}
fund_node_wei=${STORAGE_FUND_NODE_WEI:-120000000000000}
fund_payer_wei=${STORAGE_FUND_PAYER_WEI:-30000000000000}
case "$fund_node_wei$fund_payer_wei" in *[!0-9]*) echo "STORAGE_FUND_*_WEI must be whole numbers" >&2; exit 2 ;; esac
keys=$REPO/.onyx-testnet/keys
state=$REPO/.onyx-testnet/storage-sepolia.json
book=0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5
tonyx=0x1cb5f5816812991ac3b52c63c3fcadcc9f544579
# The deployer's address: also the treasury, as for the markets on this test network
# (docs/ops/deployment.md; a multisig before mainnet, ROADMAP H1).
deployer=0x80c91f45b8353ff5c2b1e4c127a2a21746b37972
chain_id=84532
# The nodes and the onyx-storage tool use the public endpoint (no key in its URL).
public_rpc=https://sepolia.base.org
[ "$nodes" -ge 3 ] && [ "$nodes" -le 5 ] || { echo "STORAGE_NODES must be 3..5 (the Caddy routes are n1..n5)" >&2; exit 2; }
case "$host" in -*) echo "bad ONYX_SITE_HOST" >&2; exit 2 ;; esac

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
# an API key; review STORAGE-R3-6), and their output is scrubbed of it.
# forge script reads only FOUNDRY_ETH_RPC_URL (foundry 1.8: no ETH_RPC_URL fallback for --rpc-url).
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
names() { for i in $(seq "$nodes"); do echo "n$i"; done; }
addr() { "$bin/onyx-publish" address --key "$keys/storage-$1.key"; }
common() { echo --rpc "$public_rpc" --contract "$(get contract)" --from-block "$(get block)"; }
need_contract() { [ -n "$(get contract)" ] || { echo "no contract yet: run deploy first" >&2; exit 1; }; }
deployer_key() {
  local k
  k=$(env_value BASE_SEPOLIA_PRIVATE_KEY)
  [ -n "$k" ] || { echo "BASE_SEPOLIA_PRIVATE_KEY is missing from .env" >&2; exit 1; }
  printf '%s' "$k"
}
# Run a tool; on failure show its (scrubbed) output and stop (review STORAGE-R3-3).
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
  printf '%s\n' "$out" | grep -E 'Contract Address|Estimated|ONCHAIN EXECUTION|Total Paid|SUCCESS' || true
}
# A chain read that failed stops the script with its reason; it never reads as "no code", "no
# logs" or an empty value. Bash clears `set -e` inside $(...), so each read is checked here and its
# caller assigns the result to a variable first (an assignment's failure stops the script; inside a
# function run in $(...) the assignment needs its own `|| exit 1`).
whole() { case "$2" in '' | *[!0-9]*) echo "$1 is not a whole number: ${2:0:200}" >&2; exit 1 ;; esac; }
code_at() {
  local c
  c=$(cast_ code "$@") || { printf '%s\n' "$c" | tail -c 400 >&2; echo "eth_getCode failed for $1" >&2; exit 1; }
  case "$c" in 0x*) printf '%s\n' "$c" ;; *) echo "eth_getCode for $1 answered: ${c:0:200}" >&2; exit 1 ;; esac
}
# A cast read whose failure prints its reason (the caller's assignment then stops the script).
read_() {
  local o
  o=$(cast_ "$@") || { printf '%s\n' "$o" | tail -c 400 >&2; echo "cast $1 failed" >&2; exit 1; }
  printf '%s\n' "$o"
}
# Sends from $2 only when none of its transactions waits unmined: a send that timed out can still
# land, and the on-chain checks below see only mined logs (review STORAGE-S16-R9 finding 2).
no_pending() {
  local mined pending
  mined=$(read_ nonce "$2")
  pending=$(read_ nonce "$2" --block pending)
  whole "the mined nonce of $2" "$mined"
  whole "the pending nonce of $2" "$pending"
  [ "$mined" = "$pending" ] || { echo "$1 ($2) has $((pending - mined)) transaction(s) not yet mined: wait for them, then run again; nothing sent" >&2; exit 1; }
}
head_block() {
  local b
  b=$(cast_ block-number) || { printf '%s\n' "$b" | tail -c 400 >&2; echo "eth_blockNumber failed" >&2; exit 1; }
  whole "the chain head" "$b"
  echo "$b"
}
# The first block from $2 on where $1 has code.
code_block() {
  local b=$2 c
  whole "the start block" "$b"
  while :; do
    c=$(code_at "$1" --block "$b") || exit 1
    [ "$c" = 0x ] || break
    b=$((b + 1))
  done
  echo "$b"
}
# Logs of the contract for an event, filtered by topics, as one JSON list: 200 blocks per call
# (sepolia.base.org answers HTTP 413 above that; THREATS STORAGE-S15-1). A failed call stops the
# script with its reason (it never reads as "no logs").
logs_for() {
  local from to end o
  from=$(get block)
  whole "the deploy block" "$from"
  to=$(head_block) || exit 1
  # A lagging endpoint can answer a head before the deploy block: that is not "no logs".
  [ "$to" -ge "$from" ] || { echo "the chain head $to is before the deploy block $from: a lagging endpoint; run again" >&2; exit 1; }
  while [ "$from" -le "$to" ]; do
    end=$((from + 199))
    [ "$end" -le "$to" ] || end=$to
    o=$(cast_ logs --json --from-block "$from" --to-block "$end" --address "$(get contract)" "$@") \
      || { printf '%s\n' "$o" | tail -c 400 >&2; echo "eth_getLogs failed for blocks $from..$end" >&2; exit 1; }
    printf '%s\n' "$o"
    from=$((end + 1))
  done | python3 -I -c '
import json, sys
text, at, out, d = sys.stdin.read(), 0, [], json.JSONDecoder()
while True:
    while at < len(text) and text[at].isspace():
        at += 1
    if at == len(text):
        break
    part, at = d.raw_decode(text, at)
    out.extend(part)
print(json.dumps(out))'
}

cmd=${1:-}
shift || true
case "$cmd" in
plan | deploy)
  [ -z "$(get contract)" ] || { echo "already deployed: $(get contract) (block $(get block))" >&2; exit 1; }
  [ "$cmd" = plan ] || [ "${1:-}" = --yes ] || { echo "deploy sends from the deployer: run plan first, then deploy --yes" >&2; exit 2; }
  # A deploy that was sent but not recorded (forge stopped after broadcasting) is found here
  # by the address it was sent to, never deployed twice.
  pending=$(get pending_contract)
  code=0x
  [ -z "$pending" ] || code=$(code_at "$pending")
  if [ "$code" != 0x ]; then
    first=$(code_block "$pending" "$(get pending_block)")
    put contract "$pending"
    put block "$first"
    echo "found the earlier deploy: $pending (block $(get block))"
    exit 0
  fi
  args=()
  if [ "$cmd" = deploy ]; then
    args=(--broadcast --slow)
    nonce=$(read_ nonce "$deployer")
    whole "the deployer's nonce" "$nonce"
    next=$(read_ compute-address "$deployer" --nonce "$nonce" | grep -oE '0x[0-9a-fA-F]{40}' | tr '[:upper:]' '[:lower:]' || true)
    [ -n "$next" ] || { echo "cast compute-address gave no address" >&2; exit 1; }
    at=$(head_block)
    put pending_contract "$next"
    put pending_block "$at"
  fi
  key=$(deployer_key)
  BASE_SEPOLIA_PRIVATE_KEY=$key ONYX_STORAGE_TOKEN=$tonyx ONYX_SIGNED_BOOK=$book ONYX_STORAGE_TREASURY=$deployer \
    ONYX_STORAGE_EPOCH_BLOCKS=$epoch_blocks forge_script script/DeployStorage.s.sol:DeployStorage "${args[@]}"
  unset key
  [ "$cmd" = deploy ] || { echo "PLAN ONLY: nothing sent"; exit 0; }
  a=$(get pending_contract)
  code=0x
  [ -z "$a" ] || code=$(code_at "$a")
  [ "$code" != 0x ] || { echo "no code at the expected address $a" >&2; exit 1; }
  first=$(code_block "$a" "$(get pending_block)")
  put contract "$a"
  put block "$first"
  put epoch_blocks "$epoch_blocks"
  put treasury "$deployer"
  echo "OnyxStorage $a at block $(get block) ($(cast_ codesize "$a") bytes); conserved: $(cast_ call "$a" 'conserved()(bool)')"
  ;;
keys)
  umask 077
  mkdir -p "$keys"
  for n in $(names) payer; do
    f=$keys/storage-$n.key
    [ -s "$f" ] || python3 -I -c 'import secrets; print(secrets.token_hex(32))' >"$f"
    chmod 600 "$f"
    echo "$n $(addr "$n")"
  done
  ;;
fund)
  need_contract
  [ "${1:-}" = --yes ] || { echo "fund sends from the deployer: fund --yes" >&2; exit 2; }
  to=
  for n in $(names); do
    a=$(addr "$n")
    to=${to:+$to,}$a
  done
  key=$(deployer_key)
  # Topped up to (never added on top of): nodes 0.00012 test ETH (about a week of answers at
  # 0.006 gwei) and 20 tONYX (the bond); the deal payer gas for a few deals and 100 tONYX.
  BASE_SEPOLIA_PRIVATE_KEY=$key ONYX_FUND_TO=$to ONYX_FUND_WEI=$fund_node_wei ONYX_FUND_TONYX=20000000000000000000 \
    ONYX_STORAGE_TOKEN=$tonyx forge_script script/FundStorage.s.sol:FundStorage --broadcast --slow
  BASE_SEPOLIA_PRIVATE_KEY=$key ONYX_FUND_TO=$(addr payer) ONYX_FUND_WEI=$fund_payer_wei ONYX_FUND_TONYX=100000000000000000000 \
    ONYX_STORAGE_TOKEN=$tonyx forge_script script/FundStorage.s.sol:FundStorage --broadcast --slow
  unset key
  for n in $(names) payer; do
    a=$(addr "$n")
    echo "$n $a eth=$(cast_ balance "$a" --ether) tONYX=$(cast_ call "$tonyx" 'balanceOf(address)(uint256)' "$a" | awk '{print $1}')"
  done
  ;;
register)
  need_contract
  for n in $(names); do
    [ -z "$(get "node_$n")" ] || { echo "$n already registered: $(get "node_$n")"; continue; }
    # A registration this operator already sent (activation failed, or the id was not recorded)
    # is never repeated: that would post a second bond.
    op=$(addr "$n")
    no_pending "$n" "$op"
    seen=$(logs_for 'Registered(uint64 indexed,address indexed,uint64,uint256,string)' '' "$op" \
      | python3 -I -c 'import json,sys; print(len(json.load(sys.stdin)))')
    [ "$seen" = 0 ] || { echo "$n ($op) already has $seen registration(s) on chain: finish or cancel it by hand (onyx-storage status --operator $op); nothing sent" >&2; exit 1; }
    out=$(run "$bin/onyx-storage" register $(common) --chain-id "$chain_id" --key "$keys/storage-$n.key" \
      --capacity-gib "$capacity" --url "https://$domain/storage/$n/")
    id=$(printf '%s\n' "$out" | grep -oE 'NODE_ID=0x[0-9a-f]{64}' | cut -d= -f2 || true)
    [ -n "$id" ] || { printf '%s\n' "$out" >&2; echo "register $n: no NODE_ID" >&2; exit 1; }
    put "node_$n" "$id"
    echo "$n NODE_ID=$id"
  done
  ;;
install)
  need_contract
  stage=$(mktemp -d "$HOME/.cache/onyx-storage-install.XXXXXX")
  trap 'rm -rf "$stage"' EXIT
  printf '{"version": 1, "shards": []}\n' >"$stage/catalog.json"
  cp deploy/systemd/onyx-storage@.service "$stage/"
  i=0
  for n in $(names); do
    i=$((i + 1))
    id=$(get "node_$n")
    [ -n "$id" ] || { echo "$n is not registered" >&2; exit 1; }
    keeper=off
    [ "$n" = n1 ] && keeper=on
    # One pass a minute, not every 15 s: the five nodes share this host's address on the public
    # endpoint, which answers HTTP 429 past its rate limit (THREATS STORAGE-S15-4). An epoch is
    # 24 minutes (7,200 blocks of 0.2 s), so answers still go out in the epoch's first minutes.
    cat >"$stage/$n.env" <<EOF
ONYX_ENV=testnet
ONYX_RIGHTS=fallback
ONYX_PAYMENTS=off
ONYX_LISTEN=127.0.0.1:820$i
ONYX_PUBLIC_URL=https://$domain/storage/$n
ONYX_RPC_URL=$public_rpc
ONYX_TRUSTED_PROXIES=127.0.0.1
ONYX_DEPLOYMENT=/etc/onyx/deployment.json
ONYX_CATALOG=/etc/onyx/storage-catalog.json
ONYX_BULK_MAX_GIB=$capacity
ONYX_BULK_QUOTA_GIB=$capacity
ONYX_BULK_MAX_DATASETS=1000
ONYX_STORAGE_CONTRACT=$(get contract)
ONYX_STORAGE_NODE_ID=$id
ONYX_STORAGE_FROM_BLOCK=$(get block)
ONYX_STORAGE_KEEPER=$keeper
ONYX_STORAGE_EVERY_SECS=60
EOF
    install -m 0600 "$keys/storage-$n.key" "$stage/$n.key"
  done
  # One ssh: the files go through a pipe into a fresh 0700 directory on the host that is removed
  # whatever happens (keys never stay behind; review STORAGE-R3-5). The catalog is world-readable:
  # each node's own dynamic user opens it (review STORAGE-R3-2).
  tar -C "$stage" -cz . | ssh "$host" "set -eu
    d=\$(mktemp -d); trap 'rm -rf \"\$d\"' EXIT; tar -xz -C \"\$d\"; cd \"\$d\"
    sudo install -d -m 0700 /etc/onyx/storage
    sudo install -m 0644 catalog.json /etc/onyx/storage-catalog.json
    for n in $(names | paste -sd' '); do
      sudo install -m 0600 \$n.env /etc/onyx/storage/\$n.env
      sudo install -m 0600 \$n.key /etc/onyx/storage/\$n.key
    done
    sudo install -m 0644 onyx-storage@.service /etc/systemd/system/onyx-storage@.service
    sudo systemctl daemon-reload
    for n in $(names | paste -sd' '); do sudo systemctl enable -q onyx-storage@\$n; sudo systemctl restart onyx-storage@\$n; done"
  for n in $(names); do
    ok=0
    for _ in $(seq 60); do
      curl -sf -A onyx-dev "https://$domain/storage/$n/v1/health" >/dev/null && { ok=1; break; }
      sleep 2
    done
    [ "$ok" = 1 ] || { ssh "$host" "sudo journalctl -u onyx-storage@$n -n 20 --no-pager" >&2; echo "$n does not answer" >&2; exit 1; }
    echo "$n answers at https://$domain/storage/$n/"
  done
  ;;
deal)
  need_contract
  [ $# -ge 1 ] || { echo "usage: $0 deal DIR [DIR ...]" >&2; exit 2; }
  payer=$(addr payer)
  for d in "$@"; do
    ds=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["dataset"])' "$d/manifest.json")
    root=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["root"].lower())' "$d/manifest.json")
    [ -z "$(get "deal_$ds")" ] || { echo "$ds already has deal $(get "deal_$ds")"; continue; }
    no_pending payer "$payer"
    # A deal this payer already made for this manifest (the id was not recorded) is never paid
    # twice: its first data word is the manifest root.
    seen=$(logs_for 'DealCreated(uint64 indexed,address indexed,bytes32,bytes32,uint32,uint64,uint8,uint64,uint64,bytes32,uint256,string,string,string)' '' "$payer" \
      | python3 -I -c 'import json,sys; r=sys.argv[1][2:]; print(sum(1 for l in json.load(sys.stdin) if l["data"][2:66].lower()==r))' "$root")
    [ "$seen" = 0 ] || { echo "$ds: the payer already has a deal for manifest $root on chain: record its id by hand; nothing sent" >&2; exit 1; }
    out=$(run "$bin/onyx-storage" deal $(common) --chain-id "$chain_id" --key "$keys/storage-payer.key" \
      --dir "$d" --r 3 --epochs "$deal_epochs" --source-url "https://$domain/")
    id=$(printf '%s\n' "$out" | grep -oE 'DEAL_ID=[0-9]+' | cut -d= -f2 || true)
    [ -n "$id" ] || { printf '%s\n' "$out" >&2; echo "deal $ds: no DEAL_ID" >&2; exit 1; }
    put "deal_$ds" "$id"
    printf '%s\n' "$out"
  done
  ;;
status)
  need_contract
  for n in $(names); do
    echo "== $n"
    run "$bin/onyx-storage" status $(common) --node "$(get "node_$n")"
  done
  echo "== epoch"
  run "$bin/onyx-storage" epoch $(common)
  echo "== books"
  run "$bin/onyx-storage" books $(common)
  ;;
close)
  need_contract
  a=$(get contract)
  now=$(read_ call "$a" 'currentEpoch()(uint64)' | awk '{print $1}')
  whole "the current epoch" "$now"
  for k in $(python3 -I -c 'import json,sys; print(" ".join(sorted(k for k in json.load(open(sys.argv[1])) if k.startswith("deal_"))))' "$state"); do
    id=$(get "$k")
    # dealInfo: ..., endEpoch (9th), deposit, paidOut, closed (12th).
    info=$(read_ call "$a" 'dealInfo(uint64)(address,bytes32,bytes32,bytes32,uint32,uint8,uint64,uint64,uint64,uint256,uint256,bool)' "$id" | awk '{print $1}')
    end=$(sed -n 9p <<<"$info")
    closed=$(sed -n 12p <<<"$info")
    # Finding 5 of STORAGE-S16-R9: a value that is not a number never reads as "ended".
    whole "deal $id's end epoch" "$end"
    case "$closed" in true | false) ;; *) echo "deal $id: closed is ${closed:0:80}" >&2; exit 1 ;; esac
    if [ "$closed" = true ]; then
      echo "${k#deal_} (deal $id): already closed"
    elif [ "$now" -lt "$end" ]; then
      echo "${k#deal_} (deal $id): runs until epoch $end (now $now)"
    else
      run "$bin/onyx-storage" close $(common) --chain-id "$chain_id" --key "$keys/storage-payer.key" --deal "$id" | tail -1
      echo "${k#deal_} (deal $id): closed; deposit $(sed -n 10p <<<"$info"), paid $(sed -n 11p <<<"$info")"
    fi
  done
  for n in $(names); do
    op=$(addr "$n")
    e=$(read_ call "$a" 'earned(address)(uint256)' "$op" | awk '{print $1}')
    whole "the earnings of $n" "$e"
    if [ "$e" = 0 ]; then
      echo "$n: nothing to withdraw"
    else
      run "$bin/onyx-storage" withdraw $(common) --chain-id "$chain_id" --key "$keys/storage-$n.key" | tail -1
      echo "$n: withdrew $e"
    fi
  done
  echo "conserved: $(cast_ call "$a" 'conserved()(bool)')"
  ;;
*)
  sed -n '2,27p' "$0" >&2
  exit 2
  ;;
esac
