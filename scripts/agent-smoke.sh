#!/usr/bin/env bash
# NS8 (ADR-0047): the agent toolkit end to end on this machine. Starts the demo API twice (free,
# and paid with the local mock facilitator; no chain, no real keys), then runs apps/agent's tests
# with the live part on: an MCP session asks for a row, checks it with onyx-check, sees a changed
# row refused, and gets a payment offer from the paid server.
# THREATS AGT-2 (unchecked is not verified), AGT-5 (base URL), AGT-6 (requests from the shipped
# file), AGT-7 (redirects, limits), AGT-8 (tools from the shipped file, notifications, writes
# opt-in), AGT-9 (do not pay an unchecked offer) and AGT-10 (answers checked by id) are the tests
# in apps/agent/test.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --locked -q -p onyx-check
cargo build --locked -q -p onyx-source-btc --example serve-api
target="${CARGO_TARGET_DIR:-target}/debug"
# An absolute CARGO_TARGET_DIR (a shared cache) stays as it is; a relative one is under this checkout.
case $target in /*) ;; *) target=$PWD/$target ;; esac
work="$(mktemp -d "${TMPDIR:-/tmp}/onyx-agent-smoke.XXXXXX")"
pids=()
cleanup() {
  # `start` runs in a command substitution (a subshell), so its servers' ids are in a file too:
  # an id added to `pids` there never reached this shell, and the servers outlived the run.
  [ -f "$work/pids" ] && while read -r p; do kill "$p" 2>/dev/null || true; done <"$work/pids"
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  rm -rf "$work"
}
trap cleanup EXIT

# Start one demo server on a free port; prints its URL once it listens.
start() {
  local log="$work/$1.log"
  shift
  "$target/examples/serve-api" --dev "$@" --port 0 >"$log" 2>&1 &
  pids+=("$!")
  echo "$!" >>"$work/pids"
  for _ in $(seq 1 600); do
    if url="$(grep -oE 'http://127\.0\.0\.1:[0-9]+' "$log" | head -n 1)" && [ -n "$url" ]; then
      echo "$url"
      return 0
    fi
    kill -0 "${pids[-1]}" 2>/dev/null || break
    sleep 0.2
  done
  echo "demo server did not start:" >&2
  cat "$log" >&2
  return 1
}

# The demo API serves the test-label shard: without the label data (not public until its rights are
# cleared, DATA-LICENSE.md) only the toolkit's own tests run, and their live part is reported skipped.
if [ ! -f fixtures/labels/test.onyx.entities.btc.v2.json ]; then
  node --test apps/agent/test/*.test.mjs >"$work/test.log" 2>&1 || { cat "$work/test.log"; exit 1; }
  grep -E '^# (pass|fail|skipped)' "$work/test.log"
  echo "AGENT SMOKE PASSED (toolkit tests; the live part needs the label data, not public yet)"
  exit 0
fi
free="$(start free)"
paid="$(start paid --paid)"
# An absolute CARGO_TARGET_DIR (a shared cache) is used as is; a relative one is under this checkout.
check_bin="$target/onyx-check"
[[ "$check_bin" = /* ]] || check_bin="$PWD/$check_bin"
ONYX_AGENT_FREE_URL="$free" ONYX_AGENT_PAID_URL="$paid" ONYX_CHECK_BIN="$check_bin" \
  node --test apps/agent/test/*.test.mjs >"$work/test.log" 2>&1 || { cat "$work/test.log"; exit 1; }
grep -E '^# (pass|fail|skipped)' "$work/test.log"
grep -q '^# skipped 0$' "$work/test.log" || { echo "live agent tests were skipped"; exit 1; }
echo "AGENT SMOKE PASSED"
