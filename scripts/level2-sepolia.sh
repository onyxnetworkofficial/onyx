#!/usr/bin/env bash
# ROADMAP B6: one live level-2 round on Base Sepolia (testnet only) for a dated source, on the
# OnyxRegistry of a testnet deploy run, with 7 observer processes that each sign their own
# commit. Takes about 30 minutes: the 300 s window, commits close 60 s after it, reveals 120 s
# later, then the coordinator waits until the commit phase is FINAL on Base Sepolia (about 20
# minutes) before it reads the commits, receipts the reveals and records the value on chain.
#
# Usage: scripts/level2-sepolia.sh <usgs|ncei|fiscaldata|manifest> <export dir> [state dir]
#   manifest      a CL3 manifest source (ADR-0053, no per-source code): ONYX_L2_MANIFEST names
#                 its manifest.json (default sources/us.treasury.tga_total_deposits/manifest.json);
#                 the coordinator registers its template if the registry does not hold it yet and
#                 refuses another template under its metric (THREATS CL3-8); its store is
#                 <export dir>/observations/<manifest folder name>.json
#   <export dir>  the --export-node directory of the testnet run (its deployment.json names the
#                 registry); the round's results store is written to
#                 <export dir>/observations/<source>.json (appended, never replaced) and its
#                 template pin to <export dir>/observations/pins (scripts/deploy-api.sh ships both)
#   [state dir]   the round book, spool and observer state (default ~/.cache/onyx-b6/<source>);
#                 a run that stops unfinished is finished by running again with the same dir
#
# Who signs (keys are read by the programs, never printed or put on a command line):
# - coordinator: the deployer key from .env (BASE_SEPOLIA_PRIVATE_KEY), passed to the
#   coordinator process only, in its environment; it must be OnyxSettlement.coordinator()
# - registry owner: .onyx-testnet/keys/registry-owner.key (registers the metric once)
# - observers: .onyx-testnet/keys/validator-9..15.key, the run's standby validators (the pool
#   snapshot lists them with a declared bond; the registry does not hold observer bonds)
# - receipt key (ROADMAP B3): .onyx-testnet/keys/l2-receipt.key, made here once (random, 0600)
#
# What this does NOT show: independence. All 8 processes share one host, one network path and
# one operator (THREATS L2-SRC-1). Reveals go through a local spool, not sealed on chain
# (no --reveal-key: REVLOG-1 stays open for these rounds and the API says so in `limits`).
#
# Env: ONYX_L2_RPC (https://sepolia.base.org), ONYX_L2_LEAD (seconds until the window opens,
# 150), FOUNDRY_BIN (~/.foundry/bin), ONYX_L2_RECORD_DATE (default `latest`: the newest day the
# rule allows), ONYX_L2_SERIES (north-star loop NL17: another series of the source, e.g.
# `usw00094728.tmax` for ncei or `la300.m2_5` for usgs; its store is
# <export dir>/observations/<source>-<series>.json and its state dir defaults to
# ~/.cache/onyx-b6/<source>-<series>).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
[ $# -ge 2 ] || { echo "usage: $0 <usgs|ncei|fiscaldata|manifest> <export dir> [state dir]" >&2; exit 2; }
src=$1
export_dir=$(cd "$2" && pwd)
case "$src" in
  usgs | ncei | fiscaldata | manifest) ;;
  *) echo "unknown source $src" >&2; exit 2 ;;
esac
manifest=""
if [ "$src" = manifest ]; then
  manifest=$(realpath -e "${ONYX_L2_MANIFEST:-sources/us.treasury.tga_total_deposits/manifest.json}")
  [ -z "${ONYX_L2_SERIES:-}" ] || { echo "ONYX_L2_SERIES does not apply to a manifest source" >&2; exit 2; }
fi
series=${ONYX_L2_SERIES:-}
case "$series" in
  "" | [a-z0-9]*[a-z0-9]) ;;
  *) echo "ONYX_L2_SERIES: lowercase letters, digits, dots and underscores only" >&2; exit 2 ;;
esac
[ -z "${series//[a-z0-9._]/}" ] || { echo "ONYX_L2_SERIES: lowercase letters, digits, dots and underscores only" >&2; exit 2; }
name=$src${series:+-$series}
[ -z "$manifest" ] || name=$(basename "$(dirname "$manifest")")
state=${3:-$HOME/.cache/onyx-b6/$name}
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
RPC=${ONYX_L2_RPC:-https://sepolia.base.org}
LEAD=${ONYX_L2_LEAD:-150}
KEYS=$REPO/.onyx-testnet/keys
mkdir -p "$state"
chmod 700 "$state"
work=$(mktemp -d "$HOME/.cache/onyx-b6-cast.XXXXXX")
pids=()
cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
# cast runs outside the repo: Foundry loads a .env from its working directory (M8-L55-F4).
castw() { (cd "$work" && ETH_RPC_URL=$RPC "$FOUNDRY_BIN/cast" "$@"); }

[ "$(castw chain-id)" = 84532 ] || { echo "not Base Sepolia (chain 84532): refused" >&2; exit 1; }
REG=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["registry"])' "$export_dir/deployment.json")
SETTLEMENT=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["settlement"])' "$export_dir/deployment.json")
REVENUE=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["revenue"])' "$export_dir/deployment.json")
COORD=$(castw call "$SETTLEMENT" "coordinator()(address)")
OWNER=$(castw call "$REG" "owner()(address)")
REPORTER=$(castw call "$REVENUE" "reporter()(address)")

# Private copies: every dated source's examples share these names in target/debug/examples, so a
# build of another source during the round would swap the files under this run.
bin=$work/bin
mkdir "$bin"
if [ -n "$manifest" ]; then
  CARGO_INCREMENTAL=0 cargo build -q -p onyx-level2 --example level2-manifest
  cp "${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/level2-manifest" "$bin/"
  # The same two commands as a dated source's, bound to this manifest.
  for role in coordinator observer; do
    printf '#!/bin/sh\nexec %q %s --manifest %q --repo %q "$@"\n' \
      "$bin/level2-manifest" "$role" "$manifest" "$REPO" >"$bin/level2-$role"
    chmod 700 "$bin/level2-$role"
  done
  pin_cmd=("$bin/level2-manifest" pin --manifest "$manifest" --repo "$REPO")
else
  CARGO_INCREMENTAL=0 cargo build -q -p "onyx-source-$src" --example level2-coordinator --example level2-observer
  cp "${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/level2-coordinator" \
    "${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/level2-observer" "$bin/"
  pin_cmd=("$bin/level2-coordinator" --pin)
fi
addr_of() { "$bin/level2-observer" --key-file "$1" --address; }

# The receipt key of this deployment (ROADMAP B3): made once, never printed.
receipt=$KEYS/l2-receipt.key
if [ ! -e "$receipt" ]; then
  python3 -I - "$receipt" <<'PY'
import os, secrets, sys
fd = os.open(sys.argv[1], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
os.write(fd, secrets.token_hex(32).encode()); os.close(fd)
PY
fi
[ "$(addr_of "$KEYS/registry-owner.key")" = "$(echo "$OWNER" | tr 'A-F' 'a-f')" ] \
  || { echo "registry-owner.key is not the registry's owner $OWNER" >&2; exit 1; }

# The pool snapshot: 7 observers, each with gas for its commit.
observers=(9 10 11 12 13 14 15)
pool=$state/pool.json
python3 -I - "$pool" "${observers[@]/#/$KEYS/validator-}" <<PY
import json, subprocess, sys
out, keys = sys.argv[1], sys.argv[2:]
pool = [{"address": subprocess.run(["$bin/level2-observer", "--key-file", k + ".key", "--address"],
                                    check=True, capture_output=True, text=True).stdout.strip(),
         "bond": 1000} for k in keys]
json.dump(pool, open(out, "w"))
PY
min_wei=10000000000000 # 0.00001 ETH: about 10 commits at Base Sepolia's fee
for a in $(python3 -I -c 'import json,sys; print(" ".join(o["address"] for o in json.load(open(sys.argv[1]))))' "$pool") "$COORD" "$OWNER"; do
  bal=$(castw balance "$a")
  [ "$bal" -ge "$min_wei" ] || { echo "$a holds $bal wei: needs at least $min_wei (fund it from the deployer)" >&2; exit 1; }
done
nonces() { for a in "$@"; do castw nonce "$a"; done | paste -sd' '; }
balances() { for a in "$@"; do castw balance "$a"; done | paste -sd' '; }
actors=("$COORD" "$OWNER" $(python3 -I -c 'import json,sys; print(" ".join(o["address"] for o in json.load(open(sys.argv[1]))))' "$pool"))
n0=$(nonces "${actors[@]}")
b0=$(balances "${actors[@]}")

key=$(python3 -I - "$REPO/.env" <<'PY'
import sys
for line in open(sys.argv[1]).read().splitlines():
    k, sep, v = line.partition("=")
    if sep and k.strip() == "BASE_SEPOLIA_PRIVATE_KEY":
        print(v.strip().strip('"').strip("'"))
        break
PY
)
[ -n "$key" ] || { echo "BASE_SEPOLIA_PRIVATE_KEY is missing from .env" >&2; exit 1; }

mkdir -p -m 700 "$state/spool" "$state/observers" "$state/logs"
mkdir -p "$export_dir/observations"
store=$export_dir/observations/$name.json
new_store=()
[ -e "$store" ] || new_store=(--new-store)
if [ -e "$state/window-start" ]; then
  window_start=$(cat "$state/window-start") # finishing an unfinished round on its draw
else
  window_start=$(( $(date +%s) + LEAD ))
  echo "$window_start" >"$state/window-start"
fi
feed=(--record-date "${ONYX_L2_RECORD_DATE:-latest}" --window-start "$window_start")
chain=(--rpc "$RPC" --registry "$REG" --cast "$FOUNDRY_BIN/cast")
for i in "${observers[@]}"; do
  "$bin/level2-observer" --key-file "$KEYS/validator-$i.key" --pool "$pool" --spool "$state/spool" \
    --state "$state/observers/$i" --receipt-wait 3600 "${feed[@]}" "${chain[@]}" \
    >"$state/logs/observer-$i.log" 2>&1 &
  pids+=($!)
done
echo "$name: 7 observers started on Base Sepolia (registry $REG); the window opens at $window_start (in $(( window_start - $(date +%s) )) s), about 30 minutes in all"

set +e
ONYX_L2_COORDINATOR_KEY=$key "$bin/level2-coordinator" "${chain[@]}" --coordinator "$COORD" \
  --owner "$OWNER" --owner-key "$KEYS/registry-owner.key" --reporter "$REPORTER" \
  --book "$state/book.json" --store "$store" "${new_store[@]}" --pool "$pool" \
  --spool "$state/spool" --receipt-key "$receipt" "${feed[@]}" \
  >"$state/logs/round.json" 2>"$state/logs/coordinator.log"
rc=$?
set -e
unset key
if [ $rc != 0 ]; then
  echo "the coordinator stopped (exit $rc):" >&2
  tail -5 "$state/logs/coordinator.log" >&2
  echo "run again with the same state dir to finish the round on the same draw" >&2
  exit 1
fi
cat "$state/logs/round.json"
# A round that recorded a value has written the store, so its pin goes in first: a later check of
# this run failing must not leave a served store without its pin (NL15 did, NL24).
if python3 -I -c 'import json,sys; r=json.load(open(sys.argv[1])); sys.exit(0 if r["outcome"].startswith("Value(") and r["level2_claim"] is True else 1)' "$state/logs/round.json"; then
  # The node's pin for this store (ONYX_OBSERVATION_TEMPLATES; scripts/deploy-api.sh reads it).
  pin=$("${pin_cmd[@]}")
  pins=$export_dir/observations/pins
  touch "$pins"
  { grep -v "^${pin%%=*}=" "$pins" || true; echo "$pin"; } >"$pins.new"
  mv "$pins.new" "$pins"
fi

receipted=0 undrawn=0 failed=0 resumed=0
for idx in "${!pids[@]}"; do
  i=${observers[$idx]}
  if wait "${pids[$idx]}"; then
    if grep -q "receipt on chain" "$state/logs/observer-$i.log"; then receipted=$((receipted + 1));
    elif grep -q "not drawn" "$state/logs/observer-$i.log"; then undrawn=$((undrawn + 1));
    else failed=$((failed + 1)); fi
  elif grep -q "resuming the saved commit and reveal" "$state/logs/observer-$i.log" \
    && grep -q "past reveal close; nothing sent" "$state/logs/observer-$i.log"; then
    # A rerun on the same draw after this observer revealed in an earlier run: nothing is left
    # for it to send, so it exits non-zero; the round's own record says how it answered (NL25).
    resumed=$((resumed + 1))
  else
    failed=$((failed + 1)); echo "observer $i failed:"; tail -3 "$state/logs/observer-$i.log"
  fi
done
pids=()
rm -f "$state/window-start"
n1=$(nonces "${actors[@]}")
b1=$(balances "${actors[@]}")
python3 -I - "$state/logs/round.json" "$receipted" "$undrawn" "$failed" "$n0" "$n1" "$b0" "$b1" "$resumed" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
receipted, undrawn, failed = map(int, sys.argv[2:5])
n0, n1, b0, b1 = (list(map(int, a.split())) for a in sys.argv[5:9])
resumed = int(sys.argv[9])
txs = sum(n1) - sum(n0)
spent = sum(b0) - sum(b1)
print(f"observers: {receipted} receipted on chain, {resumed} revealed in an earlier run of this round, {undrawn} not drawn, {failed} failed")
print(f"chain: {txs} transactions, {spent / 1e18:.9f} ETH (coordinator {n1[0]-n0[0]}, owner {n1[1]-n0[1]}, observers {sum(n1[2:])-sum(n0[2:])})")
assert r["outcome"].startswith("Value("), r["outcome"]
assert r["commits_from_chain"] == 5 and r["reveals"] == 5, r
# A drawn observer whose own fetch failed (the source did not answer it) is `Missed`; the round
# still settles when a strict majority of the drawn agree, the coordinator's own rule
# (`onyx_coordinator::observe::quorum`; NL17: one of five missed on 2026-10-28, NL15: two of five
# on 2026-10-29, which an "at most one" check here wrongly stopped before the pin, NL24).
agreed = sum(1 for o in r["observers"] if o["status"] == "Agreed")
assert agreed >= len(r["observers"]) // 2 + 1, r["observers"]
others = [o["status"] for o in r["observers"] if o["status"] != "Agreed"]
if others:
    print(f"note: {len(others)} drawn observer(s) not agreed: {others}")
assert r["level2_claim"] is True
assert (receipted + resumed, undrawn, failed) == (5, 2, 0), (receipted, resumed, undrawn, failed)
PY
echo "results store: $store (pin $pin)"
echo "LEVEL-2 BASE SEPOLIA ROUND PASSED ($name)"
