#!/usr/bin/env bash
# Dependency gate (ROADMAP L1, docs/audit/2026-10-13-dependencies.md).
#   cargo deny: licenses, bans and sources always (offline); RustSec advisories
#               when the advisory database can be fetched.
#   pnpm audit: apps/web against the npm advisory database, when online and pnpm exists.
# Fails on any new advisory, licence outside deny.toml, banned crate or unknown source.
# Offline: the network checks are skipped with a message; the offline ones still run.
set -euo pipefail
cd "$(dirname "$0")/.."

command -v cargo-deny >/dev/null || {
  echo "cargo-deny missing: cargo install --locked cargo-deny@0.20.2"; exit 1; }

online() { # url
  curl -fsS --max-time 10 -o /dev/null "$1" 2>/dev/null
}

if online https://github.com/RustSec/advisory-db; then
  cargo deny -L error --locked check advisories licenses bans sources
else
  echo "deps: offline, RustSec advisories SKIPPED (licenses, bans, sources still checked)"
  cargo deny -L error --locked check licenses bans sources
fi

# ROADMAP C50 (THREATS C50-10, docs/audit/2026-10-17-blst.md): blst stays at the reviewed version in the main and
# the fuzz lock files (deny.toml lets only onyx-tlock depend on it), and onyx-tlock holds no
# unsafe code of its own (the workspace lint denies it; this catches a local allow).
for lock in Cargo.lock fuzz/Cargo.lock; do
  v=$(awk '/^name = "blst"$/ { getline; print }' "$lock")
  if [ "$v" != 'version = "0.3.17"' ]; then
    echo "deps: $lock has blst '${v:-none}', the reviewed version is 0.3.17 (docs/audit/2026-10-17-blst.md)"; exit 1
  fi
done
if grep -rnE 'unsafe[[:space:]]*(\{|fn|impl|extern)|allow\(unsafe_code\)' crates/onyx-tlock/src crates/onyx-tlock/tests crates/onyx-tlock/examples; then
  echo "deps: onyx-tlock must hold no unsafe code (docs/audit/2026-10-17-blst.md)"; exit 1
fi
grep -q '^#!\[forbid(unsafe_code)\]' crates/onyx-tlock/src/lib.rs \
  || { echo "deps: onyx-tlock lost #![forbid(unsafe_code)]"; exit 1; }
if ! command -v pnpm >/dev/null; then
  echo "deps: pnpm missing, apps/web npm audit SKIPPED"
elif ! online https://registry.npmjs.org/; then
  echo "deps: offline, apps/web npm audit SKIPPED"
else
  # Exits non-zero on any advisory of any severity. Reads pnpm-lock.yaml; no install.
  (cd apps/web && pnpm audit --audit-level low)
fi
echo "deps ok"
