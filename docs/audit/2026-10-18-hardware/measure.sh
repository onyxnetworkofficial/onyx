#!/usr/bin/env bash
# P5 measurements: peak RSS and wall time of Onyx's own release runs (/usr/bin/time -v).
# Build first: cargo test --release --no-run -p onyx-source-btc -p onyx-web-verify and
# cargo build --release --example bench-golden. Run as (docs/ops/hardware.md section 4):
#   systemd-run --user --scope -p MemoryMax=12G nice -n 10 measure.sh
# Logs go to $ONYX_MEASURE_LOGS; runs.txt in this folder is the 2026-10-18 output.
set -uo pipefail
W=${ONYX_WORKTREE:?the Onyx checkout}
T=${CARGO_TARGET_DIR:-$W/target}/release
L=${ONYX_MEASURE_LOGS:?a directory for the logs}
FL=${ONYX_FIXTURES_LIVE:?the fixtures-live block directories}
mkdir -p "$L"
bin() { ls -t "$T"/deps/"$1"-* 2>/dev/null | grep -v '\.d$' | head -1; }
run() { # name dir cmd...
  local name=$1 dir=$2; shift 2
  echo "== $name" >"$L/$name.txt"
  echo "load before: $(cat /proc/loadavg)" >>"$L/$name.txt"
  (cd "$dir" && /usr/bin/time -v "$@") >"$L/$name.out" 2>"$L/$name.time"
  echo "exit: $?" >>"$L/$name.txt"
  echo "load after: $(cat /proc/loadavg)" >>"$L/$name.txt"
  grep -E 'Elapsed|Maximum resident|User time|System time|Percent of CPU' "$L/$name.time" >>"$L/$name.txt"
  cat "$L/$name.txt"
}
SB=$W/adapters/source-btc
run bench-golden "$W" "$T/examples/bench-golden"
run golden "$SB" "$(bin golden)" --test-threads 1
ONYX_LIVE_DIR=$FL/node run golden-series-24 "$SB" "$(bin golden_series)" --test-threads 1
ONYX_WEEK_DIR=$FL/week run golden-pools-week "$SB" "$(bin golden_pools)" --test-threads 1
ONYX_HISTORY_DIR=$FL/week run golden-history-week "$SB" "$(bin golden_history)" --test-threads 1
ONYX_RESERVE_DIRS=$FL/week:$FL/tail run golden-reserve-week "$SB" "$(bin golden_reserve)" --test-threads 1
VW=$W/apps/web/verify
run verify-lib "$VW" "$(bin onyx_web_verify)" --test-threads 1
run verify-reserves "$VW" "$(bin reserves)" --test-threads 1
echo done
