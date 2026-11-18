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
# The last stages try again by themselves (docs/READINESS.md A27, THREATS EARLY-21, RETRY-R1-*):
# `catalog-anchor`, `catalog-upload` (the wait for the anchor to be final) and each `upload`, and
# only when onyx-publish says the stop passes by itself: its exit 75 with a last line
# `TRANSIENT class=...` (the transaction not mined before the deadline, the chain node or the Onyx
# node unreachable, answering 429 or 5xx or "try again", the anchor not final in time). Each is
# safe to repeat: a recorded report is never sent twice, a transaction of the report that stays
# unmined is replaced at its own nonce by the tool, and a node takes only what it misses
# (crates/onyx-publish/src/anchor.rs, lib.rs `send_until`). Exit 1 of theirs is a refusal (a
# report the book rejects, a gas key without money, a stuck transaction a person must look at, a
# full node, a wrong chain): the run stops at once, exit 7, with onyx-publish's own line. Nothing
# before these stages is tried again here (the scan, the builds, verify, the catalog, the
# signature), and a stage that is done is never run again, with one exception: while the anchor is
# not final, `catalog-anchor` is run again before each new try (it reads the book first and sends
# only when the record is gone, as after a reorg), and its marker is cleared when the wait gives up.
#   early-record-daily: 2026-11-17: catalog-anchor try 2 of 5 after 300 s: not_mined
#   early-record-daily: 2026-11-17: catalog-anchor done at try 2 of 5
# The WHOLE run is time-boxed (review RETRY-R2-M4): ONYX_EARLY_RUN_SECS from its start, whatever
# it is doing (a build, verify, a backlog of days, a wait, a pause). Every tool runs under
# `timeout` with the time that is left, and when it is up the run stops with exit 8 and goes on
# where it stopped the next time. With the unit as installed (a start 20 minutes after each
# failed one, 4 starts in 12 hours) four runs and four gaps fit inside those 12 hours, so a day
# that keeps failing uses the unit's start limit up and the unit ends failed
# (scripts/early-record-daily-tests.py computes it from the unit file). Inside that box the
# pauses and failed tries of the last stages take at most ONYX_EARLY_RETRY_BUDGET_SECS, and no
# try waits for finality longer than what is left of it. EVERY exit but 0 logs one line with the
# tag onyx-early-record-alert by itself (`logger`), whatever the unit does next.
# An onyx-publish built before exit 75 (its usage text does not name it) ends 1 for everything: only
# then its last line is matched against the few texts of the same stops (`old_transient` below).
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
# The retry of the last stages (whole numbers; anything else is exit 2):
#   ONYX_EARLY_RETRY_TRIES        tries of one stage, the first included (5; 1 to 20; 1: no retry)
#   ONYX_EARLY_RETRY_SECS         the pause before the second try; it doubles before each later
#                                 one (300, so 300, 600, 1200, 2400: 75 minutes; 1 to 3600)
#   ONYX_EARLY_RETRY_BUDGET_SECS  the most a run spends on pauses and failed tries, all stages and
#                                 days together (5400; 600 to 6000: more would let a failing day
#                                 slip past the unit's start limit). Also the longest wait for
#                                 finality one try gets (the lower of it and ONYX_EARLY_WAIT_SECS)
#   ONYX_EARLY_RUN_SECS           the whole run's time, from its start (7200; 600 to 8400: four
#                                 runs of more, with the unit's gaps, would not fit its 12 hours).
#                                 A first backfill of many days is run again until "nothing new"
#
# Exit: 0 done (or nothing new, or the dry run reached its stop); 1 a tool or a node failed before
# the last stages, or a tool was killed (run again: it goes on); 2 usage or config (also: the state
# has built days but OUT holds none); 3 the second node's rebuild of a day differs; 4 the privacy
# scan refused a version folder; 5 another run holds OUT's lock; 6 a last stage kept failing in a
# way that passes by itself, past its tries or the budget (run again: it goes on, nothing is sent
# twice); 7 onyx-publish refused a last stage (not tried again: read its line first); 8 the run's
# time was up (run again: it goes on where it stopped). 3, 4 and 7 need a person (runbook "When
# the run stops"): that day and every later one stay unpublished until then; every earlier day is
# published. Every exit but 0 logs the alert line.
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
# One line with the unit's alert tag (deploy/systemd/user/onyx-early-record-alert.service logs
# the same tag when the unit ends failed): every exit but 0 is loud by itself, whether or not the
# unit starts the run again (review RETRY-R1-H2, RETRY-R2-M4). The messages name a day, a stage, a
# class or a folder of the job; never a node URL or the key's path.
ALERT_TAG=onyx-early-record-alert
alerted=""
alert() {
  alerted=1
  logger -t "$ALERT_TAG" -p user.err -- "$*" 2>/dev/null || echo "$ALERT_TAG: $*" >&2
}
die() { local code=$1; shift; echo "$me: $*" >&2; alert "onyx-early-record exit $code: $*"; exit "$code"; }
# A command that failed with no line of its own (set -e) still alerts.
trap 'rc=$?; [ "$rc" = 0 ] || [ -n "$alerted" ] || alert "onyx-early-record exit $rc: stopped without a message of its own (a command failed)"' EXIT
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
retry_tries=${ONYX_EARLY_RETRY_TRIES:-5} retry_secs=${ONYX_EARLY_RETRY_SECS:-300}
retry_budget=${ONYX_EARLY_RETRY_BUDGET_SECS:-5400}
# A whole number of at most 5 digits, no sign, no leading zero (bash would read 010 as octal).
whole() { [[ "$1" =~ ^(0|[1-9][0-9]{0,4})$ ]] && [ "$1" -ge "$2" ] && [ "$1" -le "$3" ]; }
whole "$retry_tries" 1 20 || die 2 "ONYX_EARLY_RETRY_TRIES is a whole number from 1 to 20"
whole "$retry_secs" 1 3600 || die 2 "ONYX_EARLY_RETRY_SECS is a whole number from 1 to 3600"
whole "$retry_budget" 600 6000 || die 2 "ONYX_EARLY_RETRY_BUDGET_SECS is a whole number from 600 to 6000"
run_secs=${ONYX_EARLY_RUN_SECS:-7200}
whole "$run_secs" 600 8400 || die 2 "ONYX_EARLY_RUN_SECS is a whole number from 600 to 8400"
# bounded COMMAND...: run it for at most what is left of the run's time (SECONDS counts from the
# script's start). Status 124 when the time is up, before it or during it (`timeout` ends the
# command, and kills it 30 s later if it does not go).
bounded() {
  local l=$((run_secs - SECONDS))
  [ "$l" -gt 0 ] || return 124
  timeout -k 30 "$l" "$@"
}
# Whether status $1 is the run's time being up (124; 137 when `timeout` had to kill).
timeup() { [ "$1" = 124 ] || { [ "$1" = 137 ] && [ "$SECONDS" -ge "$run_secs" ]; }; }
# ended STATUS CODE MESSAGE: exit 8 when STATUS is the time being up, else exit CODE with MESSAGE.
ended() {
  local rc=$1 code=$2
  shift 2
  ! timeup "$rc" || die 8 "the run's time is up ($SECONDS s of ONYX_EARLY_RUN_SECS=$run_secs) at: $*; run again: it goes on where it stopped"
  die "$code" "$*"
}
[ -z "${ONYX_EARLY_BIN:-}" ] || PATH=$ONYX_EARLY_BIN:$PATH
for t in early-record onyx-publish python3 flock realpath tee sleep sed tail timeout; do command -v "$t" >/dev/null || die 2 "$t is not on PATH"; done

vdir=$out/versions cdir=$out/catalogs done=$out/done
mkdir -p "$vdir" "$cdir" "$done"
exec 9>>"$out/lock"
flock -n 9 || die 5 "another run holds $out/lock"
RECORD=testnet.early-contributors
EXCL=testnet.early-contributors.exclusions
mark() { : >"$done/$1.$2"; }
marked() { [ -e "$done/$1.$2" ]; }
# The node URLs reach early-record only through its environment.
er() { local rpc=$1; shift; ONYX_EARLY_RPC=$rpc bounded early-record "$@"; }
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

# Whether this onyx-publish ends with exit 75 for a stop that passes by itself: its usage text
# says so. Asked once; an older one (exit 1 for everything) is read by its text instead.
has_exit_75=""
if [ -z "$dry" ]; then
  usage=$(bounded onyx-publish --help 2>/dev/null) || usage=""
  [[ $usage != *"exit 75"* ]] || has_exit_75=1
fi
# The stop on the last line of FILE, when an onyx-publish built before exit 75 (everything is
# exit 1 there) names one that passes by itself; nothing and status 1 otherwise. The texts are that
# binary's own, each for one class of crates/onyx-publish/src/lib.rs `classes`. A gas key without
# money is never one of them. Never used with a tool that has the exit code.
old_transient() {
  local last
  last=$(tail -n 1 "$1") || return 1
  case $last in *"insufficient funds"* | *"insufficient balance"*) return 1 ;; esac
  if [[ $last =~ postReport:\ transaction\ 0x[0-9a-f]+\ not\ mined\ before\ the\ deadline$ ]]; then
    echo not_mined
  elif [[ $last =~ eth_sendRawTransaction:\ node\ error:.*\"message\":\"(already\ known|nonce\ too\ low|replacement\ transaction\ underpriced)\" ]]; then
    # The run before sent this report and its transaction holds the nonce (seen 2026-11-18).
    echo earlier_pending
  elif [[ $last =~ refused\ \(409\):.*\"bulk_not_anchored\" ]]; then
    echo not_final
  elif [[ $last =~ node\ answered\ HTTP\ (408|429|5[0-9][0-9])$ ]]; then
    echo rpc_http
  elif [[ $last =~ refused\ \((429|502)\): || $last =~ refused\ \(503\):.*\"(bulk_chain_unavailable|overloaded|bulk_busy)\" ]]; then
    echo node_busy
  elif [[ $last =~ (node\ unreachable:|the\ node\ did\ not\ answer\ in\ time|node\ call\ timed\ out) ]]; then
    echo unreachable
  else
    return 1
  fi
}
# Why onyx-publish's stop (exit code $1, its stderr in file $2) passes by itself: the class of
# its `TRANSIENT class=` line. Status 1 when it does not (a refusal).
transient_reason() {
  local class
  case $1 in
    75)
      class=$(sed -n 's/^onyx-publish [a-z-]*: TRANSIENT class=\([a-z_]*\) .*$/\1/p' "$2" | tail -n 1) || class=""
      echo "${class:-transient}"
      ;;
    1) [ -z "$has_exit_75" ] && old_transient "$2" ;;
    *) return 1 ;;
  esac
}
# Run onyx-publish with the arguments given; its exit code in tool_rc. Its output goes where it
# went, its stderr also to OUT/stage.err (for the class). The exit code is the tool's own, read
# from the pipe's first place: a `tee` that fails (a full disk) never turns a 75 into something else.
tool_rc=0
run_tool() {
  local -a st
  set +e
  {
    bounded onyx-publish "$@" 2>&1 >&3 | tee "$out/stage.err" >&2
    st=("${PIPESTATUS[@]}")
  } 3>&1
  set -e
  tool_rc=${st[0]}
}
# The record may have left the chain (a block reorganized away) although the day is marked
# anchored: while its anchor is not final, run catalog-anchor again. It reads the book first, so
# it sends only when the record is gone. Its own passing stop does not end the wait.
reanchor() {
  local d=$1 why began=$SECONDS
  run_tool catalog-anchor --catalog "$cdir/$d.bin" --rpc "$ONYX_EARLY_ANCHOR_RPC" \
    --chain-id "$chain_id" --book "$ONYX_EARLY_BOOK" --gas-key "$ONYX_EARLY_KEY"
  [ "$tool_rc" != 0 ] || return 0
  # Its time counts against the run's budget like any failed try.
  lost=$((lost + SECONDS - began))
  rm -f "${done:?}/${d:?}.anchored"
  ! timeup "$tool_rc" || ended "$tool_rc" 8 "$d: catalog-anchor (run again while the anchor is not final)"
  [ "$tool_rc" -le 128 ] \
    || die 1 "$d: catalog-anchor: onyx-publish was killed (exit $tool_rc, signal $((tool_rc - 128))); run again: it goes on"
  if why=$(transient_reason "$tool_rc" "$out/stage.err"); then
    say "$d: catalog-anchor, run again while the anchor is not final, stopped ($why): the wait goes on"
    return 0
  fi
  die 7 "$d: catalog-anchor, run again while the anchor is not final, was refused (onyx-publish exit $tool_rc): read its line above before running again"
}
# retried DAY STAGE onyx-publish-arguments...: run the stage, and again while its stop passes by
# itself, within ONYX_EARLY_RETRY_TRIES and the run's budget (pauses and failed tries). An
# argument @WAIT@ becomes the seconds this try may wait for finality: ONYX_EARLY_WAIT_SECS, or
# what is left of the budget when that is less (a minute at least). Exit 6 when it gives up, 7 at
# once when onyx-publish refused, 1 when the tool was killed.
slept=0 lost=0
retried() {
  local d=$1 stage=$2 try=1 delay=$retry_secs reason left wait began a
  local -a args
  shift 2
  while :; do
    left=$((retry_budget - lost))
    wait=$wait_secs
    [ "$wait" -le "$left" ] || wait=$left
    [ "$wait" -ge 60 ] || [ "$wait_secs" -lt 60 ] || wait=60
    [ "$wait" -ge 0 ] || wait=0
    args=()
    for a in "$@"; do
      [ "$a" != @WAIT@ ] || a=$wait
      args+=("$a")
    done
    began=$SECONDS
    run_tool "${args[@]}"
    if [ "$tool_rc" = 0 ]; then
      [ "$try" = 1 ] || say "$d: $stage done at try $try of $retry_tries"
      return 0
    fi
    lost=$((lost + SECONDS - began))
    ! timeup "$tool_rc" || ended "$tool_rc" 8 "$d: $stage, try $try"
    # Killed (128 + a signal: out of memory, a stop of the unit): not the tool's answer.
    [ "$tool_rc" -le 128 ] \
      || die 1 "$d: $stage: onyx-publish was killed (exit $tool_rc, signal $((tool_rc - 128))) at try $try; run again: it goes on"
    reason=$(transient_reason "$tool_rc" "$out/stage.err") \
      || die 7 "$d: $stage refused at try $try (onyx-publish exit $tool_rc): not tried again; read its line above before running again"
    if [ "$try" -ge "$retry_tries" ] || [ $((lost + delay)) -gt "$retry_budget" ]; then
      # The anchor may be gone for good (a reorg): the next run anchors again (it reads the book first).
      [ "$reason" != not_final ] || rm -f "${done:?}/${d:?}.anchored"
      [ "$try" -lt "$retry_tries" ] \
        || die 6 "$d: $stage gave up after $try of $retry_tries tries ($reason; $lost s of the run's $retry_budget s budget used); run again: it goes on, nothing is sent twice"
      die 6 "$d: $stage gave up after $try of $retry_tries tries ($reason): the next pause of $delay s is past the run's budget ($lost of $retry_budget s used); run again: it goes on, nothing is sent twice"
    fi
    [ $((SECONDS + delay)) -lt "$run_secs" ] \
      || ended 124 8 "$d: $stage after $try of $retry_tries tries ($reason): no time for the next pause of $delay s"
    try=$((try + 1))
    say "$d: $stage try $try of $retry_tries after $delay s: $reason"
    sleep "$delay" || die 1 "$d: $stage: the pause before try $try was cut"
    slept=$((slept + delay))
    lost=$((lost + delay))
    delay=$((delay * 2))
    [ "$delay" -le 3600 ] || delay=3600
    [ "$reason" != not_final ] || [ "$stage" = catalog-anchor ] || reanchor "$d"
  done
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
  res=$(er "$ONYX_EARLY_RPC" "${args[@]}") || ended $? 1 "early-record build stopped (run again to go on)"
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
    rc=0
    bounded env -u SHOW FAIL_ON_HIT=1 python3 -I "$scan" "$repo" "$ONYX_EARLY_INDEXER" "$f" >"$out/scan.log" 2>&1 || rc=$?
    if [ "$rc" != 0 ]; then
      cat "$out/scan.log" >&2
      ended "$rc" 4 "$d: the privacy scan refused the folder: $d and every later day stay unpublished"
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
    bounded onyx-publish "${bargs[@]}" || ended $? 1 "$d: onyx-publish build of $ds stopped"
  done
  # c. The second node: bytes, state hash and shard roots.
  if ! marked "$d" verified; then
    before=$state/before/$d.json
    vargs=(verify --contracts "$contracts" --exclusions "$exclusions" --dir "$f")
    [ ! -e "$before" ] || vargs+=(--state-before "$before")
    rc=0
    res=$(er "$ONYX_EARLY_RPC2" "${vargs[@]}" 2>"$out/verify.err") || rc=$?
    if [ "$rc" != 0 ]; then
      cat "$out/verify.err" >&2
      ! timeup "$rc" || ended "$rc" 8 "$d: verify on the second node"
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
    res=$(bounded onyx-publish "${cargs[@]}") || ended $? 1 "$d: catalog-build stopped"
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
  if [ ! -e "$cat.report" ]; then
    rc=0
    bounded onyx-publish catalog-sign --key "$ONYX_EARLY_KEY" --catalog "$cat" || rc=$?
    if [ "$rc" != 0 ]; then
      # Never anchored (the anchor comes after): a signature cut off half-written is not kept.
      rm -f "${cat:?}.report"
      ended "$rc" 1 "$d: catalog-sign stopped"
    fi
  fi
  # (A wait that gave up on "not final" cleared the marker: the anchor is read again, see reanchor.)
  if ! marked "$d" anchored; then
    retried "$d" catalog-anchor catalog-anchor --catalog "$cat" --rpc "$ONYX_EARLY_ANCHOR_RPC" \
      --chain-id "$chain_id" --book "$ONYX_EARLY_BOOK" --gas-key "$ONYX_EARLY_KEY"
    mark "$d" anchored
  fi
  if ! marked "$d" catalog-uploaded; then
    retried "$d" catalog-upload catalog-upload --catalog "$cat" --node "$ONYX_EARLY_NODE" --wait-secs @WAIT@
    mark "$d" catalog-uploaded
  fi
  for g in "$f" "$f/exclusions"; do
    [ -e "$g/manifest.bin" ] || continue
    stage=upload; [ "$g" = "$f" ] || stage=upload-exclusions
    retried "$d" "$stage" upload --dir "$g" --node "$ONYX_EARLY_NODE" --catalog "$n" --wait-secs @WAIT@
  done
  mark "$d" published
  say "$d: published (catalog $n)"
done
[ -z "$dry" ] || say "dry run done: ${#new[@]} day(s) ready, nothing sent"
[ "$slept" = 0 ] || say "done: the last stages paused $slept s in all between tries"
