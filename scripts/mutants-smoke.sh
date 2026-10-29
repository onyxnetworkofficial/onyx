#!/usr/bin/env bash
# Opt-in mutation smoke test (ROADMAP L4, docs/audit/2026-10-13-mutation.md): cargo-mutants on a
# few files that carry money and verification rules. Every mutant must be caught (or time out,
# which is an infinite loop the tests notice). Not part of check-all: about 30 minutes on the dev
# box. Needs `cargo install --locked cargo-mutants` (free, MIT).
#
#   scripts/mutants-smoke.sh            # all four scopes
#   MUTANTS_JOBS=3 CARGO_BUILD_JOBS=4 scripts/mutants-smoke.sh   # parallel copies, compile jobs
#
# Scopes (each runs only its own package's fast tests):
#   1. onyx-merkle: the whole crate (roots, proofs, verify).
#   2. onyx-core claim.rs: claim and cross-source quorum validation.
#   3. onyx-coordinator revenue.rs: the usage revenue split (R-REV-2).
#   4. onyx-api x402.rs `exact_evm::*` and settle_check.rs: payment precheck, EIP-712 digest,
#      signature recovery, the settle check after an unknown settle.
#
# cargo-mutants copies the tree into $TMPDIR and builds each copy in its own target, so this sets
# TMPDIR under target/ (not a RAM-backed /tmp) and leaves CARGO_TARGET_DIR unset for the run.
set -uo pipefail
cd "$(dirname "$0")/.."
if ! cargo mutants --version >/dev/null 2>&1; then
  echo "cargo-mutants is not installed: cargo install --locked cargo-mutants"
  exit 1
fi
jobs="${MUTANTS_JOBS:-2}"
root="$PWD/target/mutants-smoke"
rm -rf "$root"
mkdir -p "$root/tmp"
# The test binaries keep the caller's TMPDIR (usually a RAM disk): the onyx-api tests fsync
# journals, 12 s on tmpfs against 270 s on disk. A runner restores it for them only.
orig_tmp="${TMPDIR:-/tmp}"
printf '#!/bin/sh\nexport TMPDIR="%s"\nexec "$@"\n' "$orig_tmp" >"$root/runner.sh"
chmod +x "$root/runner.sh"
host=$(rustc -vV | sed -n 's/^host: //p' | tr 'a-z-' 'A-Z_')
export "CARGO_TARGET_${host}_RUNNER=$root/runner.sh"
export TMPDIR="$root/tmp"
unset CARGO_TARGET_DIR
# A memory cap where systemd user scopes exist (the dev box); plain otherwise (CI, containers).
cap=()
if command -v systemd-run >/dev/null 2>&1 && systemd-run --user --scope -q true >/dev/null 2>&1; then
  cap=(systemd-run --user --scope -q -p MemoryMax=12G)
fi

fail=0
run() {
  local name=$1
  shift
  echo "== $name"
  "${cap[@]}" cargo mutants --jobs "$jobs" --test-workspace=false --no-shuffle \
    -o "$root/$name" "$@" >"$root/$name.log" 2>&1
  local rc=$?
  tail -n 1 "$root/$name.log"
  # Exit 0: all caught; 2: some missed; 3: some timed out (an infinite loop the tests notice;
  # cargo-mutants returns 3 even when some were also missed, so missed.txt decides). Anything
  # else (1 usage, 4 baseline failed) is a failure of the run itself.
  local missed="$root/$name/mutants.out/missed.txt"
  if { [ "$rc" != 0 ] && [ "$rc" != 2 ] && [ "$rc" != 3 ]; } || [ ! -f "$missed" ] || [ -s "$missed" ]; then
    echo "FAILED ($name, cargo-mutants exit $rc); missed:"
    cat "$missed" 2>/dev/null
    fail=1
  fi
}

run merkle -p onyx-merkle
run core-claim -p onyx-core -f crates/onyx-core/src/claim.rs -- --lib
run revenue -p onyx-coordinator -f crates/onyx-coordinator/src/revenue.rs -- --lib
run x402 -p onyx-api -f crates/onyx-api/src/x402.rs -f crates/onyx-api/src/settle_check.rs \
  --re 'x402\.rs:.* in exact_evm::|settle_check\.rs:' -- --lib -- --skip idx_r4 --skip idx_r3_property

rm -rf "$root/tmp"
if [ "$fail" != 0 ]; then
  echo "MUTANTS SMOKE FAILED (outputs in target/mutants-smoke)"
  exit 1
fi
echo "MUTANTS SMOKE PASSED"
