#!/usr/bin/env bash
# Secret scan of the tracked tree (THREATS T-KEY-1, L9 sweep). Fails when git tracks an env or key
# file, or when a tracked file holds a private key, a secret assignment or a mnemonic. It reads
# only tracked files (never .env or .onyx-testnet/) and prints file:line, never the matched value.
# --history runs the same patterns over every line ever added on any ref (`git log --all -p`) and
# over every file name ever added (ROADMAP L13, LC-25); it prints counts per pattern, and with
# --list commit:file:line per hit, never a value; merge commits are scanned too (what a merge adds
# beyond its automatic result, SCAN-4); then gitleaks with .gitleaks.toml (redacted, rule/commit:
# file:line only). It fails when gitleaks is missing, fails or writes no report (SCAN-5); the
# gitleaks binary can be named in ONYX_GITLEAKS. Run before any public push:
# docs/audit/2026-10-16-history-secret-scan.md.
#   scripts/check-secrets.sh                  scan this repository's tracked tree
#   scripts/check-secrets.sh --history [--list] scan all history (exit 1 on any hit)
#   scripts/check-secrets.sh --paths-from LIST  scan the NUL-separated paths in LIST (relative to
#                                             the current directory; scripts/remote-check.sh)
#   scripts/check-secrets.sh --self-test      plant each secret form in a temp repo; each must fail,
#                                             public placeholders must pass; a key committed and
#                                             then deleted, or added in a merge resolution, must
#                                             pass the tree scan and fail --history
set -euo pipefail

# Public, well-known test values that may appear in docs and tests: anvil's default account 0 key
# and the "test test ... junk" mnemonic (both published by Foundry). Compared in the script only.
# The key is account 0 of that mnemonic (`cast wallet private-key --mnemonic ... --mnemonic-index 0`);
# until 2026-10-15 one digit was mistyped, so the real anvil key was not allowlisted (SCAN-1).
ALLOW_RE='ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80|test test test test test test test test test test test junk'
# --history only: reviewed hits, by full commit:file:line (no value is written here). The one entry
# is the mistyped copy of anvil's key that the pre-SCAN-1 self-test line held: a typo of a public
# test key, not a credential; the L13 derivation check found no address in the repository for it.
# A new hit anywhere else, including the same value in another commit or file, still fails.
HISTORY_ALLOW_AT=(
  bc1733fbb2aeb3d31aafcbd08fef079482fc41f9:scripts/check-secrets.sh:68
)

# Tracked files that are env or key files (.env.example is the one allowed template).
FILE_RE='(^|/)\.env[^/]*$|[^/]+\.env$|(^|/)\.envrc$|\.key$|\.pem$|(^|/)\.onyx-testnet/'
HEX='(0x)?[0-9a-f]{64}'
Q="[\"'\`]?"
# One extended regex per form, matched case-insensitively.
PATTERNS=(
  # NAME_KEY=0x.., private_key = "..", "privateKey": "0x..", PK=0x.., SECRET: ..
  "(private[_-]?key|privkey|secret|_key|\\bpk)${Q}[[:space:]]*[=:][[:space:]]*${Q}${HEX}\\b"
  # cast/forge flags: --private-key 0x.., --private-key=0x..
  "--private-key[= ]+${Q}${HEX}\\b"
  # base64 or long API secrets (e.g. CDP_API_KEY_SECRET="...", apiKeySecret: "...")
  "(secret|api[_-]?key)[a-z_]*${Q}[[:space:]]*[=:][[:space:]]*${Q}[A-Za-z0-9+/_=-]{32,}"
  # mnemonics: mnemonic = "word word ... (12 to 24 words)"
  "(mnemonic|seed[_ -]?phrase)${Q}[[:space:]]*[=:][[:space:]]*${Q}([a-z]+[[:space:]]+){11,23}[a-z]+"
  # PEM private keys
  "-----BEGIN ([A-Z]+ )?PRIVATE KEY-----"
)
# --paths-from only (files about to leave this machine, THREATS REMOTE-1, REMOTE-6): credential
# files by name (ssh, cloud, package-registry and wallet files, key stores, which are often binary
# and so never content-scanned) and token forms by content, on top of FILE_RE and PATTERNS.
SYNC_FILE_RE='(^|/)(\.ssh|\.gnupg|\.aws|\.azure|\.kube|\.docker|secrets?|credentials?|wallets?|keystores?)/|(^|/)id_(rsa|dsa|ecdsa|ed25519)(_sk)?$|\.(p12|pfx|jks|keystore|kdbx|ppk|gpg|pgp|p8|ovpn|tfstate)$|(^|/)\.(npmrc|yarnrc\.yml|pypirc|netrc|git-credentials|pgpass|htpasswd|my\.cnf|s3cfg|dockercfg)$|(^|/)(credentials|secrets?)(\.(json|ya?ml|toml|txt|ini|cfg|conf))?$|(^|/)[^/]*(wallet|keystore)[^/]*\.json$|(^|/)UTC--'
SYNC_PATTERNS=(
  # GitHub, Slack, AWS, OpenAI or Anthropic style and npm tokens
  '\b(gh[pousr]_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{40,})'
  '\bxox[abposr]-[A-Za-z0-9-]{10,}'
  '\bAKIA[0-9A-Z]{16}\b'
  '\bsk-(ant-|proj-|live-)?[A-Za-z0-9_-]{32,}'
  '\bnpm_[A-Za-z0-9]{36}\b'
  # token, password and registry-auth values: a quoted literal, or a whole-line env assignment
  "(token|passw(or)?d|passwd|_auth)[\"'\`]?[[:space:]]*[=:][[:space:]]*[\"'\`][A-Za-z0-9+/_=.-]{16,}[\"'\`]"
  '^[[:space:]]*(export[[:space:]]+)?[a-z0-9_]*(token|passw(or)?d|passwd|_auth)[a-z0-9_]*=[A-Za-z0-9+/_=.-]{16,}[[:space:]]*$'
  # a URL holding a password (scheme://user:pass@host)
  "[a-z][a-z0-9+.-]*://[A-Za-z0-9._%~-]+:[^/[:space:]@\"'[]{6,}@"
  # PGP private key blocks (the K in brackets: this line never matches itself); JSON key stores
  '-----BEGIN PGP PRIVATE KEY BLOC[K]-----'
  '"crypto"[[:space:]]*:[[:space:]]*\{[^}]*"ciphertext"'
)

scan() { # run inside a git work tree
  local fail=0 files hits=""
  files=$(git ls-files | grep -iE "$FILE_RE" | grep -vE '(^|/)\.env\.example$' || true)
  if [ -n "$files" ]; then
    echo "SECRET FILE TRACKED (remove it from git and keep it ignored):"
    printf '  %s\n' $files
    fail=1
  fi
  for p in "${PATTERNS[@]}"; do
    # -o prints only the match, so placeholders are judged on the matched text; the value is
    # dropped before anything is printed (only file:line leaves this pipeline).
    hits+=$(git grep -nIioE -e "$p" | grep -viE "$ALLOW_RE" | cut -d: -f1,2 || true)
    hits+=$'\n'
  done
  hits=$(printf '%s' "$hits" | sed '/^$/d' | sort -u)
  if [ -n "$hits" ]; then
    echo "SECRET MATERIAL in tracked files (value not shown):"
    printf '  %s\n' $hits
    fail=1
  fi
  return "$fail"
}

# The same name and content rules over a NUL-separated list of paths relative to the current
# directory, tracked or not (scripts/remote-check.sh, THREATS REMOTE-1: what is about to leave this
# machine). Symlinks are judged by name only (their targets are never read). Prints path or
# path:line, never the matched value.
paths_scan() { # $1 = list file
  local fail=0 names hits="" files
  names=$(tr '\0' '\n' <"$1" | grep -iE "$FILE_RE|$SYNC_FILE_RE" | grep -vE '(^|/)\.env\.example$' || true)
  if [ -n "$names" ]; then
    echo "SECRET FILE in the list (env, key or credential file name):"
    printf '%s\n' "$names" | sed 's/^/  /'
    fail=1
  fi
  files=$(mktemp)
  while IFS= read -r -d '' f; do
    if [ -f "$f" ] && [ ! -L "$f" ]; then printf '%s\0' "$f"; fi
  done <"$1" >"$files"
  for p in "${PATTERNS[@]}" "${SYNC_PATTERNS[@]}"; do
    hits+=$(xargs -0 -r grep -HnIioE -e "$p" -- <"$files" | grep -viE "$ALLOW_RE" | cut -d: -f1,2 || true)
    hits+=$'\n'
  done
  rm -f "$files"
  hits=$(printf '%s' "$hits" | sed '/^$/d' | sort -u)
  if [ -n "$hits" ]; then
    echo "SECRET MATERIAL in the list (value not shown):"
    printf '%s\n' "$hits" | sed 's/^/  /'
    fail=1
  fi
  return "$fail"
}

# History scan. Every line added by any commit on any ref is a candidate (a removed line was added
# by an earlier commit, so added lines cover all content). A merge commit shows, with
# --remerge-diff, only what differs from git's own automatic merge (conflict resolutions and any
# extra edit, SCAN-4); the rest of its content was added by its parents' commits. The patch text
# goes to a private temp dir and is deleted on exit; only counts and commit:file:line leave this
# function.
history_scan() { # run inside a git work tree; $1 = 1 to list commit:file:line per hit
  local list=$1 tmp fail=0 i=0 n hits names total locs all
  tmp=$(mktemp -d)
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'; trap - RETURN" RETURN
  if ! git log --all -p -U0 --remerge-diff --no-color --no-ext-diff --no-renames --format='@@ONYX-COMMIT %H' |
    awk -v C="$tmp/content" -v M="$tmp/meta" '
      /^@@ONYX-COMMIT / { sha = $2; hdr = 0; next }
      /^diff --git /    { hdr = 1; file = "?"; ln = 0; next }
      hdr && /^\+\+\+ / { file = substr($0, 5); sub(/^b\//, "", file); next }
      /^@@ /            { hdr = 0; match($0, /\+[0-9]+/); ln = substr($0, RSTART + 1, RLENGTH - 1); next }
      hdr               { next }
      /^\+/             { print substr($0, 2) > C; print sha ":" file ":" ln > M; ln++ }'; then
    echo "history: git log failed, no result"
    return 1
  fi
  [ -f "$tmp/content" ] || : >"$tmp/content"
  [ -f "$tmp/meta" ] || : >"$tmp/meta"
  total=$(wc -l <"$tmp/content")
  names=$(git log --all --format= --name-only --diff-filter=AR | sort -u | grep -iE "$FILE_RE" |
    grep -vE '(^|/)\.env\.example$' || true)
  n=$(printf '%s' "$names" | grep -c . || true)
  echo "history: $(git rev-list --all | wc -l) commits (merges included), $total added lines"
  echo "  env/key file ever added: $n"
  [ "$list" = 1 ] && [ -n "$names" ] && printf '    %s\n' $names
  [ "$n" -gt 0 ] && fail=1
  for p in "${PATTERNS[@]}"; do
    i=$((i + 1))
    # Line numbers only leave grep; the matched value is dropped before anything is printed.
    hits=$(grep -nIioE -e "$p" "$tmp/content" | grep -viE "$ALLOW_RE" | cut -d: -f1 | sort -un || true)
    # Hit line numbers -> full-commit:file:line, minus the reviewed locations.
    locs=$(printf '%s\n' "$hits" | awk 'NR == FNR { if ($1 != "") want[$1] = 1; next } FNR in want' - "$tmp/meta")
    all=$(printf '%s' "$locs" | grep -c . || true)
    locs=$(printf '%s\n' "$locs" | grep -vxF -f <(printf '%s\n' "${HISTORY_ALLOW_AT[@]}") || true)
    n=$(printf '%s' "$locs" | grep -c . || true)
    echo "  pattern $i: $n$([ "$all" -gt "$n" ] && echo " (+$((all - n)) reviewed, HISTORY_ALLOW_AT)")"
    if [ "$n" -gt 0 ]; then
      fail=1
      [ "$list" = 1 ] && printf '%s\n' "$locs" | awk '{ print "    " substr($0, 1, 7) substr($0, 41) }'
    fi
  done
  return "$fail"
}

# gitleaks (free, open source; docs/audit/2026-10-16-history-secret-scan.md) over all refs, merges
# against their first parent (SCAN-4), with the reviewed allowlist $1 (empty: default rules only).
# `--redact` plus a private temp report whose template holds only rule, short commit, file and line.
# Fails closed: gitleaks missing, a non-zero gitleaks exit or no report is a failure (SCAN-5).
gitleaks_scan() { # run inside a git work tree
  local cfg=${1:-} bin=${ONYX_GITLEAKS:-gitleaks} gl n rc=0
  if ! command -v "$bin" >/dev/null; then
    echo "gitleaks: NOT INSTALLED, so the history scan is incomplete (see docs/audit/2026-10-16-history-secret-scan.md)"
    return 1
  fi
  gl=$(mktemp -d)
  printf '%s\n' '{{ range . }}{{ .RuleID }} {{ printf "%.7s" .Commit }}:{{ .File }}:{{ .StartLine }}' '{{ end }}' >"$gl/t.tmpl"
  if ! "$bin" git --no-banner --log-level error --log-opts="--all --diff-merges=first-parent" --redact \
    ${cfg:+--config "$cfg"} --report-format template --report-template "$gl/t.tmpl" \
    --report-path "$gl/r.txt" --exit-code 0 . >/dev/null; then
    echo "gitleaks: FAILED (non-zero exit), no result"
    rm -rf "$gl"
    return 1
  fi
  if [ ! -f "$gl/r.txt" ]; then
    echo "gitleaks: wrote no report, no result"
    rm -rf "$gl"
    return 1
  fi
  n=$(grep -c . "$gl/r.txt" || true)
  echo "gitleaks $("$bin" version) (${cfg:-default rules}, all refs and merges): $n"
  if [ "$n" -gt 0 ]; then rc=1; sed '/^$/d; s/^/    /' "$gl/r.txt"; fi
  rm -rf "$gl"
  return "$rc"
}

if [ "${1:-}" = --self-test ]; then
  # The allowlisted anvil key must be the published one (SCAN-1); checked when Foundry is present.
  cast=$(command -v cast || echo "$HOME/.foundry/bin/cast")
  if [ -x "$cast" ]; then
    anvil0=$("$cast" wallet private-key --mnemonic 'test test test test test test test test test test test junk')
    case "$ALLOW_RE" in "${anvil0#0x}|"*) ;; *) echo "self-test: ALLOW_RE does not start with anvil account 0's key"; exit 1 ;; esac
  fi
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  fake=$(printf 'onyx-check-secrets-self-test' | sha256sum | cut -d' ' -f1)
  b64=$(printf 'onyx-check-secrets-self-test-base64-material' | base64 -w0)
  words='abandon ability able about above absent absorb abstract absurd abuse access accident'
  (
    cd "$tmp"
    git init -q
    printf 'ok\n' >README
    # Public placeholders and look-alikes must pass.
    {
      echo "anvil key: --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
      echo 'MNEMONIC="test test test test test test test test test test test junk"'
      echo "BASE_SEPOLIA_PRIVATE_KEY="
      echo "codeHash: 0x$fake"
      echo "pk = bound(pk, 1, 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364140);"
    } >docs.md
    git add README docs.md
    scan >/dev/null || { echo "self-test: a clean tree with placeholders failed"; exit 1; }
    planted=(
      "DEPLOYER_KEY=0x$fake"
      "cast send --private-key 0x$fake"
      "forge script --private-key=$fake"
      "{\"privateKey\": \"0x$fake\"}"
      "private_key = \"$fake\""
      "PRIVATE_KEY = \`0x$fake\`"
      "PK=0x$fake"
      "CDP_API_KEY_SECRET=\"$b64\""
      "mnemonic = \"$words\""
      "-----BEGIN EC PRIV""ATE KEY-----"
    )
    for line in "${planted[@]}"; do
      printf '%s\n' "$line" >notes.txt
      git add notes.txt
      if scan >/dev/null; then echo "self-test: planted form ${line%%[=:\ ]*} passed"; exit 1; fi
      git rm -q --cached notes.txt
    done
    for f in .env .env.local deploy.env .envrc keys/actor.key; do
      mkdir -p "$(dirname "$f")"
      echo 'X=1' >"$f"
      git add -f "$f"
      if scan >/dev/null; then echo "self-test: a tracked $f passed"; exit 1; fi
      git rm -q --cached "$f"
    done
    echo 'X=' >.env.example
    git add -f .env.example
    scan >/dev/null || { echo "self-test: .env.example or the cleaned tree failed"; exit 1; }
    git -c user.name=t -c user.email=t@t commit -qm clean
    history_scan 0 >/dev/null || { echo "self-test: --history failed on a clean history"; exit 1; }
    # A key committed and deleted later: the tree scan passes, the history scan must fail.
    echo "DEPLOYER_KEY=0x$fake" >old.txt
    git add old.txt
    git -c user.name=t -c user.email=t@t commit -qm planted
    git rm -q old.txt
    git -c user.name=t -c user.email=t@t commit -qm removed
    scan >/dev/null || { echo "self-test: the tree scan failed after the key was deleted"; exit 1; }
    if history_scan 0 >/dev/null; then echo "self-test: --history missed a deleted key"; exit 1; fi
    out=$(history_scan 1 || true)
    grep -q ':old.txt:1$' <<<"$out" || { echo "self-test: --list did not name commit:old.txt:1"; exit 1; }
    if grep -q "$fake" <<<"$out"; then echo "self-test: --history printed the value"; exit 1; fi
    # A key added only in a merge commit's conflict resolution (SCAN-4).
    git checkout -q -b side
    echo b >m.txt
    git add m.txt
    git -c user.name=t -c user.email=t@t commit -qm side
    git checkout -q -
    echo a >m.txt
    git add m.txt
    git -c user.name=t -c user.email=t@t commit -qm main
    git -c user.name=t -c user.email=t@t merge -q side >/dev/null 2>&1 || true
    printf 'a\nPRIVATE_KEY=0x%s\n' "$fake" >m.txt
    git add m.txt
    git -c user.name=t -c user.email=t@t commit -qm merge
    git rm -q m.txt
    git -c user.name=t -c user.email=t@t commit -qm removed-m
    merge=$(git rev-parse --short=7 HEAD~1)
    out=$(history_scan 1 || true)
    grep -qE "^    $merge:m\.txt:[0-9]+$" <<<"$out" || { echo "self-test: --history missed a key added in a merge"; exit 1; }
    if grep -q "$fake" <<<"$out"; then echo "self-test: --history printed the value"; exit 1; fi
    # gitleaks must fail closed when missing, failing or silent (SCAN-5).
    printf '#!/bin/sh\nexit 2\n' >.git/gl-crash
    printf '#!/bin/sh\nexit 0\n' >.git/gl-silent
    chmod +x .git/gl-crash .git/gl-silent
    for g in "$tmp/.git/gl-missing" "$tmp/.git/gl-crash" "$tmp/.git/gl-silent"; do
      if ONYX_GITLEAKS=$g gitleaks_scan >/dev/null; then echo "self-test: gitleaks_scan passed with ${g##*/}"; exit 1; fi
    done
    gl_note="gitleaks not installed, its live check skipped"
    if command -v gitleaks >/dev/null; then
      if out=$(gitleaks_scan 2>&1); then echo "self-test: gitleaks missed the planted keys"; exit 1; fi
      grep -q "^    [a-z-]* $merge:m\.txt:" <<<"$out" || { echo "self-test: gitleaks missed a key added in a merge"; exit 1; }
      if grep -q "$fake" <<<"$out"; then echo "self-test: gitleaks output printed the value"; exit 1; fi
      gl_note="gitleaks finds the merge key"
    fi
    # --paths-from (REMOTE-1): an untracked planted key and an env file name fail; placeholders
    # pass; a symlink to a planted file is judged by its name only.
    echo "DEPLOYER_KEY=0x$fake" >untracked.txt
    printf '%s\0' docs.md untracked.txt >list
    if paths_scan list >/dev/null; then echo "self-test: --paths-from missed an untracked key"; exit 1; fi
    if paths_scan list | grep -q "$fake"; then echo "self-test: --paths-from printed the value"; exit 1; fi
    printf '%s\0' docs.md .env >list
    if paths_scan list >/dev/null; then echo "self-test: --paths-from passed a .env"; exit 1; fi
    ln -s untracked.txt link.txt
    printf '%s\0' docs.md .env.example link.txt >list
    paths_scan list >/dev/null || { echo "self-test: --paths-from failed placeholders or a symlink"; exit 1; }
    echo "secrets self-test ok: ${#planted[@]} planted forms and 5 tracked env/key files fail, placeholders pass, a deleted key and a merge-only key fail --history, gitleaks fails closed ($gl_note), --paths-from fails an untracked key and a .env"
  ) || exit 1
  exit 0
fi

if [ "${1:-}" = --paths-from ]; then
  [ -f "${2:-}" ] || { echo "usage: scripts/check-secrets.sh --paths-from <NUL-separated list>" >&2; exit 2; }
  paths_scan "$2" || exit 1
  echo "secrets ok: $(tr -cd '\0' <"$2" | wc -c) listed paths scanned"
  exit 0
fi
cd "$(dirname "$0")/.."
if [ "${1:-}" = --history ]; then
  rc=0
  history_scan "$([ "${2:-}" = --list ] && echo 1 || echo 0)" || rc=1
  gitleaks_scan .gitleaks.toml || rc=1
  exit "$rc"
fi
scan
echo "secrets ok: $(git ls-files | wc -l) tracked files scanned"
