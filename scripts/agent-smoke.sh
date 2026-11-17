#!/usr/bin/env bash
# NS8 (ADR-0047): the agent toolkit end to end on this machine. Starts the demo API twice (free,
# and paid with the local mock facilitator; no chain, no real keys), then runs apps/agent's tests
# with the live part on: an MCP session asks for a row, checks it with onyx-check, sees a changed
# row refused, and gets a payment offer from the paid server.
# THREATS AGT-2 (unchecked is not verified), AGT-5 (base URL), AGT-6 (requests from the shipped
# file), AGT-7 (redirects, limits), AGT-8 (tools from the shipped file, notifications, writes
# opt-in), AGT-9 (do not pay an unchecked offer) and AGT-10 (answers checked by id) are the tests
# in apps/agent/test.
#
# Usage: scripts/agent-smoke.sh [--self-test]
# --self-test checks only how a test log is read (no build, no server): a TAP log gives its three
# count lines, and a log in another reporter's format, or a failed run, ends with its reason.
#
# No failure is silent: every failing path names its step and prints the log it has, and any other
# command that fails is named by the ERR trap. (The public CI failed here for two weeks with no
# line at all: Node 24 prints the `spec` format when its output is not a terminal, where Node 22
# printed TAP, so the count lines were not found and `set -e` ended the run. The format is now
# asked for by name.)
set -Eeuo pipefail
cd "$(dirname "$0")/.."
trap 'st=$?; echo "agent-smoke: FAILED (exit $st) at line $LINENO: $BASH_COMMAND" >&2' ERR
fail() { # step, then the log file to print (optional)
  echo "agent-smoke: FAILED at: $1 ($(node --version 2>/dev/null || echo "node not found"))" >&2
  if [ -n "${2:-}" ] && [ -f "$2" ]; then
    echo "--- output ($2) ---" >&2
    cat "$2" >&2
    echo "--- end of output ---" >&2
  fi
  trap - ERR
  exit 1
}
# The toolkit's tests into the log $1, in TAP whatever this Node's default reporter is. Extra
# arguments are environment assignments for the run.
run_tests() {
  local log=$1 st=0
  shift
  env "$@" node --test --test-reporter=tap apps/agent/test/*.test.mjs >"$log" 2>&1 || st=$?
  [ "$st" = 0 ] || fail "the toolkit's tests (node --test apps/agent/test, exit $st)" "$log"
}
# Prints the pass, fail and skipped counts of the TAP log $1; all three must be there.
summary() {
  local lines
  lines="$(grep -E '^# (pass|fail|skipped) [0-9]+$' "$1" || true)"
  [ "$(grep -c . <<<"$lines")" = 3 ] ||
    fail "reading the test counts (expected '# pass', '# fail' and '# skipped' lines of a TAP log)" "$1"
  echo "$lines"
}
if [ "${1:-}" = --self-test-unhandled ]; then # the self-test's planted failure, nothing else
  false
fi
if [ "${1:-}" = --self-test ]; then
  work="$(mktemp -d "${TMPDIR:-/tmp}/onyx-agent-smoke-self.XXXXXX")"
  trap 'rm -rf "$work"' EXIT
  printf 'TAP version 13\nok 1 - a\n# tests 3\n# pass 2\n# fail 0\n# skipped 1\n' >"$work/tap.log"
  [ "$(summary "$work/tap.log")" = $'# pass 2\n# fail 0\n# skipped 1' ] || fail "self-test: a TAP log's counts" "$work/tap.log"
  # What Node 24 prints by default when its output is a file: no line starts with `# `.
  printf '\xe2\x9c\x94 a (1ms)\n\xe2\x84\xb9 tests 3\n\xe2\x84\xb9 pass 2\n\xe2\x84\xb9 fail 0\n\xe2\x84\xb9 skipped 1\n' >"$work/spec.log"
  for case in "summary $work/spec.log:reading the test counts" "fail planted-step $work/tap.log:planted-step"; do
    st=0
    (${case%%:*}) >"$work/out" 2>&1 || st=$?
    [ "$st" = 1 ] || { echo "agent-smoke self-test: '${case%%:*}' ended with $st, expected 1" >&2; exit 1; }
    grep -qF "agent-smoke: FAILED at: ${case#*:}" "$work/out" && grep -qF -- "--- output (" "$work/out" ||
      { echo "agent-smoke self-test: '${case%%:*}' did not name its step and print the log:" >&2; cat "$work/out" >&2; exit 1; }
  done
  # A command that fails with no handler of its own is named by the ERR trap.
  st=0
  bash scripts/agent-smoke.sh --self-test-unhandled >"$work/out" 2>&1 || st=$?
  [ "$st" = 1 ] && grep -qF "agent-smoke: FAILED (exit 1) at line" "$work/out" ||
    { echo "agent-smoke self-test: the ERR trap did not name a failed command:" >&2; cat "$work/out" >&2; exit 1; }
  echo "agent-smoke self-test ok: counts read from TAP; another format, a failed step and a failed command are each reported"
  exit 0
fi
# --workspace (not -p): the same feature set as check-all's `cargo test --workspace`, so these
# targets reuse its build instead of compiling a second variant of the shared crates.
cargo build --locked -q --workspace --bin onyx-check --example serve-api
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
  local name=$1 log="$work/$1.log"
  shift
  "$target/examples/serve-api" --dev "$@" --port 0 >"$log" 2>&1 &
  pids+=("$!")
  echo "$!" >>"$work/pids"
  for _ in $(seq 1 600); do
    # pipefail-ok: the log is the few lines a server prints at start
    if url="$(grep -oE 'http://127\.0\.0\.1:[0-9]+' "$log" | head -n 1)" && [ -n "$url" ]; then
      echo "$url"
      return 0
    fi
    kill -0 "${pids[-1]}" 2>/dev/null || break
    sleep 0.2
  done
  fail "starting the $name demo server (serve-api --dev $* --port 0): it printed no address" "$log"
}

# The demo API serves the test-label shard: without the label data (not public until its rights are
# cleared, DATA-LICENSE.md) only the toolkit's own tests run, and their live part is reported skipped.
if [ ! -f fixtures/labels/test.onyx.entities.btc.v2.json ]; then
  run_tests "$work/test.log"
  summary "$work/test.log"
  echo "AGENT SMOKE PASSED (toolkit tests; the live part needs the label data, not public yet)"
  exit 0
fi
free="$(start free)" || fail "the free demo server (its output is above)"
paid="$(start paid --paid)" || fail "the paid demo server (its output is above)"
# An absolute CARGO_TARGET_DIR (a shared cache) is used as is; a relative one is under this checkout.
check_bin="$target/onyx-check"
[[ "$check_bin" = /* ]] || check_bin="$PWD/$check_bin"
run_tests "$work/test.log" ONYX_AGENT_FREE_URL="$free" ONYX_AGENT_PAID_URL="$paid" ONYX_CHECK_BIN="$check_bin"
summary "$work/test.log"
grep -q '^# skipped 0$' "$work/test.log" || fail "the live part: some live agent tests were skipped" "$work/test.log"
echo "AGENT SMOKE PASSED"
