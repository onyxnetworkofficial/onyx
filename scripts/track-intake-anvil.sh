#!/usr/bin/env bash
# ROADMAP C5 (ADR-0036 amendment 1): the track-record intake against a real local chain. Starts its
# own anvil on a loopback port the anvil picks (parallel runs never share one), stops it on any
# exit, and runs `examples/track-intake-anvil.rs`: signed statements, batch headers posted as
# calldata from anvil's dev account 3 to a sink, `committed_at` from the posts' blocks, sealed until
# close, a client recheck from its own chain reads, then a revoked key, a replay, a statement past
# its close, a backdated log and a hidden post, each refused.
# Local only: chain id 31337, anvil's public unlocked dev account (no key is read or passed); cast
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
cargo build --locked -q -p onyx-coordinator --example track-intake-anvil
# Port 0: anvil binds a free port itself and prints it, so no other process can take it between
# a probe and the bind. Probing a port first and passing it raced parallel check-all runs (other
# worktrees): when another anvil won the port, this one died on "Address already in use" and the
# run went on against the other anvil, posting from its dev account (review TR-R4-3).
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
bin=${CARGO_TARGET_DIR:-$PWD/target}/debug/examples/track-intake-anvil
(cd "$work" && "$bin" --rpc "$RPC" --cast "$FOUNDRY_BIN/cast") \
  2>&1 | tee "$work/run.log"
grep -q '^TRACK INTAKE ANVIL PASSED$' "$work/run.log"
