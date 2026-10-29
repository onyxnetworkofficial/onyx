#!/usr/bin/env bash
# Build a history-free snapshot of the tracked tree in a new folder OUT and check it, for the
# open-source checklist's option A (docs/ops/open-source-checklist.md; partner tracker
# 2026-10-24): a new public repository from today's files, with no history, so the old commits'
# excluded data (OSS-3) and author e-mail (OSS-18) never go public. It never pushes, publishes,
# creates a repository or changes any visibility: that switch is the project owner's own step (checklist D).
#   scripts/public-snapshot.sh OUT                    whole tree; refused while DATA-LICENSE.md
#                                                     section 1 is still pending
#   scripts/public-snapshot.sh OUT --without-pending  leave out every path in
#                                                     docs/ops/public-snapshot-exclude.txt
#   scripts/public-snapshot.sh OUT --self-test        as --without-pending, then plant a fake personal
#                                                     address and a fake key: the checks must fail
# Checks in OUT: no left-out path remains; gitleaks over the folder (redacted); no e-mail address
# except reviewed placeholders; it prints file:line for any hit, never a value. Exit 0 only when
# every check passes. Env: ONYX_GITLEAKS (gitleaks binary, default `gitleaks`).
# Always (PP13): the paths in docs/ops/public-snapshot-private.txt are left out, and
# scripts/public-scrub.py replaces the owner's and partners' names and local home paths, then
# fails when any is left.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:?usage: public-snapshot.sh OUT [--without-pending]}"
mode="${2:-}"
[ -z "$mode" ] || [ "$mode" = --without-pending ] || [ "$mode" = --self-test ] || { echo "unknown option $mode"; exit 2; }
plant=0
if [ "$mode" = --self-test ]; then plant=1; mode=--without-pending; fi
[ ! -e "$out" ] || { echo "$out exists; give a new folder"; exit 2; }
pending=0
grep -q 'not licensed for redistribution; pending\*\*' "$root/DATA-LICENSE.md" && pending=1
if [ "$pending" = 1 ] && [ "$mode" != --without-pending ]; then
  echo "DATA-LICENSE.md section 1 is still pending (no written label permission on file):"
  echo "use --without-pending, or file the permissions first (launch checklist LC-1)"
  exit 2
fi
python3 -I "$root/scripts/public-snapshot-coverage.py" "$root" || { echo "the exclude list is behind DATA-LICENSE.md section 1"; exit 2; }
mkdir -p "$out"
git -C "$root" archive --format=tar HEAD | tar -x -C "$out"
echo "snapshot of $(git -C "$root" rev-parse --short HEAD): $(find "$out" -type f | wc -l) files"
fail=0
leaks=0
if [ "$mode" = --without-pending ]; then
  removed=0
  while IFS= read -r pat; do
    case "$pat" in ''|'#'*) continue ;; esac
    for f in $out/$pat; do
      [ -e "$f" ] || continue
      rm -rf "$f"; removed=$((removed + 1))
    done
  done < "$root/docs/ops/public-snapshot-exclude.txt"
  echo "left out (pending label rights): $removed paths"
  while IFS= read -r pat; do
    case "$pat" in ''|'#'*) continue ;; esac
    for f in $out/$pat; do
      if [ -e "$f" ]; then echo "STILL PRESENT: ${f#$out/}"; fail=1; fi
    done
  done < "$root/docs/ops/public-snapshot-exclude.txt"
fi
# PP13: without the label-derived files, the tests that read them are ignored, not failed: the
# crates' `label-data` feature (on in this repository) is off in the snapshot.
if [ "$mode" = --without-pending ]; then
  grep -rl --include=Cargo.toml '^default = \["label-data"\]' "$out" | while IFS= read -r f; do
    sed -i 's/^default = \["label-data"\]/default = []/' "$f"
  done
  echo "label-data feature off in: $(grep -rl --include=Cargo.toml '^default = \[\]' "$out" | wc -l) crates"
fi
# PP13: the partners' notes and the scrub tool never go public; names and home paths are scrubbed.
private=0
while IFS= read -r pat; do
  case "$pat" in ''|'#'*) continue ;; esac
  for f in $out/$pat; do
    [ -e "$f" ] || continue
    rm -rf "$f"; private=$((private + 1))
  done
done < "$root/docs/ops/public-snapshot-private.txt"
echo "left out (private notes): $private paths"
python3 -I "$root/scripts/public-scrub.py" "$out" || { echo "names or home paths are left (see above)"; fail=1; }
# The scrub reads text only; binaries must not name the home folder either (the site's verifier
# once carried it in its panic messages).
home_hits=$(grep -rlaF "$HOME" "$out" | sed "s|^$out/|  |" || true)
[ -z "$home_hits" ] || { echo "files holding the home folder path:"; echo "$home_hits"; fail=1; }
lists="$root/docs/ops/public-snapshot-private.txt"
[ "$mode" != --without-pending ] || lists="$lists $root/docs/ops/public-snapshot-exclude.txt"
# shellcheck disable=SC2086
python3 -I "$root/scripts/public-scrub.py" --unlink "$out" $lists
# The generated maps (README layout, scripts/README.md, docs/README.md) describe this tree, not
# the private one.
python3 -I "$out/scripts/check-readme.py" --write >/dev/null && python3 -I "$out/scripts/check-readme.py" \
  || { echo "the snapshot's maps do not regenerate"; fail=1; }
if [ "$plant" = 1 ]; then
  # Synthetic values built at run time, so this file itself holds neither.
  mkdir -p "$out/planted"
  printf 'contact: %s@%s\n' "jane.doe" "gmail.com" > "$out/planted/person.txt"
  printf 'token = %s%s\n' "ghp_" "$(head -c 27 /dev/urandom | base64 | tr -dc 'A-Za-z0-9' | head -c 36)" > "$out/planted/key.txt"
fi
gl="$(command -v "${ONYX_GITLEAKS:-gitleaks}" || true)"
if [ -n "$gl" ]; then
  # From inside OUT, so paths are relative and the reviewed allowlist (exact path AND value) applies.
  if (cd "$out" && "$gl" dir . --config "$root/.gitleaks.toml" --redact --no-banner --exit-code 1 \
      --report-format json --report-path "$out.gitleaks.json" >/dev/null 2>&1); then
    echo "gitleaks: no secret"
  else
    echo "gitleaks: FINDINGS (redacted report: $out.gitleaks.json)"; fail=1; leaks=1
  fi
else
  echo "gitleaks missing: secret scan not run"; fail=1
fi
# E-mail-like strings: only these reviewed kinds pass (2026-10-24 review): reserved test domains
# (RFC 2606 / RFC 6761: example, test, invalid, and the doc placeholder domain.tld), git remote URLs
# (git@host), image names (name@2x.png), noreply attribution, the synthetic intake vectors (a@b.co,
# x@y.io, user@ in a refused URL), and one organisation's published role contact cited in
# RESEARCH.md (legal@chain.link), and the agent client's refused credentials URL (https://u:p@h.org,
# apps/agent/test/client.test.mjs). Anything else, a person's address above all, fails.
allow='@([A-Za-z0-9-]+\.)*(example|test|invalid)(\.[A-Za-z]+)?\b|@domain\.tld\b|git@(github\.com|gitlab\.com|bitbucket\.org)|@[0-9]x\.png|noreply@|@users\.noreply\.github\.com|\ba@b\.co\b|\bx@y\.io\b|user@api\.fiscaldata\.treasury\.gov|\blegal@chain\.link\b|\bp@h\.org\b'
hits=$(grep -rInEo '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' "$out" --exclude='*.wasm' --exclude='*.lock' \
  | grep -vE "$allow" | cut -d: -f1,2 | sort -u || true)
if [ -n "$hits" ]; then
  echo "e-mail addresses outside the placeholder list (file:line, review each):"
  echo "$hits" | sed "s|^$out/|  |"
  fail=1
else
  echo "e-mail guard: none outside the placeholder list"
fi
if [ "$plant" = 1 ]; then
  if [ "$leaks" = 1 ] && [ -n "$hits" ]; then echo "SELF-TEST PASSED: the planted address and key were caught"; exit 0; fi
  echo "SELF-TEST FAILED: a planted value passed the checks"; exit 1
fi
[ "$fail" = 0 ] && echo "SNAPSHOT CHECKS PASSED (nothing was published)" || echo "SNAPSHOT CHECKS FAILED (nothing was published)"
exit "$fail"
