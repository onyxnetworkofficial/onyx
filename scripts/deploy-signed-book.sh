#!/usr/bin/env bash
# ROADMAP C10 (ADR-0028 amendment 7): deploy OnyxSignedBook, the level-3 book of record, next to a
# running ledger, and pin it (address, runtime code hash, reviewer) into that ledger's deployment
# manifest, so nodes and clients serve level 3 only against it. Runbook:
# docs/ops/runbooks/deploy-signed-book.md.
#
#   scripts/deploy-signed-book.sh anvil   --rpc URL --anvil-config FILE --reviewer ADDR --manifest FILE [--plan]
#       a local anvil or an anvil fork of Base Sepolia (loopback, answers as anvil); the deployer
#       is the anvil's first dev key from its --config-out FILE (public, holds nothing elsewhere);
#       sends unless --plan (the rehearsal runs --plan too: review C10-R2-3)
#   scripts/deploy-signed-book.sh testnet --reviewer ADDR --manifest FILE --plan|--yes
#       Base Sepolia (84532). LEAD ONLY. The deployer key is BASE_SEPOLIA_PRIVATE_KEY from .env,
#       read into forge's environment only, never printed or put on a command line; the RPC is
#       BASE_SEPOLIA_RPC_URL from .env (default https://sepolia.base.org). --plan simulates and
#       prints the gas, sending nothing; --yes sends.
#
# Order: chain checks; forge dry run (refuses a reviewer that is the deployer before anything is
# signed, THREATS C10-1; the book's constructor refuses it too); broadcast; pin (onyx-node example
# pin-signed-book: chain id, this checkout's build with only `reviewer` filled in, reviewer(), and
# the reviewer is not the manifest settlement's coordinator()); read-back lines. Any failure
# stops with a non-zero exit and leaves the manifest as it was. forge and cast run in a temp dir
# outside the repo (Foundry loads a `.env` from its working directory, M8-L55-F4).
# Env: FOUNDRY_BIN (~/.foundry/bin), CARGO_TARGET_DIR, ONYX_PIN_BIN (a prebuilt pin-signed-book).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
usage="usage: $0 anvil --rpc URL --anvil-config FILE [--plan] | testnet --plan|--yes|--pin-only BROADCAST; both: --reviewer ADDR --manifest FILE [--replace]"
mode=${1:-}
case "$mode" in
  anvil | testnet) shift ;;
  *) echo "$usage" >&2; exit 2 ;;
esac
rpc="" anvil_config="" reviewer="" manifest="" plan="" yes="" replace=() pin_only=""
while [ $# -gt 0 ]; do
  a=$1
  shift
  case "$a" in
    --rpc | --anvil-config | --reviewer | --manifest | --pin-only)
      [ $# -gt 0 ] || { echo "$a needs a value" >&2; exit 2; }
      case "$a" in
        --pin-only) pin_only=$1 ;;
        --rpc) rpc=$1 ;;
        --anvil-config) anvil_config=$1 ;;
        --reviewer) reviewer=$1 ;;
        --manifest) manifest=$1 ;;
      esac
      shift
      ;;
    --plan) plan=1 ;;
    --yes) yes=1 ;;
    --replace) replace=(--replace) ;;
    *) echo "unknown argument: $a" >&2; echo "$usage" >&2; exit 2 ;;
  esac
done
[ -n "$reviewer" ] || { echo "--reviewer ADDR is required (never the deployer)" >&2; exit 2; }
[[ "$reviewer" =~ ^0x[0-9a-fA-F]{40}$ ]] || { echo "--reviewer must be a 0x address" >&2; exit 2; }
[ -n "$manifest" ] && [ -f "$manifest" ] || { echo "--manifest FILE (the ledger's deployment.json) is required" >&2; exit 2; }
manifest=$(cd "$(dirname "$manifest")" && pwd)/$(basename "$manifest")

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT TERM HUP
castw() { (cd "$work" && ETH_RPC_URL=$rpc "$FOUNDRY_BIN/cast" "$@"); }
lower() { tr '[:upper:]' '[:lower:]'; }
# A value from .env, without printing it (only the named key is read).
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

manifest_chain=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["chainId"])' "$manifest")
if [ "$mode" = anvil ]; then
  [ -n "$rpc" ] && [ -n "$anvil_config" ] || { echo "anvil mode needs --rpc URL and --anvil-config FILE" >&2; exit 2; }
  case "$rpc" in
    http://127.0.0.1:* | http://localhost:*) ;;
    *) echo "anvil mode runs only on a loopback RPC" >&2; exit 2 ;;
  esac
  castw rpc web3_clientVersion | lower | grep -q anvil \
    || { echo "the node at $rpc is not an anvil" >&2; exit 1; }
  key=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["private_keys"][0])' "$anvil_config")
  # --plan runs here exactly as in testnet mode (the rehearsal checks it sends nothing).
  [ -n "$plan" ] || yes=1
else
  [ -z "$rpc" ] && [ -z "$anvil_config" ] || { echo "testnet mode takes the RPC and key from .env only" >&2; exit 2; }
  [ -n "$plan" ] || [ -n "$yes" ] || [ -n "$pin_only" ] || { echo "a testnet deploy sends from the deployer: run with --plan first, then --yes" >&2; exit 2; }
  rpc=$(env_value BASE_SEPOLIA_RPC_URL)
  rpc=${rpc:-https://sepolia.base.org}
  # --pin-only sends nothing, so it never reads the deployer key (review C10-R2-3).
  if [ -z "$pin_only" ]; then
    key=$(env_value BASE_SEPOLIA_PRIVATE_KEY)
    [ -n "$key" ] || { echo "BASE_SEPOLIA_PRIVATE_KEY is missing from .env" >&2; exit 1; }
  else
    key=""
  fi
fi
chain=$(castw chain-id)
case "$mode:$chain" in
  anvil:31337 | anvil:84532 | testnet:84532) ;;
  *) echo "chain $chain is not allowed in $mode mode (anvil: 31337 or a Base Sepolia fork; testnet: 84532)" >&2; exit 1 ;;
esac
[ "$chain" = "$manifest_chain" ] || { echo "the manifest is for chain $manifest_chain, the node is chain $chain" >&2; exit 1; }
echo "chain $chain ($mode); reviewer $reviewer; manifest $manifest"
# Printed tool output never shows the RPC URL (it can carry an API key).
scrub() { RPC_URL=$rpc python3 -I -c 'import os,sys; u=os.environ["RPC_URL"]; [sys.stdout.write(l.replace(u, "<rpc>")) for l in sys.stdin]'; }
lc() { tr '[:upper:]' '[:lower:]'; }
# Review C10-R1-5: the reviewer is not the manifest settlement's coordinator, checked before
# anything is sent (the dry run compares it with the deploy key; pin-signed-book checks again).
settlement=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["settlement"])' "$manifest")
coordinator=$(castw call "$settlement" 'coordinator()(address)' | lc)
[ "$coordinator" != "$(echo "$reviewer" | lc)" ] \
  || { echo "the reviewer is the settlement's coordinator $coordinator: refused (THREATS C10-1); nothing sent" >&2; exit 1; }
# Build the pin tool before anything is sent, and before the RPC URL is in the environment
# (review C10-R1-4, R1-6): a failed build never leaves a deployed book unpinned.
if [ -n "${ONYX_PIN_BIN:-}" ]; then
  pin=$ONYX_PIN_BIN
else
  cargo build --locked -q -p onyx-node --example pin-signed-book
  pin=${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/pin-signed-book
fi
[ -x "$pin" ] || { echo "no pin-signed-book binary at $pin" >&2; exit 1; }
pin_run() {
  ONYX_PIN_RPC=$rpc "$pin" --manifest "$manifest" --broadcast "$1" \
    --artifact "$REPO/contracts/out/OnyxSignedBook.sol/OnyxSignedBook.json" --reviewer "$reviewer" "${replace[@]}" 2>&1 | scrub
  return "${PIPESTATUS[0]}"
}
if [ -n "$pin_only" ]; then
  # Resume a deploy whose pin step failed: pin the book a kept broadcast names (no transaction).
  [ -s "$pin_only" ] || { echo "--pin-only: no broadcast file $pin_only" >&2; exit 2; }
  unset key
  (cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/build.log" 2>&1) \
    || { scrub <"$work/build.log" >&2; exit 1; }
  echo "pin only (nothing sent)"
  pin_run "$pin_only"
  echo "SIGNED BOOK PINNED"
  exit 0
fi

# Build in the repo (cached), then run forge from a copy so the broadcast stays out of the repo.
(cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/build.log" 2>&1) \
  || { cat "$work/build.log" >&2; exit 1; }
cp -a "$REPO/contracts" "$work/contracts"
rm -rf "$work/contracts/broadcast" "$work/contracts/cache/DeploySignedBook.s.sol"
sed -i "s#$REPO/contracts#$work/contracts#g" "$work/contracts/cache/solidity-files-cache.json"
printf '\n[rpc_endpoints]\nonyx_pin = "${ONYX_PIN_RPC}"\n' >>"$work/contracts/foundry.toml"
forge_run() {
  (cd "$work/contracts" && ONYX_PIN_RPC=$rpc BASE_SEPOLIA_PRIVATE_KEY=$key ONYX_SIGNED_REVIEWER=$reviewer \
    "$FOUNDRY_BIN/forge" script script/DeploySignedBook.s.sol:DeploySignedBook --rpc-url onyx_pin "$@")
}

echo "1. dry run (simulation; nothing sent)"
if ! forge_run >"$work/dry.log" 2>&1; then
  grep -E 'Error|revert|ReviewerIsDeployer|NoReviewer|WrongChain|ChainMismatch' "$work/dry.log" | head -5 | scrub >&2
  echo "dry run refused: nothing sent" >&2
  exit 1
fi
grep -E 'Estimated (total gas used|amount required)' "$work/dry.log" || true
if [ -n "$plan" ]; then
  echo "PLAN ONLY: nothing sent; rerun with --yes to deploy"
  exit 0
fi
[ -n "$yes" ] || exit 2

echo "2. deploy"
broadcast=$work/contracts/broadcast/DeploySignedBook.s.sol/$chain/run-latest.json
kept=$manifest.signed-book-broadcast.json
if ! forge_run --broadcast --slow >"$work/deploy.log" 2>&1; then
  tail -20 "$work/deploy.log" | scrub >&2
  # Review C10-R2-3: forge may have sent the creation and then failed (a receipt timeout). Keep
  # whatever broadcast it wrote before the temp dir goes, so the book is found, never redeployed.
  if [ -s "$broadcast" ]; then
    cp "$broadcast" "$kept"
    echo "deploy failed after forge wrote a broadcast, kept at $kept: check the chain for the book before any rerun; if it is there, use --pin-only $kept" >&2
  fi
  exit 1
fi
unset key
[ -s "$broadcast" ] || { echo "no broadcast file" >&2; exit 1; }
book=$(python3 -I -c 'import json,sys
v=json.load(open(sys.argv[1]))
print(next(t["contractAddress"] for t in v["transactions"] if t["contractName"]=="OnyxSignedBook" and t["transactionType"]=="CREATE"))' "$broadcast")
tx=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["receipts"][0]["transactionHash"])' "$broadcast")
echo "  OnyxSignedBook $book (tx $tx)"
# Keep the broadcast next to the manifest: a failed pin resumes with --pin-only (review C10-R1-4).
cp "$broadcast" "$kept"
echo "  broadcast kept at $kept"

echo "3. pin into the manifest"
pin_run "$kept" || { echo "pin failed; the book is deployed: fix the cause, then rerun with --pin-only $kept" >&2; exit 1; }

echo "4. read back"
echo "  reviewer() $(castw call "$book" 'reviewer()(address)')"
echo "  codehash   $(castw codehash "$book")"
echo "  DISPUTE_WINDOW_SECS $(castw call "$book" 'DISPUTE_WINDOW_SECS()(uint64)')"
echo "SIGNED BOOK DEPLOYED AND PINNED"
