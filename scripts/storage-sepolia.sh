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
#
# Every step can be run again: a deploy that was sent is found by its address, a node or deal
# already on chain is never registered or paid twice (review STORAGE-R3-3), funding tops up to a
# target. `install` uses the onyx-node that scripts/deploy-api.sh put on the host: deploy the API
# from the same commit first.
# Env: ONYX_STORAGE_BIN_DIR (onyx-storage and onyx-publish, default target/release),
# ONYX_SITE_HOST (onyx-web), ONYX_SITE_DOMAIN (onyx.cryptagraph.com), STORAGE_NODES (5),
# STORAGE_EPOCH_BLOCKS (7200: four hours at 2 s), STORAGE_DEAL_EPOCHS (42: a week),
# STORAGE_CAPACITY_GIB (6 per node: five nodes fit the host's disk).
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
export ETH_RPC_URL=$rpc
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
# The first block from $2 on where $1 has code.
code_block() {
  local b=$2
  while [ "$(cast_ code "$1" --block "$b")" = 0x ]; do b=$((b + 1)); done
  echo "$b"
}
# Logs of the contract for an event, filtered by topics, as JSON.
logs_for() { cast_ logs --json --from-block "$(get block)" --address "$(get contract)" "$@"; }

cmd=${1:-}
shift || true
case "$cmd" in
plan | deploy)
  [ -z "$(get contract)" ] || { echo "already deployed: $(get contract) (block $(get block))" >&2; exit 1; }
  [ "$cmd" = plan ] || [ "${1:-}" = --yes ] || { echo "deploy sends from the deployer: run plan first, then deploy --yes" >&2; exit 2; }
  # A deploy that was sent but not recorded (forge stopped after broadcasting) is found here
  # by the address it was sent to, never deployed twice.
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
  BASE_SEPOLIA_PRIVATE_KEY=$key ONYX_STORAGE_TOKEN=$tonyx ONYX_SIGNED_BOOK=$book ONYX_STORAGE_TREASURY=$deployer \
    ONYX_STORAGE_EPOCH_BLOCKS=$epoch_blocks forge_script script/DeployStorage.s.sol:DeployStorage "${args[@]}"
  unset key
  [ "$cmd" = deploy ] || { echo "PLAN ONLY: nothing sent"; exit 0; }
  a=$(get pending_contract)
  [ "$(cast_ code "$a")" != 0x ] || { echo "no code at the expected address $a" >&2; exit 1; }
  put contract "$a"
  put block "$(code_block "$a" "$(get pending_block)")"
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
  to=$(for n in $(names); do addr "$n"; done | paste -sd,)
  key=$(deployer_key)
  # Topped up to (never added on top of): nodes 0.00012 test ETH (about a week of answers at
  # 0.006 gwei) and 20 tONYX (the bond); the deal payer gas for a few deals and 100 tONYX.
  BASE_SEPOLIA_PRIVATE_KEY=$key ONYX_FUND_TO=$to ONYX_FUND_WEI=120000000000000 ONYX_FUND_TONYX=20000000000000000000 \
    ONYX_STORAGE_TOKEN=$tonyx forge_script script/FundStorage.s.sol:FundStorage --broadcast --slow
  BASE_SEPOLIA_PRIVATE_KEY=$key ONYX_FUND_TO=$(addr payer) ONYX_FUND_WEI=30000000000000 ONYX_FUND_TONYX=100000000000000000000 \
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
    seen=$(logs_for 'Registered(uint64 indexed,address indexed,uint64,uint256,string)' '' "$op" \
      | python3 -I -c 'import json,sys; print(len(json.load(sys.stdin)))')
    [ "$seen" = 0 ] || { echo "$n ($op) already has $seen registration(s) on chain: finish or cancel it by hand (onyx-storage status --operator $op); nothing sent" >&2; exit 1; }
    out=$(run "$bin/onyx-storage" register $(common) --chain-id "$chain_id" --key "$keys/storage-$n.key" \
      --capacity-gib "$capacity" --url "https://$domain/storage/$n/")
    id=$(printf '%s\n' "$out" | grep -oE 'NODE_ID=0x[0-9a-f]{64}' | cut -d= -f2)
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
    # A deal this payer already made for this manifest (the id was not recorded) is never paid
    # twice: its first data word is the manifest root.
    seen=$(logs_for 'DealCreated(uint64 indexed,address indexed,bytes32,bytes32,uint32,uint64,uint8,uint64,uint64,bytes32,uint256,string,string,string)' '' "$payer" \
      | python3 -I -c 'import json,sys; r=sys.argv[1][2:]; print(sum(1 for l in json.load(sys.stdin) if l["data"][2:66].lower()==r))' "$root")
    [ "$seen" = 0 ] || { echo "$ds: the payer already has a deal for manifest $root on chain: record its id by hand; nothing sent" >&2; exit 1; }
    out=$(run "$bin/onyx-storage" deal $(common) --chain-id "$chain_id" --key "$keys/storage-payer.key" \
      --dir "$d" --r 3 --epochs "$deal_epochs" --source-url "https://$domain/")
    id=$(printf '%s\n' "$out" | grep -oE 'DEAL_ID=[0-9]+' | cut -d= -f2)
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
*)
  sed -n '2,24p' "$0" >&2
  exit 2
  ;;
esac
