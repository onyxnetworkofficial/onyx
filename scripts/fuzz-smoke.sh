#!/usr/bin/env bash
# Fuzz smoke (ROADMAP L3, docs/audit/2026-10-13-fuzzing.md): build every cargo-fuzz target in
# fuzz/ and run each one for FUZZ_SECS seconds (default 30) on its corpus plus the seeds
# fuzz/common's `seedgen` writes from the project's fixtures. Fails if any target crashes;
# the reproducer is left in fuzz/artifacts/<target>/.
#
# Opt-in, not part of scripts/check-all.sh: it needs a nightly toolchain and cargo-fuzz
#   rustup toolchain install nightly --profile minimal
#   cargo install --locked cargo-fuzz
# Options (environment): FUZZ_SECS=30, FUZZ_TARGETS="rpc_json merkle_proof" (default: all),
# FUZZ_RSS_MB=2048 (libFuzzer's memory limit per target, a huge allocation is a crash).
# Crashes found so far are unit tests on the stable toolchain (cargo test), so check-all keeps them.
set -euo pipefail
cd "$(dirname "$0")/../fuzz"
secs=${FUZZ_SECS:-30}
rss=${FUZZ_RSS_MB:-2048}
if ! command -v cargo-fuzz >/dev/null; then
  echo "fuzz-smoke: needs cargo-fuzz (cargo install --locked cargo-fuzz)"; exit 2
fi
if ! rustup run nightly rustc --version >/dev/null 2>&1; then
  echo "fuzz-smoke: needs a nightly toolchain (rustup toolchain install nightly --profile minimal)"; exit 2
fi
echo "fuzz-smoke: $(rustup run nightly rustc --version), $(cargo fuzz --version)"
# Seeds from the fixtures and real values (deterministic; fuzz/seeds is not committed).
cargo +nightly run --release -q -p onyx-fuzz-common --bin seedgen -- seeds
# No sanitizer: the fuzzed code is safe Rust (unsafe_code = deny); debug assertions and
# overflow checks stay on (cargo-fuzz default), so an overflow is a crash.
cargo +nightly fuzz build -s none
targets=${FUZZ_TARGETS:-$(cargo +nightly fuzz list)}
log=$(mktemp -d)
fail=0
printf '%-16s %10s %8s %8s %s\n' target execs cov corpus result
for t in $targets; do
  mkdir -p "corpus/$t" "seeds/$t"
  if cargo +nightly fuzz run -s none "$t" "corpus/$t" "seeds/$t" -- \
      -max_total_time="$secs" -rss_limit_mb="$rss" -timeout=10 -print_final_stats=1 \
      >"$log/$t.log" 2>&1; then
    result=ok
  else
    result="CRASH (see fuzz/artifacts/$t, log $log/$t.log)"
    fail=1
  fi
  execs=$(grep -o 'stat::number_of_executed_units: *[0-9]*' "$log/$t.log" | grep -o '[0-9]*$' | tail -1)
  cov=$(grep -o 'cov: [0-9]*' "$log/$t.log" | tail -1 | grep -o '[0-9]*')
  corpus=$(find "corpus/$t" -type f | wc -l)
  printf '%-16s %10s %8s %8s %s\n' "$t" "${execs:-?}" "${cov:-?}" "$corpus" "$result"
done
[ "$fail" -eq 0 ] && echo "fuzz-smoke: all targets ran ${secs}s without a crash"
exit "$fail"
