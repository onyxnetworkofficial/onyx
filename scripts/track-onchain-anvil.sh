#!/usr/bin/env bash
# ROADMAP C7 (ADR-0036 amendment 3): track records resolved from on-chain facts only and committed
# on chain, against a real local chain. Builds the contracts (cached), starts its own anvil on a
# loopback port the anvil picks, stops it on any exit, and runs `examples/track-onchain-anvil.rs`:
# OnyxSignedBook deployed, operators declared, level-3 reports posted, a commitment derived from the
# book at a pinned head and posted to the book's log, a client recheck from its own reads; then an
# omitted and a forged resolution, a hidden affiliation, a late declaration, a rollback, a fork, spam
# and a reorg under a pinned head, each refused (THREATS TR-5, TR-R2-4, TR-R2-5, TR-R1-11, TR-S-14,
# TR-C7-2, TR-C7-4, TR-C7-9, TR-C7-R1-3, TR-C7-R1-9).
# Local only: chain id 31337, anvil's public unlocked dev accounts (no key is read or passed); cast
# runs in a temp dir outside the repo (Foundry loads a `.env` from its working directory, M8-L55-F4).
# Env: FOUNDRY_BIN (~/.foundry/bin).
set -euo pipefail
cd "$(dirname "$0")/.."
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
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
(cd contracts && "$FOUNDRY_BIN/forge" build --skip test >"$work/forge.log" 2>&1) \
  || { cat "$work/forge.log"; echo "forge build failed"; exit 1; }
artifact=$PWD/contracts/out/OnyxSignedBook.sol/OnyxSignedBook.json
[ -s "$artifact" ] || { echo "no OnyxSignedBook artifact"; exit 1; }
cargo build --locked -q -p onyx-coordinator --example track-onchain-anvil
# Port 0: anvil binds a free port itself and prints it (review TR-R4-3).
(cd "$work" && exec "$FOUNDRY_BIN/anvil" --host 127.0.0.1 --port 0 >"$work/anvil.log" 2>&1) &
anvil_pid=$!
port=""
for _ in $(seq 200); do
  kill -0 "$anvil_pid" 2>/dev/null || { cat "$work/anvil.log"; echo "anvil exited"; exit 1; }
  port=$(grep -oE 'Listening on 127\.0\.0\.1:[0-9]+' "$work/anvil.log" | head -n1 | sed 's/.*://' || true)
  [ -n "$port" ] && break
  sleep 0.1
done
[ -n "$port" ] || { echo "anvil did not report its port"; exit 1; }
RPC=http://127.0.0.1:$port
[ "$(cd "$work" && "$FOUNDRY_BIN/cast" chain-id --rpc-url "$RPC")" = 31337 ] || { echo "not a local anvil"; exit 1; }
kill -0 "$anvil_pid" 2>/dev/null || { echo "anvil exited"; exit 1; }
bin=${CARGO_TARGET_DIR:-$PWD/target}/debug/examples/track-onchain-anvil
(cd "$work" && "$bin" --rpc "$RPC" --cast "$FOUNDRY_BIN/cast" --artifact "$artifact") \
  2>&1 | tee "$work/run.log"
grep -q '^TRACK ONCHAIN ANVIL PASSED$' "$work/run.log"
