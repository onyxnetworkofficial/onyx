#!/usr/bin/env bash
# Every spec file must have Scope, Interfaces, Assumptions and Open questions.
# Every ADR must have Status, Context, Decision and Consequences, and appear in DECISIONS.md.
# THREATS.md rows follow CLAUDE.md (test or TODO with an open milestone; unique IDs).
# docs/ops/launch-checklist.md stays checkable (scripts/check-launch.sh).
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0
need() { # file heading...
  local f=$1; shift
  for h in "$@"; do
    grep -qE "^## $h" "$f" || { echo "MISSING '## $h' in $f"; fail=1; }
  done
}
for f in docs/spec/*.md; do [ "$(basename "$f")" = README.md ] && continue; need "$f" "Scope" "Interfaces" "Assumptions" "Open questions"; done
for f in docs/adr/[0-9]*.md; do need "$f" "Status" "Context" "Decision" "Consequences"; done
for s in claims recipes labels validation disputes settlement delivery pricing rights; do
  [ -f "docs/spec/$s.md" ] || { echo "MISSING docs/spec/$s.md"; fail=1; }
done
# Every ADR file is listed in the DECISIONS.md index (L9 sweep: 0020-0032 were missing).
for f in docs/adr/[0-9]*.md; do
  grep -qF "](docs/adr/$(basename "$f"))" DECISIONS.md || { echo "MISSING $f in DECISIONS.md"; fail=1; }
done
# THREATS.md: every row names its test, an ACCEPTED reason, a docs-only fix or a TODO with an open
# milestone; open rows name an open milestone; IDs are unique (scripts/check-threats.py).
python3 -I scripts/check-threats.py || fail=1
# docs/api/openapi.json against the routes, error codes, the error table, Caddy and the site copy
# (ADR-0045).
python3 -I scripts/check-openapi.py || fail=1
[ "$fail" -eq 0 ] && { python3 -I scripts/check-openapi.py --self-test || fail=1; }
# README.md's layout and routes, scripts/README.md and docs/README.md are generated from the
# repository and must be current (PP7; `scripts/check-readme.py --write` regenerates them).
python3 -I scripts/check-readme.py || fail=1
[ "$fail" -eq 0 ] && { python3 -I scripts/check-readme.py --self-test || fail=1; }
# Its own self-test: planted fake citations, bare ACCEPTED, missing docs paths must fail.
[ "$fail" -eq 0 ] && { python3 -I scripts/check-threats.py --self-test || fail=1; }
# The launch checklist (ROADMAP L10): every evidence link resolves, every gate has an owner, a state
# and a link, ticked == VERIFIED, and the Count line matches (scripts/check-launch.sh).
scripts/check-launch.sh || fail=1
[ "$fail" -eq 0 ] && { scripts/check-launch.sh --self-test || fail=1; }
# The served-source terms table (docs/ops/data-sources.md section 5, LC-27): shape only here; the
# dated run is a release step (docs/ops/runbooks/release-source-terms.md), never part of check-all.
python3 -I scripts/check-source-terms.py --lint || fail=1
[ "$fail" -eq 0 ] && { python3 -I scripts/check-source-terms.py --self-test || fail=1; }
# Public snapshot (open-source checklist E): the exclude list covers every path DATA-LICENSE.md
# section 1 names, so an uncleared file cannot reach a snapshot.
python3 -I scripts/public-snapshot-coverage.py . >/dev/null || { python3 -I scripts/public-snapshot-coverage.py .; fail=1; }
[ "$fail" -eq 0 ] && echo "docs ok: $(ls docs/spec/*.md | wc -l) specs, $(ls docs/adr/[0-9]*.md | wc -l) ADRs"
exit "$fail"
