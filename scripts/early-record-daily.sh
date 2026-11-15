#!/usr/bin/env bash
# ROADMAP S26 slice 3 (ADR-0065 item 12, THREATS EARLY): the early-contributor record's daily run, as the
# systemd user timer deploy/systemd/user/onyx-early-record.timer starts it after 00:30 UTC. Runbook:
# docs/ops/early-record.md "The daily run". Test: scripts/early-record-daily-tests.py (stub tools).
#
#   scripts/early-record-daily.sh [--dry-run]
#
# 1. `early-record build` (node ONYX_EARLY_RPC) of every complete day not built yet, one folder a day:
#    OUT/versions/YYYY-MM-DD (staged in OUT/versions/next, then renamed to its day).
# 2. Then each day not published yet, in day order, all the way before the next day starts (so a
#    day is never left anchored and not uploaded when a later day stops the run):
#    a. scripts/audit-privacy-scan.py on the version folder: a hit publishes neither this day nor a
#       later one (exit 4);
#    b. `onyx-publish build` of the day's CSV (and of `exclusions/` on a day it changes) with
#       `--prev`, into the same folder, under the record's own publisher id, created at the end of
#       the day;
#    c. `early-record verify` against a SECOND node, ONYX_EARLY_RPC2 (from the state saved before
#       the day): the CSV bytes, day.json with the hash of the join's state after the day (so a log
#       node 1 left out whose row comes later shows on its own day), and both versions' shard roots.
#       A difference publishes neither this day nor a later one (exit 3);
#    d. one catalog a day (docs/ops/bulk-publish.md "Many datasets with one anchor"): `catalog-build`
#       (`--prev` the day before's) into OUT/catalogs/YYYY-MM-DD.bin. --dry-run stops here for the
#       day: nothing is signed, no key is used, no transaction is sent;
#    e. `catalog-sign`, `catalog-anchor` (one postReport a day), `catalog-upload`, then
#       `upload --catalog N` of the day's folders (waiting up to ONYX_EARLY_WAIT_SECS for the
#       anchor to be final).
# Each step is skipped when done (a marker per day and step, the folders and catalogs kept): a
# rerun after a failure goes on where it stopped, and never builds a version or a catalog again.
#
# Config (environment only; nothing here names a host, a key or a path):
#   ONYX_EARLY_RPC, ONYX_EARLY_RPC2  two chain nodes of different providers: their scheme://host:port
#                          must differ (they may hold provider keys: passed to early-record in its
#                          environment only, never printed)
#   ONYX_EARLY_STATE       the early-record state directory (only early-record writes it)
#   ONYX_EARLY_OUT         the output root, an existing folder (versions/, catalogs/, done/, lock)
#   ONYX_EARLY_PUBLISHER   the record's own publisher id (early-record), never a data publisher's
#   ONYX_EARLY_INDEXER     the entity indexer checkout whose lists the privacy scan reads
#   ONYX_EARLY_REPO        optional: the Onyx checkout (default: this script's), for
#                          deploy/early-record/contracts.json, exclusions.json and the scan;
#                          ONYX_EARLY_CONTRACTS, ONYX_EARLY_EXCLUSIONS override the two files
#   ONYX_EARLY_BIN         optional: a directory holding early-record and onyx-publish, put first on PATH
# Publishing only (not read with --dry-run):
#   ONYX_EARLY_KEY         the record's key file, made by `onyx-publish keygen`: its path is passed to
#                          onyx-publish (--key, --gas-key); this script never reads or prints it
#   ONYX_EARLY_ANCHOR_RPC  the chain node for catalog-anchor; it goes on onyx-publish's command line,
#                          which every local user can read, so it must hold no key (no user info,
#                          query or path): a public endpoint
#   ONYX_EARLY_BOOK        the signed book (0x address); ONYX_EARLY_CHAIN_ID (84532)
#   ONYX_EARLY_NODE        the Onyx node to upload to; ONYX_EARLY_WAIT_SECS (3600)
#
# Exit: 0 done (or nothing new, or the dry run reached its stop); 1 a tool or a node failed (run
# again: it goes on); 2 usage or config (also: the state has built days but OUT holds none); 3 the
# second node's rebuild of a day differs; 4 the privacy scan refused a version folder; 5 another
# run holds OUT's lock. 3 and 4 need a person (runbook "When the run stops"): that day and every
# later one stay unpublished until then; every earlier day is published.
set -euo pipefail
me=early-record-daily
dry=""
case "${1:-}" in
  "") ;;
  --dry-run) dry=1 ;;
  -h | --help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
  *) echo "$me: unknown argument (usage: $0 [--dry-run])" >&2; exit 2 ;;
esac
[ $# -le 1 ] || { echo "$me: one argument at most" >&2; exit 2; }
say() { echo "$me: $*"; }
die() { local code=$1; shift; echo "$me: $*" >&2; exit "$code"; }
need() { [ -n "${!1:-}" ] || die 2 "set $1 (see the header of $0)"; }
# scheme://host:port of the URL in $U (read from the environment: a URL may hold a key, and the
# process list shows every argument).
origin() { U=$1 python3 -I -c 'import os, urllib.parse as u
p = u.urlsplit(os.environ["U"].strip())
s = p.scheme.lower()
print("%s://%s:%d" % (s, (p.hostname or "").lower(), p.port or {"http": 80, "https": 443}.get(s, 0)))'; }

repo=${ONYX_EARLY_REPO:-$(cd "$(dirname "$0")/.." && pwd)}
for v in ONYX_EARLY_RPC ONYX_EARLY_RPC2 ONYX_EARLY_STATE ONYX_EARLY_OUT ONYX_EARLY_PUBLISHER ONYX_EARLY_INDEXER; do
  need "$v"
done
o1=$(origin "$ONYX_EARLY_RPC" 2>/dev/null) && o2=$(origin "$ONYX_EARLY_RPC2" 2>/dev/null) \
  || die 2 "ONYX_EARLY_RPC or ONYX_EARLY_RPC2 is not a URL"
[ "$o1" != "$o2" ] || die 2 "ONYX_EARLY_RPC2 must be another node than ONYX_EARLY_RPC (another scheme, host or port; verify is the second opinion)"
[[ "$ONYX_EARLY_PUBLISHER" =~ ^[a-z0-9]([a-z0-9._:-]*[a-z0-9])?$ ]] || die 2 "ONYX_EARLY_PUBLISHER is not a publisher id"
contracts=${ONYX_EARLY_CONTRACTS:-$repo/deploy/early-record/contracts.json}
exclusions=${ONYX_EARLY_EXCLUSIONS:-$repo/deploy/early-record/exclusions.json}
scan=$repo/scripts/audit-privacy-scan.py
for f in "$contracts" "$exclusions" "$scan"; do [ -f "$f" ] || die 2 "missing $f"; done
[ -d "$ONYX_EARLY_INDEXER" ] || die 2 "ONYX_EARLY_INDEXER is not a directory"
# Absolute paths: the privacy scan joins a relative target to the repository, not to here.
[ -d "$ONYX_EARLY_OUT" ] || die 2 "ONYX_EARLY_OUT is not an existing folder (make it once)"
out=$(realpath -e "$ONYX_EARLY_OUT") || die 2 "ONYX_EARLY_OUT cannot be resolved"
state=$(realpath -m "$ONYX_EARLY_STATE") || die 2 "ONYX_EARLY_STATE cannot be resolved"
if [ -z "$dry" ]; then
  for v in ONYX_EARLY_KEY ONYX_EARLY_ANCHOR_RPC ONYX_EARLY_BOOK ONYX_EARLY_NODE; do need "$v"; done
  [ -e "$ONYX_EARLY_KEY" ] || die 2 "ONYX_EARLY_KEY names no file (make it with onyx-publish keygen)"
  [[ "$ONYX_EARLY_ANCHOR_RPC" =~ ^https?://[A-Za-z0-9.-]+(:[0-9]+)?/?$ ]] \
    || die 2 "ONYX_EARLY_ANCHOR_RPC must be a bare scheme://host[:port] (it goes on a command line; a URL with a path, query or user info may hold a key)"
  [[ "$ONYX_EARLY_BOOK" =~ ^0x[0-9a-fA-F]{40}$ ]] || die 2 "ONYX_EARLY_BOOK is not a 0x address"
fi
chain_id=${ONYX_EARLY_CHAIN_ID:-84532}
wait_secs=${ONYX_EARLY_WAIT_SECS:-3600}
[[ "$chain_id" =~ ^[0-9]+$ && "$wait_secs" =~ ^[0-9]+$ ]] || die 2 "ONYX_EARLY_CHAIN_ID and ONYX_EARLY_WAIT_SECS are numbers"
[ -z "${ONYX_EARLY_BIN:-}" ] || PATH=$ONYX_EARLY_BIN:$PATH
for t in early-record onyx-publish python3 flock realpath; do command -v "$t" >/dev/null || die 2 "$t is not on PATH"; done

vdir=$out/versions cdir=$out/catalogs done=$out/done
mkdir -p "$vdir" "$cdir" "$done"
exec 9>>"$out/lock"
flock -n 9 || die 5 "another run holds $out/lock"
RECORD=testnet.early-contributors
EXCL=testnet.early-contributors.exclusions
mark() { : >"$done/$1.$2"; }
marked() { [ -e "$done/$1.$2" ]; }
# The node URLs reach early-record only through its environment.
er() { local rpc=$1; shift; ONYX_EARLY_RPC=$rpc early-record "$@"; }
json() { python3 -I -c 'import json,sys; v=json.loads(sys.stdin.read() or "{}"); print(v.get(sys.argv[1]) or "")' "$1"; }
# DAYS: the built days, sorted (a folder listing that fails stops the run).
DAYS=()
load_days() {
  local list
  list=$(find "$vdir" -mindepth 1 -maxdepth 1 -type d -name '????-??-??' -printf '%f\n' | sort) \
    || die 1 "cannot list $vdir"
  DAYS=()
  [ -z "$list" ] || mapfile -t DAYS <<<"$list"
}
# The version folder of the newest day before $1 (with sub-folder $2, when given) that holds a manifest.
prev_of() {
  local d best=""
  for d in "${DAYS[@]}"; do
    [[ "$d" < "$1" ]] || break
    [ -e "$vdir/$d${2:+/$2}/manifest.bin" ] && best=$vdir/$d${2:+/$2}
  done
  echo "$best"
}
# The catalogs of days before $1.
catalogs_before() {
  local c
  for c in "$cdir"/????-??-??.bin; do
    [ -e "$c" ] && [[ "$(basename "$c" .bin)" < "$1" ]] && echo "$c"
  done
  return 0
}

# 1. Build every complete day not built yet.
staged=$vdir/next
load_days
# OUT lost but the state kept: a build would start a new version 1 and catalog 0 after the state's
# last day. Stop: restore OUT, or start over with a new state (runbook "When the run stops").
if [ ${#DAYS[@]} = 0 ] && [ ! -e "$staged/day.json" ] && [ -e "$state/state.json" ]; then
  last=$(python3 -I -c 'import datetime, json, sys
l = json.load(open(sys.argv[1])).get("last")
print(datetime.date(1970, 1, 1) + datetime.timedelta(days=l["day"]) if l else "")' "$state/state.json") \
    || die 1 "cannot read $state/state.json"
  [ -z "$last" ] || die 2 "the state has built days (the last one $last) but $vdir holds none: restore OUT before going on"
fi
while :; do
  args=(build --contracts "$contracts" --exclusions "$exclusions" --state "$state" --out "$staged")
  # A staged folder left by a stopped run is the state's last day (or the day it was writing): no --prev.
  [ -e "$staged/day.json" ] || [ ${#DAYS[@]} = 0 ] || args+=(--prev "$vdir/${DAYS[-1]}")
  res=$(er "$ONYX_EARLY_RPC" "${args[@]}") || die 1 "early-record build stopped (run again to go on)"
  day=$(json built <<<"$res"); [ -n "$day" ] || day=$(json already_built <<<"$res")
  if [ -z "$day" ]; then
    [ -n "$(json nothing_new <<<"$res")" ] || die 1 "early-record build said: $res"
    break
  fi
  [[ "$day" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || die 1 "early-record build named no day: $res"
  [ ! -e "$vdir/$day" ] || die 1 "$vdir/$day exists and $staged holds the same day: look before going on"
  mv "$staged" "$vdir/$day"
  say "$day: built: $res"
  load_days
done
[ ! -e "$staged" ] || rmdir "$staged" 2>/dev/null || die 1 "$staged is left over and not empty: look before going on"

# 2. Each day not published yet, in order, all the way.
new=()
for d in "${DAYS[@]}"; do marked "$d" published || new+=("$d"); done
[ ${#new[@]} -gt 0 ] || { say "nothing new"; exit 0; }
for d in "${new[@]}"; do
  f=$vdir/$d cat=$cdir/$d.bin
  # a. The privacy scan, on the folder's text files (before the bulk build adds the shards).
  if ! marked "$d" scanned; then
    if ! env -u SHOW FAIL_ON_HIT=1 python3 -I "$scan" "$repo" "$ONYX_EARLY_INDEXER" "$f" >"$out/scan.log" 2>&1; then
      cat "$out/scan.log" >&2
      die 4 "$d: the privacy scan refused the folder: $d and every later day stay unpublished"
    fi
    mark "$d" scanned
    say "$d: privacy scan clean ($(grep -m1 '^files scanned' "$out/scan.log"))"
  fi
  # b. The bulk versions.
  for sub in "" exclusions; do
    g=$f${sub:+/$sub}
    [ -e "$g/data.csv" ] && [ ! -e "$g/manifest.bin" ] || continue
    ds=$RECORD; [ -z "$sub" ] || ds=$EXCL
    # Created at the end of its day (00:00 UTC of the next): later than the day before's, never in
    # the future, and the same bytes when a stopped build runs again.
    bargs=(build --schema "$g/schema.json" --input "$g/data.csv" --dataset "$ds" --publisher "$ONYX_EARLY_PUBLISHER"
      --license CC0-1.0 --out "$g" --created "$(( $(date -u -d "$d" +%s) + 86400 ))")
    p=$(prev_of "$d" "$sub"); [ -z "$p" ] || bargs+=(--prev "$p")
    onyx-publish "${bargs[@]}" || die 1 "$d: onyx-publish build of $ds stopped"
  done
  # c. The second node: bytes, state hash and shard roots.
  if ! marked "$d" verified; then
    before=$state/before/$d.json
    vargs=(verify --contracts "$contracts" --exclusions "$exclusions" --dir "$f")
    [ ! -e "$before" ] || vargs+=(--state-before "$before")
    if ! res=$(er "$ONYX_EARLY_RPC2" "${vargs[@]}" 2>"$out/verify.err"); then
      cat "$out/verify.err" >&2
      if grep -qE "differs from the chain's|root differs|is not the (record|exclusions)'s? schema|previous day's last block changed" "$out/verify.err"; then
        die 3 "$d: the second node's rebuild differs: $d and every later day stay unpublished"
      fi
      die 1 "$d: verify on the second node stopped (run again to go on)"
    fi
    [ "$(json verified <<<"$res")" = "$d" ] || die 1 "$d: verify said: $res"
    [ "$(json shard_roots <<<"$res")" -ge 1 ] 2>/dev/null || die 1 "$d: verify compared no shard root: $res"
    [ ! -e "$f/exclusions/manifest.bin" ] || [ "$(json exclusion_shard_roots <<<"$res")" -ge 1 ] 2>/dev/null \
      || die 1 "$d: verify compared no exclusions shard root: $res"
    mark "$d" verified
    say "$d: the second node gives the same bytes, state and shard roots: $res"
  fi
  # d. The day's catalog: its number is its place among the record's catalogs.
  n=$(catalogs_before "$d" | grep -c .) || true
  if [ ! -e "$cat" ]; then
    { echo "$f"; [ ! -e "$f/exclusions/manifest.bin" ] || echo "$f/exclusions"; } >"$cdir/$d.dirs"
    cargs=(catalog-build --publisher "$ONYX_EARLY_PUBLISHER" --dirs "$cdir/$d.dirs" --out "$cat")
    pc=$(catalogs_before "$d" | tail -n 1)
    [ -z "$pc" ] || cargs+=(--prev "$pc")
    res=$(onyx-publish "${cargs[@]}") || die 1 "$d: catalog-build stopped"
    echo "$res"
    if ! grep -q "^CATALOG number=$n " <<<"$res"; then
      # Never signed, so safe to set aside; the next run builds it again (and stops again until
      # a person finds the missing or extra catalog).
      mv "$cat" "$cat.refused"
      die 1 "$d: the catalog is not number $n (one a day, in day order; set aside as $cat.refused)"
    fi
  fi
  if [ -n "$dry" ]; then
    say "$d: dry run: built, scanned, verified, catalog $n ready; not signed, anchored or uploaded"
    continue
  fi
  # e. Sign, anchor, upload.
  [ -e "$cat.report" ] || onyx-publish catalog-sign --key "$ONYX_EARLY_KEY" --catalog "$cat" \
    || die 1 "$d: catalog-sign stopped"
  if ! marked "$d" anchored; then
    onyx-publish catalog-anchor --catalog "$cat" --rpc "$ONYX_EARLY_ANCHOR_RPC" --chain-id "$chain_id" \
      --book "$ONYX_EARLY_BOOK" --gas-key "$ONYX_EARLY_KEY" || die 1 "$d: catalog-anchor stopped (run again: a recorded report is not sent twice)"
    mark "$d" anchored
  fi
  if ! marked "$d" catalog-uploaded; then
    onyx-publish catalog-upload --catalog "$cat" --node "$ONYX_EARLY_NODE" --wait-secs "$wait_secs" \
      || die 1 "$d: catalog-upload stopped (run again to go on)"
    mark "$d" catalog-uploaded
  fi
  for g in "$f" "$f/exclusions"; do
    [ -e "$g/manifest.bin" ] || continue
    onyx-publish upload --dir "$g" --node "$ONYX_EARLY_NODE" --catalog "$n" --wait-secs "$wait_secs" \
      || die 1 "$d: upload of $g stopped (run again: it sends only what the node misses)"
  done
  mark "$d" published
  say "$d: published (catalog $n)"
done
[ -z "$dry" ] || say "dry run done: ${#new[@]} day(s) ready, nothing sent"
