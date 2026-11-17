#!/usr/bin/env bash
# Rebuild a lost deploy export (the <export dir> of scripts/deploy-api.sh) from a snapshot of the
# node backup (scripts/backup-node.sh; docs/ops/deployment.md "Folders that must not be deleted").
#
# Usage: scripts/export-from-backup.sh SNAPSHOT OUT
#        scripts/export-from-backup.sh --self-test   (offline: a fake snapshot, no server)
#   SNAPSHOT  one snapshot folder of the backup (it holds etc-onyx/ and cas/); only read
#   OUT       the new export folder; must not exist (made 0700, complete or not at all)
#
# What it writes, the reverse of what deploy-api.sh installs on the server:
#   deployment.json, catalog.json   the snapshot's etc-onyx/ copies
#   cas/                            the snapshot's row batches
#   observations/*.json and pins    the level-2 stores, and their pins taken from the
#                                   ONYX_OBSERVATION_TEMPLATES line of the server's node.env
#   track/publication.borsh         the track-record publication, when the snapshot has one
#   publishers.json                 the limits per publisher (ADR-0066 item 10), when the server
#                                   had them: a redeploy from the rebuilt export keeps them
#   node.service.env                the server's node.env without the lines deploy-api.sh writes for
#                                   the host (listen address, public URL, node URL, proxy, health,
#                                   intake journal, bulk store, level-2 and track paths). The
#                                   payment mode is kept as the server had it: an export deployed
#                                   with payments off has lost its own mode (the script says so).
# Not in any backup, so not rebuilt: files other scripts keep beside these and never send to the
# server (markets.json, insurance.json), and every key.
#
# Then it checks the result by staging: deploy-api.sh's own local lines (its checks of the export
# and the files it puts in its staging folder; nothing after that) run on OUT, and every setting
# the export carries, every level-2 store and the publication must come out as the snapshot has
# them. Those lines are refused when they name ssh, scp, rsync, sudo, systemctl or curl: nothing
# here reaches a server. A host setting this commit's deploy-api.sh would write differently from
# the snapshot (a changed limit) is named in a NOTE line, not a failure.
# No file content is printed (node.env may be sensitive): setting names and counts only.
set -euo pipefail
self=$(realpath "$0")
repo=$(dirname "$(dirname "$self")")
# Test hook (the self-test only): another file standing in for deploy-api.sh.
deploy_script=${ONYX_EXPORT_DEPLOY_SCRIPT:-$repo/scripts/deploy-api.sh}

die() { echo "export-from-backup: $*" >&2; exit 1; }

# The settings deploy-api.sh writes itself: its own filter, plus the intake journal it appends.
host_keys() {
  local pat
  pat=$(grep -oE "^grep -vE '[^']+'" "$deploy_script" | sed -e "s/^grep -vE '//" -e "s/'\$//" || true)
  [ -n "$pat" ] && [ "$(wc -l <<<"$pat")" = 1 ] || die "cannot find the settings filter in $(basename "$deploy_script")"
  printf '%s\n' "$pat"
}

build() {
  local snap=$1 out=$2 etc tmp pat n=0 f
  etc=$snap/etc-onyx
  for f in node.env deployment.json catalog.json; do
    [ -s "$etc/$f" ] || die "the snapshot has no etc-onyx/$f"
  done
  [ -d "$snap/cas" ] || die "the snapshot has no cas/"
  [ ! -e "$out" ] || die "$out exists: name a new folder (nothing is replaced)"
  [ -d "$(dirname "$out")" ] || die "the folder for $out does not exist"
  pat=$(host_keys)
  tmp=$(mktemp -d "$out.partial.XXXXXX")
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'" EXIT
  chmod 700 "$tmp"
  cp "$etc/deployment.json" "$etc/catalog.json" "$tmp/"
  cp -r "$snap/cas" "$tmp/cas"
  (umask 077 && { grep -vE "$pat" "$etc/node.env" | grep -v -e '^ONYX_SUBMISSIONS=' -e '^ONYX_PUBLISHER_POINTERS=' || true; } >"$tmp/node.service.env")
  # Kept from the server, so the export has a mode at all.
  (umask 077 && { grep '^ONYX_PAYMENTS=' "$etc/node.env" || true; } >>"$tmp/node.service.env")
  for f in "$etc"/observations/*.json; do
    [ -e "$f" ] || continue
    mkdir -p "$tmp/observations"
    cp "$f" "$tmp/observations/"
    n=$((n + 1))
  done
  if [ "$n" -gt 0 ]; then
    sed -n 's/^ONYX_OBSERVATION_TEMPLATES=//p' "$etc/node.env" | tr ',' '\n' | sed '/^$/d' >"$tmp/observations/pins"
    [ -s "$tmp/observations/pins" ] || die "the snapshot has level-2 stores but its node.env names no pin"
  fi
  if [ -s "$etc/track/publication.borsh" ]; then
    mkdir "$tmp/track"
    cp "$etc/track/publication.borsh" "$tmp/track/"
  fi
  # Limits per publisher (ADR-0066 item 10): the server's file, so a redeploy keeps the limits.
  if grep -q '^ONYX_BULK_PUBLISHERS=' "$etc/node.env"; then
    [ -s "$etc/publishers.json" ] || die "the snapshot's node.env sets ONYX_BULK_PUBLISHERS but it has no etc-onyx/publishers.json"
    cp "$etc/publishers.json" "$tmp/publishers.json"
  fi
  chmod -R go-rwx "$tmp"
  verify "$snap" "$tmp"
  mv -T "$tmp" "$out"
  trap - EXIT
  echo "EXPORT REBUILT: $out ($(find "$out/cas" -type f | wc -l) row files, $n level-2 stores, track $([ -d "$out/track" ] && echo yes || echo no), limits per publisher $([ -f "$out/publishers.json" ] && echo yes || echo no)); checked by staging"
  if grep -qx 'ONYX_PAYMENTS=off' "$out/node.service.env"; then
    echo "NOTE: the server ran with payments off, so the export's own payment mode is not in the backup (it matters only for a deploy with ONYX_API_PAYMENTS=1)"
  fi
}

# deploy-api.sh's local lines on the export, then the comparison with the snapshot.
verify() {
  local snap=$1 exp=$2 first last lines work rc=0
  first=$(grep -n '^for f in node.service.env ' "$deploy_script" | cut -d: -f1 || true)
  last=$(grep -n '^remote=' "$deploy_script" | cut -d: -f1 || true)
  [[ $first =~ ^[0-9]+$ ]] && [[ $last =~ ^[0-9]+$ ]] && [ "$first" -lt "$last" ] \
    || die "cannot find the staging lines in $(basename "$deploy_script") (from its export checks to 'remote=')"
  lines=$(sed -n "${first},$((last - 1))p" "$deploy_script")
  grep -q '^stage=\$(mktemp -d ' <<<"$lines" || die "the staging lines of $(basename "$deploy_script") make no staging folder"
  # Not a pipe into grep -q: it exits at the first match, and once the filtered lines pass 4096
  # bytes (the writer's buffer) the writer's next write dies of SIGPIPE, which under pipefail reads
  # as "no match": the refusal was skipped in some runs and the lines ran.
  local code
  code=$(grep -vE '^[[:space:]]*#' <<<"$lines" || true)
  if grep -qwE 'ssh|scp|rsync|sudo|systemctl|curl' <<<"$code"; then
    die "the staging lines of $(basename "$deploy_script") reach a server (ssh, scp, rsync, sudo, systemctl or curl): refused, nothing was run"
  fi
  work=$(mktemp -d "${TMPDIR:-/tmp}/export-from-backup.XXXXXX")
  : >"$work/onyx-node" && chmod 755 "$work/onyx-node"
  {
    echo 'set -euo pipefail'
    echo 'export_dir=$1 bin=$2 snap=$3'
    echo 'host=none domain=${ONYX_SITE_DOMAIN:-onyx.example}'
    printf '%s\n' "$lines"
    cat <<'TAIL'
python3 -I - "$snap/etc-onyx" "$stage" <<'PY'
import filecmp, os, sys
etc, stage = sys.argv[1:3]
def settings(path):
    out = {}
    for line in open(path):
        line = line.rstrip("\n")
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            out[k] = v
    return out
live, staged = settings(os.path.join(etc, "node.env")), settings(os.path.join(stage, "node.env"))
exported = settings(os.path.join(os.environ["EXPORT_DIR"], "node.service.env"))
bad = sorted(k for k in exported if k != "ONYX_PAYMENTS" and staged.get(k) != live.get(k))
bad += sorted(k for k in live if k not in staged)
if bad:
    sys.exit("staging the export does not give the snapshot's settings: " + ", ".join(sorted(set(bad))))
for sub in ("observations", "track"):
    a, b = os.path.join(etc, sub), os.path.join(stage, sub)
    names = sorted(n for n in (os.listdir(a) if os.path.isdir(a) else []) if sub == "track" or n.endswith(".json"))
    if names != sorted(os.listdir(b) if os.path.isdir(b) else []):
        sys.exit(f"staging the export does not give the snapshot's {sub}/ files")
    if any(not filecmp.cmp(os.path.join(a, n), os.path.join(b, n), shallow=False) for n in names):
        sys.exit(f"a staged {sub}/ file differs from the snapshot's")
a, b = os.path.join(etc, "publishers.json"), os.path.join(stage, "publishers.json")
if "ONYX_BULK_PUBLISHERS" in live and not (os.path.isfile(b) and filecmp.cmp(a, b, shallow=False)):
    sys.exit("staging the export does not give the snapshot's publishers.json")
moved = sorted(k for k in staged if k not in exported and staged[k] != live.get(k))
moved += sorted(k for k in staged if k not in live)
if moved:
    print("NOTE: this commit's deploy-api.sh would set differently from the snapshot: " + ", ".join(sorted(set(moved))))
PY
TAIL
  } >"$work/stage.sh"
  # The switches deploy-api.sh reads, set to what the server's node.env shows was deployed.
  local e=$snap/etc-onyx/node.env
  (cd "$repo" && env -u ONYX_SITE_HOST EXPORT_DIR="$exp" ONYX_API_MCP=0 ONYX_API_CONN_CAP=0 \
    ONYX_API_PAYMENTS="$(grep -qx 'ONYX_PAYMENTS=off' "$e" && echo 0 || echo 1)" \
    ONYX_API_INTAKE="$(grep -q '^ONYX_SUBMISSIONS=' "$e" && echo 1 || echo 0)" \
    ONYX_API_POINTERS="$(grep -q '^ONYX_PUBLISHER_POINTERS=' "$e" && echo 1 || echo 0)" \
    ONYX_API_BULK="$(grep -q '^ONYX_BULK_DIR=' "$e" && echo 1 || echo 0)" \
    ONYX_API_BULK_FORMAT2="$(grep -qx 'ONYX_BULK_FORMAT2=on' "$e" && echo 1 || echo 0)" \
    bash "$work/stage.sh" "$exp" "$work/onyx-node" "$snap") || rc=$?
  rm -rf "$work"
  [ "$rc" = 0 ] || die "the rebuilt export did not pass the staging check: nothing was written"
}

self_test() {
  local t pass=0 failed=0 out rc mark s
  t=$(mktemp -d "${TMPDIR:-/tmp}/export-from-backup-test.XXXXXX")
  # shellcheck disable=SC2064
  trap "rm -rf '$t'" EXIT
  ok() { echo "ok: $1"; pass=$((pass + 1)); }
  bad() { echo "FAIL: $1"; failed=$((failed + 1)); }
  check() { if eval "$2"; then ok "$1"; else bad "$1"; fi; }
  run() { rc=0; out=$("$self" "$@" 2>&1) || rc=$?; }
  mark="planted-$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')"
  # A home folder with nothing in it, as on a fresh CI runner: the staging lines make their own
  # ~/.cache (they failed there with no ~/.cache, and every case after the first with them).
  mkdir "$t/home"
  export HOME="$t/home"
  snapshot() { # DIR: a snapshot as backup-node.sh leaves it, as deploy-api.sh would have installed
    s=$1
    mkdir -p "$s/etc-onyx/observations" "$s/etc-onyx/track" "$s/etc-onyx/storage" "$s/cas/rows" "$s/bulk" "$s/state" "$s/server"
    echo '{"registry":"0x01","signedBook":"0x02"}' >"$s/etc-onyx/deployment.json"
    echo '{"pricing":{}}' >"$s/etc-onyx/catalog.json"
    echo '{"rounds":[1]}' >"$s/etc-onyx/observations/a.json"
    echo '{"rounds":[2]}' >"$s/etc-onyx/observations/b.json"
    echo publication >"$s/etc-onyx/track/publication.borsh"
    echo '{"reserve_gib": 2, "publishers": [{"id": "first.publisher", "quota_gib": 16, "datasets": 50000, "state": "open"}, {"id": "second.publisher", "quota_gib": 1, "datasets": 16, "state": "paused"}]}' >"$s/etc-onyx/publishers.json"
    echo "n1 $mark" >"$s/etc-onyx/storage/n1.json"
    echo rows1 >"$s/cas/rows/r1" && echo rows2 >"$s/cas/rows/r2"
    cat >"$s/etc-onyx/node.env" <<EOF
ONYX_ENV=testnet
ONYX_RIGHTS=fallback
ONYX_CATALOG=/etc/onyx/catalog.json
ONYX_SALT_NOTE=$mark
ONYX_LISTEN=127.0.0.1:8080
ONYX_PUBLIC_URL=https://onyx.example
ONYX_RPC_URL=https://sepolia.base.org
ONYX_TRUSTED_PROXIES=127.0.0.1
ONYX_INDEX_HEALTH=summary
ONYX_PAYMENTS=off
ONYX_SUBMISSIONS=/var/lib/onyx/state/submissions.journal
ONYX_BULK_DIR=/var/lib/onyx/bulk
ONYX_BULK_MAX_GIB=1
ONYX_BULK_QUOTA_GIB=1
ONYX_BULK_MAX_DATASETS=5
ONYX_BULK_PUBLISHERS=/etc/onyx/publishers.json
ONYX_OBSERVATIONS=/etc/onyx/observations/a.json,/etc/onyx/observations/b.json
ONYX_OBSERVATION_TEMPLATES=m.a=0x0a,m.b=0x0b
ONYX_TRACK_PUBLICATIONS=/etc/onyx/track/publication.borsh
EOF
  }
  snapshot "$t/snap"
  find "$t/snap" -type f -exec sha256sum {} + | sort >"$t/before"

  run "$t/snap" "$t/out"
  check "an export is rebuilt from a snapshot and passes the staging check" '[ "$rc" = 0 ] && [[ $out == *"EXPORT REBUILT: $t/out (2 row files, 2 level-2 stores, track yes, limits per publisher yes); checked by staging"* ]]'
  check "the limits per publisher are the server's file (ADR-0066 item 10)" 'cmp -s "$t/out/publishers.json" "$t/snap/etc-onyx/publishers.json" && [ "$(stat -c %a "$t/out/publishers.json")" = 600 ]'
  check "it holds what deploy-api.sh asks of an export" '[ -s "$t/out/node.service.env" ] && [ -s "$t/out/deployment.json" ] && [ -s "$t/out/catalog.json" ] && cmp -s "$t/out/cas/rows/r2" "$t/snap/cas/rows/r2" && cmp -s "$t/out/observations/b.json" "$t/snap/etc-onyx/observations/b.json" && cmp -s "$t/out/track/publication.borsh" "$t/snap/etc-onyx/track/publication.borsh"'
  check "the pins are the server's, one per line" '[ "$(cat "$t/out/observations/pins")" = "m.a=0x0a
m.b=0x0b" ]'
  check "node.service.env keeps the export's settings and no host line" '[ "$(sort "$t/out/node.service.env" | cut -d= -f1 | paste -sd,)" = "ONYX_CATALOG,ONYX_ENV,ONYX_PAYMENTS,ONYX_RIGHTS,ONYX_SALT_NOTE" ]'
  check "the folder is 0700, node.service.env 0600, nothing beside the export (no storage/, bulk/ or state/)" '[ "$(stat -c %a "$t/out")" = 700 ] && [ "$(stat -c %a "$t/out/node.service.env")" = 600 ] && [ "$(ls "$t/out" | paste -sd,)" = "cas,catalog.json,deployment.json,node.service.env,observations,publishers.json,track" ]'
  check "a host setting this deploy-api.sh writes differently is a NOTE with names only" '[[ $out == *"NOTE: this commit'"'"'s deploy-api.sh would set differently from the snapshot: ONYX_BULK_MAX_DATASETS, ONYX_BULK_MAX_GIB, ONYX_BULK_QUOTA_GIB"* ]]'
  check "payments off on the server is said" '[[ $out == *"NOTE: the server ran with payments off"* ]]'
  check "no setting value is printed" '[[ $out != *"$mark"* ]]'
  check "the snapshot is unchanged" 'find "$t/snap" -type f -exec sha256sum {} + | sort | cmp -s - "$t/before"'

  run "$t/snap" "$t/out"
  check "an existing OUT is refused" '[ "$rc" = 1 ] && [[ $out == *"exists"* ]] && [ -s "$t/out/node.service.env" ]'

  snapshot "$t/plain"
  rm -r "$t/plain/etc-onyx/observations" "$t/plain/etc-onyx/track" "$t/plain/etc-onyx/publishers.json"
  sed -i -e '/^ONYX_OBSERVATION/d' -e '/^ONYX_TRACK/d' -e '/^ONYX_SUBMISSIONS/d' -e '/^ONYX_BULK/d' -e 's/^ONYX_PAYMENTS=off/ONYX_PAYMENTS=x402/' "$t/plain/etc-onyx/node.env"
  run "$t/plain" "$t/out-plain"
  check "a snapshot with no level-2 store, track, intake or bulk store gives an export without them; the payment mode is kept" '[ "$rc" = 0 ] && [ ! -e "$t/out-plain/observations" ] && [ ! -e "$t/out-plain/track" ] && [ ! -e "$t/out-plain/publishers.json" ] && [[ $out == *"limits per publisher no"* ]] && grep -qx ONYX_PAYMENTS=x402 "$t/out-plain/node.service.env" && [[ $out != *NOTE* ]]'

  snapshot "$t/nonet"
  sed -i 's/^ONYX_ENV=testnet/ONYX_ENV=mainnet/' "$t/nonet/etc-onyx/node.env"
  run "$t/nonet" "$t/out-nonet"
  check "a snapshot that is not a testnet node fails the staging check and leaves nothing" '[ "$rc" = 1 ] && [[ $out == *"not a testnet export"* ]] && [ -z "$(ls "$t" | grep "^out-nonet" || true)" ]'

  snapshot "$t/nopin"
  sed -i '/^ONYX_OBSERVATION_TEMPLATES/d' "$t/nopin/etc-onyx/node.env"
  run "$t/nopin" "$t/out-nopin"
  check "level-2 stores without a pin are refused" '[ "$rc" = 1 ] && [[ $out == *"names no pin"* ]] && [ ! -e "$t/out-nopin" ]'

  snapshot "$t/nolimits"
  rm "$t/nolimits/etc-onyx/publishers.json"
  run "$t/nolimits" "$t/out-nolimits"
  check "a snapshot whose node.env sets limits per publisher but lacks the file is refused" '[ "$rc" = 1 ] && [[ $out == *"no etc-onyx/publishers.json"* ]] && [ ! -e "$t/out-nolimits" ]'
  snapshot "$t/unlisted"
  sed -i '/^ONYX_BULK_PUBLISHERS=/d' "$t/unlisted/etc-onyx/node.env"
  run "$t/unlisted" "$t/out-unlisted"
  check "a limits file the server's node.env does not name is not carried: the export deploys as the server ran" '[ "$rc" = 0 ] && [ ! -e "$t/out-unlisted/publishers.json" ] && [[ $out == *"limits per publisher no"* ]]'
  snapshot "$t/overgrant"
  sed -i 's/"reserve_gib": 2/"reserve_gib": 20/' "$t/overgrant/etc-onyx/publishers.json"
  run "$t/overgrant" "$t/out-overgrant"
  check "a limits file this deploy-api.sh would refuse fails the staging check and leaves nothing" '[ "$rc" = 1 ] && [[ $out == *"exceed the node"* ]] && [ ! -e "$t/out-overgrant" ]'

  snapshot "$t/nocas"
  rm -r "$t/nocas/cas"
  run "$t/nocas" "$t/out-nocas"
  check "a snapshot without cas/ is refused" '[ "$rc" = 1 ] && [[ $out == *"no cas/"* ]] && [ ! -e "$t/out-nocas" ]'
  rm "$t/nopin/etc-onyx/node.env"
  run "$t/nopin" "$t/out-noenv"
  check "a snapshot without node.env is refused" '[ "$rc" = 1 ] && [[ $out == *"etc-onyx/node.env"* ]] && [ ! -e "$t/out-noenv" ]'

  # A deploy-api.sh whose staging lines would reach a server: refused before anything runs.
  local word
  for word in ssh rsync sudo systemctl; do
    sed "s|^chmod 600 \"\$stage/node.env\"|$word onyx-web touch $t/ran-$word; &|" "$repo/scripts/deploy-api.sh" >"$t/deploy-$word.sh"
    grep -q "^$word onyx-web" "$t/deploy-$word.sh" || bad "the self-test could not plant $word"
    rc=0; out=$(ONYX_EXPORT_DEPLOY_SCRIPT="$t/deploy-$word.sh" "$self" "$t/snap" "$t/out-$word" 2>&1) || rc=$?
    check "staging lines that name $word are refused and never run" '[ "$rc" = 1 ] && [[ $out == *"reach a server"* ]] && [ ! -e "$t/ran-$word" ] && [ ! -e "$t/out-$word" ]'
  done
  # The same with about 1 MiB of staging lines after the word, more than a pipe holds (the refusal
  # once read them through a pipe into grep -q and was skipped when the writer was cut off).
  awk -v w="systemctl onyx-web touch $t/ran-long" '/^stage=\$\(mktemp -d /{print w; for (i = 0; i < 20000; i++) print ": padding line " i " of the staging block"} {print}' "$repo/scripts/deploy-api.sh" >"$t/deploy-long.sh"
  grep -q "^systemctl onyx-web" "$t/deploy-long.sh" || bad "the self-test could not plant the long case"
  local n ok_long=0
  for n in 1 2 3 4 5; do
    rc=0; out=$(ONYX_EXPORT_DEPLOY_SCRIPT="$t/deploy-long.sh" "$self" "$t/snap" "$t/out-long" 2>&1) || rc=$?
    if [ "$rc" = 1 ] && [[ $out == *"reach a server"* ]] && [ ! -e "$t/ran-long" ] && [ ! -e "$t/out-long" ]; then ok_long=$((ok_long + 1)); fi
  done
  check "long staging lines that name systemctl are refused in each of 5 runs" '[ "$ok_long" = 5 ]'
  sed '/^remote=/d' "$repo/scripts/deploy-api.sh" >"$t/deploy-nomark.sh"
  rc=0; out=$(ONYX_EXPORT_DEPLOY_SCRIPT="$t/deploy-nomark.sh" "$self" "$t/snap" "$t/out-nomark" 2>&1) || rc=$?
  check "a deploy-api.sh without the expected staging lines is refused" '[ "$rc" = 1 ] && [[ $out == *"cannot find the staging lines"* ]] && [ ! -e "$t/out-nomark" ]'
  check "no run left a partial folder" '[ -z "$(find "$t" -maxdepth 1 -name "*.partial.*")" ]'

  echo "export-from-backup self-test: $pass passed, $failed failed"
  [ "$failed" = 0 ]
}

case "${1:-}" in
--self-test) [ $# = 1 ] || die "--self-test takes no argument"; self_test ;;
"" | -*) echo "usage: $0 SNAPSHOT OUT | --self-test" >&2; exit 2 ;;
*)
  [ $# = 2 ] || { echo "usage: $0 SNAPSHOT OUT | --self-test" >&2; exit 2; }
  [ -d "$1" ] || die "no snapshot folder $1"
  build "$(realpath "$1")" "$(realpath -m "$2")"
  ;;
esac
