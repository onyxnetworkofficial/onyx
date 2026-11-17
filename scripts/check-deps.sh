#!/usr/bin/env bash
# Dependency gate (ROADMAP L1, docs/audit/2026-10-13-dependencies.md).
#   cargo deny: licenses, bans and sources always (offline); RustSec advisories
#               when the advisory database can be fetched.
#   pnpm audit: apps/web against the npm advisory database, when online and pnpm exists.
#               Reviewed advisories are ignored by id, each one time-boxed (see "Reviewed npm advisories" below).
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
# Reviewed npm advisories (THREATS DEP-4, DEP-5; docs/audit/2026-10-13-dependencies.md "Reviewed npm
# advisories"). `pnpm audit` skips the ids in apps/web/package.json `pnpm.auditConfig.ignoreGhsas`;
# each of them needs an `auditExceptions` entry with a THREATS row, a reason, the review date and
# a re-check date at most 14 days later. Printed on every run; from the day after the re-check
# date the gate fails until the advisory is fixed or reviewed again. Runs offline too.
python3 -I - apps/web/package.json THREATS.md <<'PY'
import datetime, json, re, sys
pkg = json.load(open(sys.argv[1]))
threats = open(sys.argv[2], encoding="utf-8").read()
audit = pkg.get("pnpm", {}).get("auditConfig", {})
ignored = audit.get("ignoreGhsas", [])
exc = pkg.get("auditExceptions", {})
today = datetime.datetime.now(datetime.timezone.utc).date()
bad = []
if audit.get("ignoreCves"):
    bad.append("ignoreCves is not allowed; ignore by GHSA id with an auditExceptions entry")
if len(set(ignored)) != len(ignored):
    bad.append("ignoreGhsas lists an id twice")
for g in sorted(set(exc) - set(ignored)):
    bad.append(f"{g}: has an auditExceptions entry but is not in ignoreGhsas (remove the entry)")
for g in ignored:
    e = exc.get(g)
    if not isinstance(e, dict):
        bad.append(f"{g}: ignored without an auditExceptions entry (threat, package, severity, reason, reviewed, recheck)")
        continue
    try:
        reviewed = datetime.date.fromisoformat(e["reviewed"])
        recheck = datetime.date.fromisoformat(e["recheck"])
        threat, reason = str(e["threat"]), str(e["reason"])
        package, severity = str(e["package"]), str(e["severity"])
    except (KeyError, TypeError, ValueError) as err:
        bad.append(f"{g}: auditExceptions entry is incomplete or has a bad date ({err!r})")
        continue
    if not re.fullmatch(r"DEP-\d+", threat) or not re.search(rf"^\| {threat} \|.*{g}", threats, re.M):
        bad.append(f"{g}: THREATS.md has no row {threat} that names {g}")
    if len(reason.split()) < 8:
        bad.append(f"{g}: the reason is too short to be a reason")
    if reviewed > today:
        bad.append(f"{g}: reviewed {reviewed} is in the future")
    if not reviewed <= recheck <= reviewed + datetime.timedelta(days=14):
        bad.append(f"{g}: recheck {recheck} must be 0 to 14 days after reviewed {reviewed}")
    left = (recheck - today).days
    print("deps: ==================== NPM ADVISORY IGNORED ====================")
    print(f"deps: {g} ({severity}, {package}) is NOT reported by pnpm audit: THREATS {threat}")
    print(f"deps:   reason: {reason}")
    print(f"deps:   reviewed {reviewed}, re-check by {recheck} ({left} day(s) left)")
    if left < 0:
        bad.append(f"{g}: the re-check date {recheck} has passed. Look for a fixed {package} "
                   f"(pnpm view {package} versions, and the advisory's patched versions): fix and drop "
                   "the ignore, or review the exposure again and set new reviewed and recheck dates")
if ignored:
    print("deps: ==============================================================")
for b in bad:
    print(f"deps: npm advisory exception: {b}")
sys.exit(1 if bad else 0)
PY

if ! command -v pnpm >/dev/null; then
  echo "deps: pnpm missing, apps/web npm audit SKIPPED"
elif ! online https://registry.npmjs.org/; then
  echo "deps: offline, apps/web npm audit SKIPPED"
else
  # Exits non-zero on any advisory of any severity. Reads pnpm-lock.yaml; no install.
  (cd apps/web && pnpm audit --audit-level low)
fi
echo "deps ok"
