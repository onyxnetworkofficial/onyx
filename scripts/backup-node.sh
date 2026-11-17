#!/usr/bin/env bash
# Daily backup of the live API node (THREATS OPS-BACKUP-1, OPS-BACKUP-2; docs/ops/deployment.md "Backups"): pulls
# the server's bulk store, state journals, row batches, /etc/onyx and its binary into a dated snapshot.
#
# Usage: scripts/backup-node.sh [--dest DIR] [--keep N] [--weeks W] [--months M] [--bwlimit KIB]
#                               [--allow-shrink] [--no-verify]
#        scripts/backup-node.sh --check-fresh HOURS [--dest DIR]
#        scripts/backup-node.sh --self-test     (offline: a local fake source, no ssh, no server)
#   --dest          where snapshots go (default $ONYX_BACKUP_DEST or ~/onyx-backups/node). Must be mode
#                   0700, owned by you and outside any git checkout; created 0700 when missing.
#   --keep          newest snapshots kept (default $ONYX_BACKUP_KEEP or 7). --weeks W (default 4): the
#                   oldest snapshot of this calendar week and of each of the W weeks before it is kept
#                   too (with daily runs the oldest of them is 28 to 34 days old; none with 0).
#                   --months M (default 3): the same for calendar months. Others go.
#   --bwlimit       rsync's cap in KiB/s (default $ONYX_BACKUP_BWLIMIT or 25000, about 200 Mbit/s; 0: none).
#   --allow-shrink  take a copy that looks like a loss on the server (refused without it, exit 4: a
#                   damaged or emptied server is not a backup): it lacks tiers.json, node.env,
#                   deployment.json, state/wants.salt, the server binary or a config file node.env
#                   names; its bulk store or row batches (cas/) hold more than 10% fewer files or
#                   bytes than the largest kept snapshot; more than 10% of the files of the previous,
#                   the largest or the oldest kept snapshot are gone from them (counted by name, so new
#                   uploads hide no loss; a shard moved to another tier is the same shard); or a file
#                   of the previous snapshot's state/ or etc-onyx/ is gone or a state file emptied.
#                   Snapshots before the newest one taken with --allow-shrink are not compared.
#   --no-verify     skip the restore drill on the copy (scripts/restore-drill.sh --full, run by default
#                   before a copy becomes a snapshot).
#   --check-fresh   exit 1 when the newest complete snapshot in DEST is older than HOURS, or none, or
#                   named more than an hour in the future (a wrong clock).
# Env: ONYX_BACKUP_HOST (ssh alias, default onyx-web), ONYX_BACKUP_TRIES (tries of each rsync and ssh
#      read after a dropped connection, default 4), ONYX_BACKUP_RETRY_SECS (the wait before try n is
#      n-1 times this, default 60), ONYX_BACKUP_LOCK_WAIT (seconds to wait for a drill or a run
#      holding DEST/.lock, default 1800).
# Exit: 0 a new snapshot; 1 failed, the next run continues (connection, a store still changing, a
#       deploy during the copy, a failed drill, DEST/.lock busy); 2 refused (arguments, DEST, a clock
#       behind the newest snapshot); 4 refused as a likely loss on the server (see --allow-shrink).
#       Nothing is pruned unless the run made a snapshot.
#
# Read only on the server: rsync over ssh with `sudo -n nice -n 19 ionice -c3 rsync` on the far side
# (lowest CPU and disk priority: the node and an upload in progress go first), one curl of /v1/health
# on 127.0.0.1, and a sha256sum of /usr/local/bin/onyx-node before and after the copy. Nothing there
# is stopped or written. The node keeps running: a state journal may end in a torn last line, which
# the journal cuts off on open (crates/onyx-api/src/journal.rs).
#
# The bulk store must be one the node can open: its loader refuses a tiers.json that names a shard
# missing from its tier folder (crates/onyx-coordinator/src/tiers.rs, TieredStore::load). So
# tiers.json is copied first (the node writes a shard before the tiers.json that names it), then the
# rest; the bulk store is copied again until a pass finds nothing new. A store still changing after 3
# passes fails the run (a manifest could name a shard the copy missed); every shard the copied
# tiers.json names must be in the copy. Then the restore drill runs on the copy with --full: a local
# node of the snapshot's own binary opens it, every shard reads back and hashes to its name, every
# dataset's newest manifest has all its shards, samples pass onyx-check against the live node.
#
# Each snapshot DEST/<UTC time>/ holds bulk/, state/, cas/, etc-onyx/ (every `*.key` left out: an
# offline copy of the operator and storage-node keys is the owner's job), server/ (the binary and its
# systemd unit), SNAPSHOT.json (time, the binary's sha256 as the build, file counts and bytes per
# part, sha256 of each state file, tier counts, passes) and DRILL.txt (the drill's lines). Files
# unchanged since the previous snapshot are hard links to it (rsync --link-dest); rsync never
# rewrites a file in place (a new file is renamed in), so a linked file is never changed under an
# older snapshot. Every file is 0600, every folder 0700. A run is written to DEST/.partial-<time>,
# flushed to disk and renamed when complete; a failed run's partial folder is continued by the next
# run. A dropped connection is retried (rsync exit 10, 12, 30, 35 or 255). A drill folder
# (.drill.*) older than a day was left by a killed drill and is deleted. A drill's node runs in its
# own scope, outside the drill's cgroup, so a SIGKILLed drill (or backup) leaves it running: before
# the lock, every onyx-restore-drill-<port>-<drill pid>.scope whose drill is gone is stopped. No file
# content is printed (node.env and the state may be sensitive): one summary line, or an error.
set -euo pipefail

host=${ONYX_BACKUP_HOST:-onyx-web}
dest=${ONYX_BACKUP_DEST:-$HOME/onyx-backups/node}
keep=${ONYX_BACKUP_KEEP:-7}
weeks=4
months=3
bwlimit=${ONYX_BACKUP_BWLIMIT:-25000}
tries=${ONYX_BACKUP_TRIES:-4}
retry_secs=${ONYX_BACKUP_RETRY_SECS:-60}
lock_wait=${ONYX_BACKUP_LOCK_WAIT:-1800}
allow_shrink=
verify=1
fresh_hours=
# Test hooks (the self-test only): a local folder standing in for the server's root, the snapshot's
# name and "now" instead of the clock, an rsync stand-in, a verify command instead of the drill
# (with a local root, no drill runs unless one is named), a sync stand-in and a systemctl stand-in
# (with a local root, no drill scope is looked for unless one is named).
local_root=${ONYX_BACKUP_LOCAL_ROOT:-}
now=${ONYX_BACKUP_NOW:-}
rsync_cmd=${ONYX_BACKUP_RSYNC:-rsync}
sync_cmd=${ONYX_BACKUP_SYNC:-sync}
systemctl_cmd=${ONYX_BACKUP_SYSTEMCTL:-}
self=$(realpath "$0")
repo=$(dirname "$(dirname "$self")")
verify_cmd=${ONYX_BACKUP_VERIFY_CMD:-}
if [ -z "$verify_cmd" ] && [ -z "$local_root" ]; then verify_cmd="$repo/scripts/restore-drill.sh --full"; fi
max_passes=3
snap_re='^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{6}Z$'
# Retention: snapshot names on stdin, KEEP WEEKS MONTHS as arguments, the names to prune on stdout.
# Kept: the newest KEEP; the oldest snapshot of this calendar week (Monday to Sunday, the newest
# snapshot's) and of each of the WEEKS weeks before it; the oldest of this calendar month and of each
# of the MONTHS months before it (review BAK-R2-M1). The oldest of a period never changes once a
# later one is taken, so with daily runs the oldest weekly copy is 28 to 34 days old (4 weeks) and
# the oldest monthly one 3 to 4 months.
PYKEEP=$(cat <<'PY'
import datetime as dt, sys
keep, weeks, months = (int(a) for a in sys.argv[1:4])
names = sorted(l.strip() for l in sys.stdin if l.strip())
def day(n):
    return dt.datetime.strptime(n, "%Y-%m-%dT%H%M%SZ").date()
kept = set(names[-keep:])
if names:
    last = day(names[-1])
    week0 = last - dt.timedelta(days=last.weekday())
    month0 = last.year * 12 + last.month - 1
    first = {}
    for n in names:  # oldest first: the first name seen in a period is its oldest
        d = day(n)
        w = (week0 - (d - dt.timedelta(days=d.weekday()))).days // 7
        m = month0 - (d.year * 12 + d.month - 1)
        if weeks and 0 <= w <= weeks:
            first.setdefault(("week", w), n)
        if months and 0 <= m <= months:
            first.setdefault(("month", m), n)
    kept |= set(first.values())
print("\n".join(n for n in names if n not in kept))
PY
)

die() { echo "backup-node: $*" >&2; exit 2; }

self_test() {
  local t pass=0 failed=0 out rc snaps s1 s2 s3 i
  t=$(mktemp -d "${TMPDIR:-/tmp}/backup-node-test.XXXXXX")
  # shellcheck disable=SC2064
  trap "rm -rf '$t'" EXIT
  ok() { echo "ok: $1"; pass=$((pass + 1)); }
  bad() { echo "FAIL: $1"; failed=$((failed + 1)); }
  check() { if eval "$2"; then ok "$1"; else bad "$1"; fi; }
  # A planted marker in every sensitive file: it must never reach the output.
  local mark
  mark="planted-$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')"
  local src=$t/src b=$t/src/var/lib/onyx
  # The node's layout: shards at bulk/hot/bulk/<root hex>, each named in bulk/tiers.json.
  local ka kb kc kd ke kx hot=$b/bulk/hot/bulk tiers_n=0
  ka=$(printf 'a%.0s' {1..64}) kb=$(printf 'b%.0s' {1..64}) kc=$(printf 'c%.0s' {1..64})
  kd=$(printf 'd%.0s' {1..64}) ke=$(printf 'e%.0s' {1..64}) kx=$(printf '9%.0s' {1..64})
  tiers() { # root... : a tiers.json naming each root as a hot shard, with a new mtime each time
    tiers_n=$((tiers_n + 1))
    python3 -I -c 'import json, sys; print(json.dumps({k: "bulk:hot" for k in sys.argv[1:]}))' "$@" >"$b/bulk/tiers.json"
    chmod 0644 "$b/bulk/tiers.json"
    touch -d "@$((1700000000 + tiers_n))" "$b/bulk/tiers.json"
  }
  mkdir -p "$b/bulk/catalogs/pub" "$b/bulk/manifests/pub/ds" "$b/bulk/manifests/pub/fill" "$hot" "$b/state" "$b/cas/rows" \
    "$src/etc/onyx/storage" "$src/etc/onyx/observations" "$src/usr/local/bin" "$src/etc/systemd/system"
  echo catalog0 >"$b/bulk/catalogs/pub/c0.bin"
  echo manifest1 >"$b/bulk/manifests/pub/ds/v1.bin"
  # 15 more manifests: one removed shard is a few percent of the store, as on a real one.
  for i in $(seq 1 15); do echo "fill $i" >"$b/bulk/manifests/pub/fill/v$i.bin"; done
  echo shard-a >"$hot/$ka"
  echo shard-b >"$hot/$kb"
  tiers "$ka" "$kb"
  echo "{\"cursor\":1,\"m\":\"$mark\"}" >"$b/state/indexer.json"
  echo "$mark" >"$b/state/wants.salt"
  : >"$b/state/submissions.journal.lock"
  echo rows >"$b/cas/rows/r1"
  echo "ONYX_RPC_URL=https://node.invalid/$mark" >"$src/etc/onyx/node.env"
  echo '{"chainId":84532}' >"$src/etc/onyx/deployment.json"
  echo '{}' >"$src/etc/onyx/catalog.json"
  echo "$mark" >"$src/etc/onyx/operator.key"
  echo "$mark" >"$src/etc/onyx/storage/n1.key"
  echo "STORE=$mark" >"$src/etc/onyx/storage/n1.env"
  echo '{}' >"$src/etc/onyx/observations/a.json"
  echo 'fake onyx-node build 1' >"$src/usr/local/bin/onyx-node"
  echo 'fake other tool' >"$src/usr/local/bin/other-tool"
  echo '[Service]' >"$src/etc/systemd/system/onyx-node.service"
  echo '[Service]' >"$src/etc/systemd/system/caddy.service"
  find "$src" -type d -exec chmod 0755 {} + && find "$src" -type f -exec chmod 0644 {} +
  # An rsync stand-in for a dropped connection: for the first $FLAKY_FAILS copies of the bulk store
  # (tiers.json alone passes) it copies all but the files named *late*, then exits 12, as rsync does
  # when ssh drops.
  cat >"$t/flaky-rsync" <<'SH'
#!/usr/bin/env bash
case " $* " in
  *--include=/tiers.json*) exec rsync "$@";;
  *"/var/lib/onyx/bulk/"*) ;;
  *) exec rsync "$@";;
esac
n=$(cat "$FLAKY_COUNT" 2>/dev/null || echo 0)
echo $((n + 1)) >"$FLAKY_COUNT"
[ "$n" -ge "$FLAKY_FAILS" ] && exec rsync "$@"
rsync --exclude='*late*' "$@" >/dev/null 2>&1 || true
echo "Read from remote host fake: No route to host" >&2
exit 12
SH
  # A publisher that never stops: every copy of the bulk store finds one more manifest version.
  cat >"$t/busy-rsync" <<'SH'
#!/usr/bin/env bash
case " $* " in *--exclude=/tiers.json*) n=$(ls "$BUSY" | wc -l); echo m >"$BUSY/v$((n + 10)).bin";; esac
exec rsync "$@"
SH
  # A deploy during the copy: the binary changes while /etc/onyx is copied.
  cat >"$t/deploy-rsync" <<'SH'
#!/usr/bin/env bash
case " $* " in *"/etc/onyx/"*) echo 'fake onyx-node build 2' >"$BIN";; esac
exec rsync "$@"
SH
  # The drill stand-in: records the folder it was given and whether SNAPSHOT.json was in it.
  cat >"$t/verify" <<'SH'
#!/usr/bin/env bash
[ "$1" = --full ] && shift
echo "$1 $([ -f "$1/SNAPSHOT.json" ] && echo with-json) ssh:${ONYX_DRILL_SSH_HOST:-}" >>"$VERIFY_LOG"
if [ "${VERIFY_FAIL:-}" = 1 ]; then echo "FAIL catalog pub: planted"; echo "DRILL FAIL: 1 of 9 checks failed"; exit 1; fi
echo "DRILL PASS: 9 checks passed"
SH
  # The sync stand-in: records whether the partial folder still existed when it ran.
  cat >"$t/sync" <<'SH'
#!/usr/bin/env bash
echo "$* partial:$(find "$SYNC_DEST" -maxdepth 1 -name '.partial-*' | wc -l)" >>"$SYNC_LOG"
SH
  chmod 0755 "$t/flaky-rsync" "$t/busy-rsync" "$t/deploy-rsync" "$t/verify" "$t/sync"
  local d=$t/backups
  mkdir -m 0700 "$d"
  mkdir -m 0700 "$d/notes" && echo keep >"$d/notes/readme"
  run() { # name keep [root [extra args...]]
    local n=$1 k=$2 root=${3:-$src}
    shift 2
    [ $# = 0 ] || shift
    out=$(ONYX_BACKUP_LOCK_WAIT=0 ONYX_BACKUP_HOST=invalid.invalid ONYX_BACKUP_LOCAL_ROOT=$root ONYX_BACKUP_NOW=$n \
      "$self" --dest "$d" --keep "$k" "$@" 2>&1) && rc=0 || rc=$?
    printf '%s\n' "$out" >>"$t/all-output"
  }
  with() { # VAR=value... -- name keep: run with extra environment
    local envs=()
    while [ "$1" != -- ]; do envs+=("$1"); shift; done
    shift
    out=$(env "${envs[@]}" ONYX_BACKUP_LOCK_WAIT=0 ONYX_BACKUP_HOST=invalid.invalid ONYX_BACKUP_LOCAL_ROOT="$src" \
      ONYX_BACKUP_NOW="$1" "$self" --dest "$d" --keep "$2" 2>&1) && rc=0 || rc=$?
    printf '%s\n' "$out" >>"$t/all-output"
  }
  flaky_run() { # name fails tries
    rm -f "$t/flaky-count"
    with FLAKY_COUNT="$t/flaky-count" FLAKY_FAILS="$2" ONYX_BACKUP_RSYNC="$t/flaky-rsync" ONYX_BACKUP_TRIES="$3" \
      ONYX_BACKUP_RETRY_SECS=0 -- "$1" 3
  }
  inode() { stat -c %i "$1" 2>/dev/null || echo none; }
  list_names() { find "$1" -mindepth 1 -maxdepth 1 -name '20*' -printf '%f\n' | sort | paste -sd' '; }
  snap_json_ok() { # SNAPSHOT.json state-dir binary
    python3 -I - "$1" "$2" "$3" <<'PY'
import hashlib, json, os, sys
s = json.load(open(sys.argv[1]))
want = {n: hashlib.sha256(open(os.path.join(sys.argv[2], n), "rb").read()).hexdigest() for n in os.listdir(sys.argv[2])}
assert s["stateSha256"] == want, "state sha256"
assert [s["parts"][p]["files"] for p in ("bulk", "state", "cas", "etc-onyx", "server")] == [20, 3, 1, 5, 2], s["parts"]
assert s["linkedFrom"] is None and s["copied"]["files"] == 31 and s["source"] == "local", s["copied"]
binsha = hashlib.sha256(open(sys.argv[3], "rb").read()).hexdigest()
assert s["serverBinarySha256"] == binsha and s["build"] == "sha256:" + binsha, (s["build"], binsha)
assert s["tiers"] == {"hot": 2, "warm": 0, "cold": 0} and s["bulkPasses"] == 2 and s["quiet"] is True, s
PY
  }

  run 2026-01-01T000001Z 5
  s1=$d/2026-01-01T000001Z
  check "a first run succeeds and makes one snapshot" '[ "$rc" = 0 ] && [ -d "$s1" ]'
  check "it prints one summary line" '[ "$(printf "%s\n" "$out" | wc -l)" = 1 ] && [[ $out == "backup ok: 2026-01-01T000001Z"* ]]'
  check "the parts and SNAPSHOT.json are there" '[ -f "$s1/bulk/hot/bulk/$ka" ] && [ -f "$s1/bulk/tiers.json" ] && [ -f "$s1/state/indexer.json" ] && [ -f "$s1/cas/rows/r1" ] && [ -f "$s1/etc-onyx/node.env" ] && [ -f "$s1/etc-onyx/storage/n1.env" ] && [ -f "$s1/SNAPSHOT.json" ]'
  # BAK-R1-M2: the server's binary and unit are in the snapshot, and the binary's sha256 is the build.
  check "the server binary and its unit are in the snapshot, nothing else of their folders (BAK-R1-M2)" '[ "$(ls "$s1/server" | paste -sd" ")" = "onyx-node onyx-node.service" ] && cmp -s "$s1/server/onyx-node" "$src/usr/local/bin/onyx-node"'
  check "every folder is 0700 and every file 0600" '[ -z "$(find "$s1" \( -type d ! -perm 0700 \) -o \( -type f ! -perm 0600 \))" ] && [ "$(stat -c %a "$d")" = 700 ]'
  check "no key file is copied" '[ -z "$(find "$s1" -name "*.key")" ]'
  check "SNAPSHOT.json has the state files' sha256, the part counts, the build and the tiers (BAK-R1-M2)" 'snap_json_ok "$s1/SNAPSHOT.json" "$b/state" "$src/usr/local/bin/onyx-node"'

  run 2026-01-01T000002Z 5
  s2=$d/2026-01-01T000002Z
  check "a second run with nothing new copies no file" '[ "$rc" = 0 ] && [[ $out == *"copied 0 files, 0 bytes"* ]]'
  check "its files are hard links to the first snapshot" '[ "$(inode "$s2/bulk/hot/bulk/$ka")" = "$(inode "$s1/bulk/hot/bulk/$ka")" ] && [ "$(inode "$s2/etc-onyx/node.env")" = "$(inode "$s1/etc-onyx/node.env")" ] && [ "$(inode "$s2/server/onyx-node")" = "$(inode "$s1/server/onyx-node")" ]'

  echo shard-c >"$hot/$kc" && chmod 0644 "$hot/$kc"
  rm "$hot/$kb"
  tiers "$ka" "$kc"
  echo '{"cursor":2}' >"$b/state/indexer.json.new" && chmod 0644 "$b/state/indexer.json.new" && mv "$b/state/indexer.json.new" "$b/state/indexer.json"
  run 2026-01-01T000003Z 5
  s3=$d/2026-01-01T000003Z
  check "a third run copies only the new shard, tiers.json and the changed state file" '[ "$rc" = 0 ] && [[ $out == *"copied 3 files"* ]]'
  check "the unchanged shard is still linked, the removed one is gone" '[ "$(inode "$s3/bulk/hot/bulk/$ka")" = "$(inode "$s1/bulk/hot/bulk/$ka")" ] && [ ! -e "$s3/bulk/hot/bulk/$kb" ] && [ -f "$s3/bulk/hot/bulk/$kc" ]'
  check "the older snapshot keeps its own state file" 'grep -qF "\"cursor\":1" "$s1/state/indexer.json" && grep -qF "\"cursor\":2" "$s3/state/indexer.json"'

  for i in 4 5 6; do run "2026-01-01T00000${i}Z" 3 "$src" --weeks 0 --months 0; done
  snaps=$(list_names "$d")
  check "--keep 3 with no weekly or monthly copy keeps the newest three snapshots" '[ "$snaps" = "2026-01-01T000004Z 2026-01-01T000005Z 2026-01-01T000006Z" ]'
  check "pruning leaves other folders in DEST alone" '[ -f "$d/notes/readme" ]'
  check "the kept snapshots still read whole after pruning" '[ "$(cat "$d/2026-01-01T000004Z/bulk/hot/bulk/$ka")" = shard-a ]'

  # A connection that drops once is retried within the run.
  echo shard-e >"$hot/$ke" && echo shard-late >"$hot/late-$ke" && chmod 0644 "$hot/$ke" "$hot/late-$ke"
  tiers "$ka" "$kc" "$ke"
  flaky_run 2026-01-02T000001Z 1 3
  check "a dropped connection is retried and the run completes" '[ "$rc" = 0 ] && [ -f "$d/2026-01-02T000001Z/bulk/hot/bulk/late-$ke" ] && [[ $out == *"connection lost (exit 12), try 2 of 3"* ]]'

  # A connection that stays down: the run fails, and the next run continues its partial copy.
  echo shard-early >"$hot/early-$kx" && echo shard-late2 >"$hot/late2-$kx" && chmod 0644 "$hot/early-$kx" "$hot/late2-$kx"
  flaky_run 2026-01-02T000002Z 99 2
  local partial early_inode before
  partial=$(find "$d" -mindepth 1 -maxdepth 1 -name '.partial-*' -printf '%f\n')
  check "a run whose connection stays down fails after its tries and keeps its partial copy" '[ "$rc" = 1 ] && [ ! -e "$d/2026-01-02T000002Z" ] && [ "$partial" = .partial-2026-01-02T000002Z ] && [ -f "$d/$partial/bulk/hot/bulk/early-$kx" ] && [ ! -e "$d/$partial/bulk/hot/bulk/late2-$kx" ]'
  early_inode=$(inode "$d/$partial/bulk/hot/bulk/early-$kx")
  run 2026-01-02T000003Z 3
  check "the next run continues the partial copy: it copies only what was missing" '[ "$rc" = 0 ] && [[ $out == *"copied 1 files"* ]] && [[ $out == *"continued $partial"* ]] && [ "$(inode "$d/2026-01-02T000003Z/bulk/hot/bulk/early-$kx")" = "$early_inode" ] && [ -f "$d/2026-01-02T000003Z/bulk/hot/bulk/late2-$kx" ] && [ -z "$(find "$d" -maxdepth 1 -name ".partial-*")" ]'

  # A tiers.json naming a shard the copy does not hold is a store the node could not open.
  tiers "$ka" "$kc" "$ke" "$kd"
  run 2026-01-02T000004Z 3
  check "a tiers.json naming a missing shard fails the run, no snapshot" '[ "$rc" = 1 ] && [ ! -e "$d/2026-01-02T000004Z" ] && [[ $out == *"tiers.json names 1 shard"* ]]'
  tiers "$ka" "$kc" "$ke"

  # BAK-R1-M1: a store that keeps changing through every pass is no snapshot, and prunes nothing.
  before=$(list_names "$d")
  with BUSY="$b/bulk/manifests/pub/ds" ONYX_BACKUP_RSYNC="$t/busy-rsync" -- 2026-01-02T000005Z 1
  check "a store still changing after 3 passes fails the run, keeps the partial copy and prunes nothing (BAK-R1-M1)" '[ "$rc" = 1 ] && [ ! -e "$d/2026-01-02T000005Z" ] && [ -d "$d/.partial-2026-01-02T000005Z" ] && [ "$(list_names "$d")" = "$before" ] && [[ $out == *"still changing after 3 passes"* ]]'
  rm -f "$b/bulk/manifests/pub/ds"/v1[0-9].bin

  # BAK-R1-M1: the drill on the copy decides; a failed drill is no snapshot and prunes nothing.
  with VERIFY_LOG="$t/verify.log" VERIFY_FAIL=1 ONYX_BACKUP_VERIFY_CMD="$t/verify --full" -- 2026-01-02T000006Z 1
  check "a failed drill on the copy fails the run, prunes nothing and shows the drill's FAIL lines (BAK-R1-M1)" '[ "$rc" = 1 ] && [ ! -e "$d/2026-01-02T000006Z" ] && [ "$(list_names "$d")" = "$before" ] && [[ $out == *"FAIL catalog pub: planted"* ]] && [ "$(cat "$t/verify.log")" = "$d/.partial-2026-01-02T000006Z with-json ssh:invalid.invalid" ]'
  rm -f "$t/verify.log"
  with VERIFY_LOG="$t/verify.log" ONYX_BACKUP_VERIFY_CMD="$t/verify --full" -- 2026-01-02T000007Z 3
  check "a passing drill makes the snapshot, its lines kept in DRILL.txt (BAK-R1-M1)" '[ "$rc" = 0 ] && [ -f "$d/2026-01-02T000007Z/DRILL.txt" ] && grep -q "DRILL PASS" "$d/2026-01-02T000007Z/DRILL.txt" && [[ $out == *"drill: DRILL PASS: 9 checks passed"* ]] && [ "$(stat -c %a "$d/2026-01-02T000007Z/DRILL.txt")" = 600 ]'
  check "the drill is given the backup's ssh host, to reach the live node when its public URL fails (BAK-R2-L5)" '[ "$(cat "$t/verify.log")" = "$d/.partial-2026-01-02T000007Z with-json ssh:invalid.invalid" ]'

  # BAK-R1-M2: a deploy during the copy mixes two builds: the run fails.
  with BIN="$src/usr/local/bin/onyx-node" ONYX_BACKUP_RSYNC="$t/deploy-rsync" -- 2026-01-02T000008Z 3
  check "a server binary that changes during the copy fails the run (BAK-R1-M2)" '[ "$rc" = 1 ] && [ ! -e "$d/2026-01-02T000008Z" ] && [[ $out == *"changed during the copy"* ]]'
  run 2026-01-02T000009Z 3
  check "the next run takes the new build (BAK-R1-M2)" '[ "$rc" = 0 ] && grep -q "build 2" "$d/2026-01-02T000009Z/server/onyx-node"'

  # BAK-R1-H1: an emptied or shrunken server store is refused and nothing is pruned.
  before=$(list_names "$d")
  mv "$b/bulk" "$t/bulk-saved" && mkdir -m 0755 "$b/bulk"
  run 2026-01-03T000001Z 1
  check "an emptied bulk store (no tiers.json) is refused, exit 4, nothing pruned (BAK-R1-H1)" '[ "$rc" = 4 ] && [ ! -e "$d/2026-01-03T000001Z" ] && [ "$(list_names "$d")" = "$before" ] && [[ $out == *"tiers.json"* ]]'
  run 2026-01-03T000002Z 1
  check "a second run on the emptied store is refused too: the good snapshots stay whole (BAK-R1-H1)" '[ "$rc" = 4 ] && [ "$(list_names "$d")" = "$before" ] && [ "$(find "$d"/20* -path "*/bulk/*" -type f | wc -l)" -gt 0 ]'
  rm -rf "$b/bulk" && mv "$t/bulk-saved" "$b/bulk"
  rm "$hot/early-$kx" "$hot/late2-$kx" "$hot/late-$ke"
  run 2026-01-03T000003Z 1
  check "a bulk store that lost more than 10% of its files is refused (BAK-R1-H1)" '[ "$rc" = 4 ] && [ ! -e "$d/2026-01-03T000003Z" ] && [ "$(list_names "$d")" = "$before" ] && [[ $out == *"--allow-shrink"* ]]'
  run 2026-01-03T000004Z 1 "$src" --allow-shrink
  check "--allow-shrink takes it (BAK-R1-H1)" '[ "$rc" = 0 ] && [ -d "$d/2026-01-03T000004Z" ]'
  mv "$src/etc/onyx/node.env" "$t/node.env.saved"
  run 2026-01-03T000005Z 1
  check "a copy without node.env is refused (BAK-R1-H1)" '[ "$rc" = 4 ] && [ ! -e "$d/2026-01-03T000005Z" ] && [[ $out == *"etc-onyx/node.env"* ]]'
  mv "$t/node.env.saved" "$src/etc/onyx/node.env"

  # BAK-R1-H1, BAK-R2-M1: retention keeps the newest N and the oldest snapshot of this week and of
  # each of the 4 weeks before it, so the oldest weekly copy is 4 weeks old (it was 15 to 27 days).
  local w=$t/weekly
  mkdir -m 0700 "$w"
  for i in 2026-02-02 2026-02-04 2026-02-09 2026-02-11 2026-02-16 2026-02-18 2026-02-23 2026-02-25 2026-03-02; do
    out=$(ONYX_BACKUP_LOCK_WAIT=0 ONYX_BACKUP_LOCAL_ROOT=$src ONYX_BACKUP_NOW="${i}T030000Z" "$self" --dest "$w" --keep 2 --months 0 2>&1) || bad "weekly run $i: $out"
  done
  check "retention keeps the newest 2 and the oldest of this week and each of the 4 weeks before it: 4 weeks back (BAK-R1-H1, BAK-R2-M1)" '[ "$(list_names "$w")" = "2026-02-02T030000Z 2026-02-09T030000Z 2026-02-16T030000Z 2026-02-23T030000Z 2026-02-25T030000Z 2026-03-02T030000Z" ]'
  # BAK-R2-M1: 400 daily runs through the retention rule, each pruning what it names.
  local w_old w_most m_old m_most
  out=$(python3 -I - "$PYKEEP" <<'PY'
import contextlib, datetime as dt, io, sys
code = compile(sys.argv[1], "PYKEEP", "exec")
def prune(names, keep, weeks, months):
    sys.argv, sys.stdin, buf = ["PYKEEP", str(keep), str(weeks), str(months)], io.StringIO("\n".join(names)), io.StringIO()
    with contextlib.redirect_stdout(buf):
        exec(code, {"__name__": "__main__"})
    gone = set(buf.getvalue().split())
    return [n for n in names if n not in gone]
for weeks, months, warm in ((4, 0, 35), (4, 3, 125)):
    names, worst, most = [], None, 0
    for i in range(400):
        today = dt.date(2026, 1, 1) + dt.timedelta(days=i)
        names = prune(names + [today.strftime("%Y-%m-%dT030000Z")], 7, weeks, months)
        most = max(most, len(names))
        oldest = (today - dt.datetime.strptime(names[0], "%Y-%m-%dT%H%M%SZ").date()).days
        if i >= warm:
            worst = oldest if worst is None else min(worst, oldest)
    print(worst, most)
PY
) && rc=0 || rc=$?
  read -r -d '' w_old w_most m_old m_most <<<"$out" || true
  check "400 daily runs: the oldest copy is never under 28 days with 4 weeks (at most 12 kept), never under 89 with 3 months too (at most 16) (BAK-R2-M1)" '[ "$rc" = 0 ] && [ "$w_old" -ge 28 ] && [ "$w_most" -le 12 ] && [ "$m_old" -ge 89 ] && [ "$m_most" -le 16 ]'

  # BAK-R2-M1: a slow loss adds up against the largest kept snapshot, not only the previous one; the
  # files gone are counted by name, so new uploads hide no loss; a tier move loses nothing.
  local s2=$t/src2 b2 h2 d2=$t/slow
  b2=$s2/var/lib/onyx h2=$t/src2/var/lib/onyx/bulk/hot/bulk
  mkdir -p "$h2" "$b2/bulk/warm/bulk" "$b2/state" "$b2/cas/rows" "$s2/etc/onyx" "$s2/usr/local/bin" "$s2/etc/systemd/system" "$t/stash2"
  mkdir -m 0700 "$d2"
  echo salt >"$b2/state/wants.salt" && echo rows >"$b2/cas/rows/r1" && echo ONYX_ENV=testnet >"$s2/etc/onyx/node.env"
  echo '{}' >"$s2/etc/onyx/deployment.json" && echo bin >"$s2/usr/local/bin/onyx-node" && echo '[Service]' >"$s2/etc/systemd/system/onyx-node.service"
  for i in $(seq 1 60); do printf 'shard %02d\n' "$i" >"$h2/$(printf '%064x' "$i")"; done
  tiers2() { # tiers.json naming every shard in hot/ and warm/
    python3 -I -c 'import json, os, sys; b = sys.argv[1]; json.dump({f: "bulk:" + t for t in ("hot", "warm") for f in os.listdir(os.path.join(b, t, "bulk"))}, open(os.path.join(b, "tiers.json"), "w"))' "$b2/bulk"
  }
  drop2() { local f; for f in $(ls "$h2" | sort | head -n "$1"); do mv "$h2/$f" "$t/stash2/"; done; tiers2; }
  slow() { # name [extra args...]
    out=$(ONYX_BACKUP_LOCK_WAIT=0 ONYX_BACKUP_HOST=invalid.invalid ONYX_BACKUP_LOCAL_ROOT=$s2 ONYX_BACKUP_NOW=$1 \
      "$self" --dest "$d2" --keep 3 "${@:2}" 2>&1) && rc=0 || rc=$?
    printf '%s\n' "$out" >>"$t/all-output"
  }
  tiers2
  slow 2026-03-02T030000Z
  drop2 5
  slow 2026-03-03T030000Z
  check "a store that lost 8% of its files since the last snapshot is taken (BAK-R2-M1)" '[ "$rc" = 0 ] && [ "$(list_names "$d2")" = "2026-03-02T030000Z 2026-03-03T030000Z" ]'
  drop2 5
  slow 2026-03-04T030000Z
  check "8% more the next day is refused: 16% down against the largest kept snapshot, nothing pruned (BAK-R2-M1)" '[ "$rc" = 4 ] && [ "$(list_names "$d2")" = "2026-03-02T030000Z 2026-03-03T030000Z" ] && [[ $out == *"bulk files 51 against 61 in 2026-03-02T030000Z"* ]] && [[ $out == *"10 of the 61 files of 2026-03-02T030000Z are gone"* ]]'
  slow 2026-03-05T030000Z --allow-shrink
  slow 2026-03-06T030000Z
  check "after a copy taken with --allow-shrink, the snapshots before it are not compared (BAK-R2-M1)" '[ "$rc" = 0 ] && [ -d "$d2/2026-03-06T030000Z" ]'
  for i in $(ls "$h2" | sort | head -n 10); do rm -f "$h2/$i"; done
  for i in $(seq 101 115); do printf 'shard %03d\n' "$i" >"$h2/$(printf '%064x' "$i")"; done
  tiers2
  slow 2026-03-07T030000Z
  check "a store that lost 10 of its 50 shards while 15 new ones raised its totals is refused: files gone are counted by name (BAK-R2-M1)" '[ "$rc" = 4 ] && [ ! -e "$d2/2026-03-07T030000Z" ] && [[ $out == *"10 of the 51 files of 2026-03-06T030000Z are gone (more than 10%; new files do not count)"* ]] && [[ $out != *"bulk files"* ]]'
  slow 2026-03-07T030001Z --allow-shrink
  for i in $(ls "$h2" | sort | tail -n 20); do mv "$h2/$i" "$b2/bulk/warm/bulk/"; done
  tiers2
  slow 2026-03-08T030000Z
  check "shards moved to another tier are the same shards, not a loss (BAK-R2-M1)" '[ "$rc" = 0 ] && [ -d "$d2/2026-03-08T030000Z" ] && [ "$(ls "$d2/2026-03-08T030000Z/bulk/warm/bulk" | wc -l)" = 20 ]'

  # BAK-R2-M2: state/, cas/ and the config node.env names have presence and shrink rules too.
  local s3=$t/src3 b3 d3=$t/m2
  b3=$s3/var/lib/onyx
  mkdir -p "$b3/bulk/hot/bulk" "$b3/state" "$b3/cas/rows" "$s3/etc/onyx/observations" "$s3/usr/local/bin" "$s3/etc/systemd/system" "$t/cas-saved"
  mkdir -m 0700 "$d3"
  echo shard >"$b3/bulk/hot/bulk/$ka" && echo "{\"$ka\":\"bulk:hot\"}" >"$b3/bulk/tiers.json"
  for i in indexer.json submissions.journal track-served-head.borsh wants.salt; do echo "$mark" >"$b3/state/$i"; done
  : >"$b3/state/submissions.journal.lock"
  for i in 1 2 3 4; do echo "rows $i" >"$b3/cas/rows/r$i"; done
  printf 'ONYX_CATALOG=/etc/onyx/catalog.json\nONYX_OBSERVATIONS=/etc/onyx/observations\nONYX_OPERATOR_KEY_FILE=/etc/onyx/operator.key\nONYX_RPC_URL=https://node.invalid/%s\n' "$mark" >"$s3/etc/onyx/node.env"
  echo '{}' >"$s3/etc/onyx/deployment.json" && echo '{}' >"$s3/etc/onyx/catalog.json" && echo '{}' >"$s3/etc/onyx/observations/a.json"
  echo bin >"$s3/usr/local/bin/onyx-node" && echo '[Service]' >"$s3/etc/systemd/system/onyx-node.service"
  m2() { # name: a run with --keep 1 and no weekly or monthly copy, the setting of the proof
    out=$(ONYX_BACKUP_LOCK_WAIT=0 ONYX_BACKUP_HOST=invalid.invalid ONYX_BACKUP_LOCAL_ROOT=$s3 ONYX_BACKUP_NOW=$1 \
      "$self" --dest "$d3" --keep 1 --weeks 0 --months 0 2>&1) && rc=0 || rc=$?
    printf '%s\n' "$out" >>"$t/all-output"
  }
  m2 2026-05-01T030000Z
  check "a server whose node.env names only files in /etc/onyx (and a key, never copied) is taken (BAK-R2-M2)" '[ "$rc" = 0 ] && [ "$(list_names "$d3")" = 2026-05-01T030000Z ]'
  mv "$b3/state" "$t/state-saved" && mkdir "$b3/state"
  m2 2026-05-02T030000Z
  check "an emptied state/ is refused, exit 4, and with --keep 1 the older snapshot stays (BAK-R2-M2)" '[ "$rc" = 4 ] && [ "$(list_names "$d3")" = 2026-05-01T030000Z ] && [[ $out == *"no state/wants.salt"* ]] && [[ $out == *"state: 4 files of 2026-05-01T030000Z gone (indexer.json, submissions.journal, track-served-head.borsh, wants.salt)"* ]]'
  rmdir "$b3/state" && mv "$t/state-saved" "$b3/state"
  : >"$b3/state/submissions.journal"
  m2 2026-05-02T030001Z
  check "a state journal emptied on the server is refused (BAK-R2-M2)" '[ "$rc" = 4 ] && [[ $out == *"state/submissions.journal is empty (it was not in 2026-05-01T030000Z)"* ]]'
  echo "$mark" >"$b3/state/submissions.journal"
  mv "$b3/cas/rows"/* "$t/cas-saved/"
  m2 2026-05-02T030002Z
  check "row batches gone from cas/ are refused (BAK-R2-M2)" '[ "$rc" = 4 ] && [[ $out == *"cas files 0 against 4 in 2026-05-01T030000Z"* ]] && [[ $out == *"cas: 4 of the 4 files of 2026-05-01T030000Z are gone"* ]]'
  mv "$t/cas-saved"/* "$b3/cas/rows/"
  mv "$s3/etc/onyx/catalog.json" "$t/catalog-saved" && mv "$s3/etc/onyx/observations" "$t/observations-saved"
  m2 2026-05-02T030003Z
  check "config files gone from /etc/onyx are refused, naming each setting that needs one, never its value (BAK-R2-M2)" '[ "$rc" = 4 ] && [[ $out == *"etc-onyx: 2 files of 2026-05-01T030000Z gone (catalog.json, observations/a.json)"* ]] && [[ $out == *"the file ONYX_CATALOG names is not in the copy"* ]] && [[ $out == *"the file ONYX_OBSERVATIONS names is not in the copy"* ]] && [[ $out != *OPERATOR* ]] && [[ $out != *"$mark"* ]]'
  mv "$t/catalog-saved" "$s3/etc/onyx/catalog.json" && mv "$t/observations-saved" "$s3/etc/onyx/observations"
  m2 2026-05-02T030004Z
  check "the whole server again is taken (BAK-R2-M2)" '[ "$rc" = 0 ] && [ "$(list_names "$d3")" = 2026-05-02T030004Z ]'

  # BAK-R1-L1: a clock behind the newest snapshot would sort the new one oldest and prune it.
  before=$(list_names "$d")
  run 2026-01-01T000009Z 1
  check "a snapshot name at or below the newest one is refused, nothing pruned (BAK-R1-L1)" '[ "$rc" = 2 ] && [ ! -e "$d/2026-01-01T000009Z" ] && [ "$(list_names "$d")" = "$before" ] && [[ $out == *"clock"* ]]'

  # BAK-R1-L4: the copy is flushed to disk before it is renamed.
  with SYNC_LOG="$t/sync.log" SYNC_DEST="$d" ONYX_BACKUP_SYNC="$t/sync" -- 2026-01-04T000001Z 3
  check "the copy is synced before the rename and the folder after it (BAK-R1-L4)" '[ "$rc" = 0 ] && [ "$(sed -n 1p "$t/sync.log")" = "-f $d/.partial-2026-01-04T000001Z partial:1" ] && [ "$(sed -n 2p "$t/sync.log")" = "-f $d partial:0" ]'

  # BAK-R1-L5: a drill folder a killed drill left (it holds a node.env copy) is deleted after a day.
  mkdir -m 0700 "$d/.drill.old1" "$d/.drill.new1" && echo x >"$d/.drill.old1/node.env"
  touch -d '-2 days' "$d/.drill.old1"
  run 2026-01-04T000002Z 3
  check "a drill folder older than a day is deleted, a newer one kept (BAK-R1-L5)" '[ "$rc" = 0 ] && [ ! -e "$d/.drill.old1" ] && [ -d "$d/.drill.new1" ]'
  rmdir "$d/.drill.new1"

  # BAK-R1-M3: the freshness check the alert units run.
  out=$(ONYX_BACKUP_NOW=2026-01-05T000000Z "$self" --check-fresh 30 --dest "$d" 2>&1) && rc=0 || rc=$?
  check "--check-fresh passes for a snapshot younger than the limit (BAK-R1-M3)" '[ "$rc" = 0 ] && [[ $out == *"2026-01-04T000002Z"* ]]'
  out=$(ONYX_BACKUP_NOW=2026-01-06T080000Z "$self" --check-fresh 30 --dest "$d" 2>&1) && rc=0 || rc=$?
  check "--check-fresh fails for an older one (BAK-R1-M3)" '[ "$rc" = 1 ] && [[ $out == *"stale"* ]]'
  mkdir -m 0700 "$t/empty"
  out=$(ONYX_BACKUP_NOW=2026-01-05T000000Z "$self" --check-fresh 30 --dest "$t/empty" 2>&1) && rc=0 || rc=$?
  check "--check-fresh fails when there is no snapshot (BAK-R1-M3)" '[ "$rc" = 1 ]'
  # BAK-R2-L3: a snapshot named in the future is a wrong clock, not a fresh backup.
  out=$(ONYX_BACKUP_NOW=2026-01-01T000000Z "$self" --check-fresh 30 --dest "$d" 2>&1) && rc=0 || rc=$?
  check "--check-fresh fails, exit 1, for a snapshot named in the future (BAK-R2-L3)" '[ "$rc" = 1 ] && [[ $out == *"stale"*"2026-01-04T000002Z is named 72.0 h in the future"* ]]'
  out=$(ONYX_BACKUP_NOW=2026-01-04T000001Z "$self" --check-fresh 30 --dest "$d" 2>&1) && rc=0 || rc=$?
  check "--check-fresh allows a snapshot named less than an hour ahead (clocks a little apart) (BAK-R2-L3)" '[ "$rc" = 0 ] && [[ $out == *"fresh"* ]]'

  # BAK-R2-M3: systemd 259 leaves MONITOR_* empty when two units share one OnFailure= handler, so
  # each unit starts its own instance of a template, which reads the failed unit's result itself.
  local u=$repo/deploy/systemd/user unit argv
  check "each backup unit names its own instance of the alert template, no unit the shared handler (BAK-R2-M3)" '[ "$(grep -c "^OnFailure=" "$u/onyx-node-backup.service")" = 1 ] && [ "$(grep -c "^OnFailure=" "$u/onyx-node-backup-fresh.service")" = 1 ] && grep -qx "OnFailure=onyx-node-backup-alert@%n.service" "$u/onyx-node-backup.service" && grep -qx "OnFailure=onyx-node-backup-alert@%n.service" "$u/onyx-node-backup-fresh.service" && [ -f "$u/onyx-node-backup-alert@.service" ] && [ ! -e "$u/onyx-node-backup-alert.service" ] && ! grep -rq "onyx-node-backup-alert\.service" "$u" && ! grep -q "^ExecStart=.*MONITOR_" "$u/onyx-node-backup-alert@.service" && grep -q -- "--check-fresh" "$u/onyx-node-backup-fresh.service" && grep -q "^OnCalendar=" "$u/onyx-node-backup-fresh.timer"'
  # The template's ExecStart as systemd runs it for each unit (%i: the unit; %%: %; $$: $), with
  # systemctl and logger stand-ins: the line names that unit, its result and its exit status.
  mkdir -m 0700 "$t/alertbin"
  printf '%s\n' '#!/usr/bin/env bash' 'echo "$*" >>"$ALERT_LOG.systemctl"' \
    'case " $* " in *" -p Result "*) echo exit-code;; *" -p ExecMainStatus "*) case $* in *fresh*) echo 1;; *) echo 4;; esac;; esac' >"$t/alertbin/systemctl"
  printf '%s\n' '#!/usr/bin/env bash' 'echo "$*" >>"$ALERT_LOG"' >"$t/alertbin/logger"
  chmod 0755 "$t/alertbin/systemctl" "$t/alertbin/logger"
  for unit in onyx-node-backup.service onyx-node-backup-fresh.service; do
    mapfile -d '' argv < <(python3 -I - "$u/onyx-node-backup-alert@.service" "$unit" <<'PY'
import re, shlex, sys
lines = [l for l in open(sys.argv[1]).read().splitlines() if l.startswith("ExecStart=")]
assert len(lines) == 1, "one ExecStart"
spec = {"i": sys.argv[2], "I": sys.argv[2], "%": "%"}
line = re.sub(r"%(.)", lambda m: spec[m.group(1)], lines[0][len("ExecStart="):]).replace("$$", "\x01")
assert "$" not in line, "a systemd variable in ExecStart (the handler gets no MONITOR_* it can trust)"
sys.stdout.write("\0".join(a.replace("\x01", "$") for a in shlex.split(line)) + "\0")
PY
)
    PATH=$t/alertbin:$PATH ALERT_LOG=$t/alert.log "${argv[@]}" >/dev/null 2>&1 || bad "the alert handler for $unit exited non-zero"
  done
  check "the alert template logs each failed unit with its own result and exit status, read with systemctl show (BAK-R2-M3)" '[ "$(sed -n 1p "$t/alert.log")" = "-t onyx-node-backup-alert -p user.err onyx-node-backup.service failed: result exit-code, exit status 4 (1: failed, the next run continues; 2: refused; 4: a likely loss on the server, see --allow-shrink; journalctl --user -u onyx-node-backup.service)" ] && [[ $(sed -n 2p "$t/alert.log") == "-t onyx-node-backup-alert -p user.err onyx-node-backup-fresh.service failed: result exit-code, exit status 1 "* ]] && grep -qx -- "--user show -p Result --value onyx-node-backup-fresh.service" "$t/alert.log.systemctl"'

  run 2026-01-04T000003Z 3 "$t/missing-root"
  check "a failed run (no source) exits non-zero and leaves no snapshot" '[ "$rc" != 0 ] && [ ! -e "$d/2026-01-04T000003Z" ]'
  run 2026-01-04T000004Z 3
  check "the next run completes and leaves no partial folder" '[ "$rc" = 0 ] && [ -d "$d/2026-01-04T000004Z" ] && [ -z "$(find "$d" -maxdepth 1 -name ".partial-*")" ]'
  run 2026-01-04T000004Z 3
  check "a snapshot name already taken is refused" '[ "$rc" != 0 ]'
  check "a leftover lock file is no lock (every run above reused it)" '[ -f "$d/.lock" ]'
  out=$(flock "$d/.lock" env ONYX_BACKUP_LOCK_WAIT=0 ONYX_BACKUP_LOCAL_ROOT="$src" ONYX_BACKUP_NOW=2026-01-04T000005Z "$self" --dest "$d" 2>&1) && rc=0 || rc=$?
  check "a run while a drill or another run holds the lock fails, exit 1, nothing written" '[ "$rc" = 1 ] && [[ $out == *"holds $d/.lock"* ]] && [ ! -e "$d/2026-01-04T000005Z" ]'
  # BAK-R2-L1: a drill node's scope whose drill is gone (SIGKILL) is stopped before the lock; the
  # scope of a drill still running is left alone. A systemctl stand-in lists both.
  local dead_pid live_pid
  cat >"$t/systemctl" <<'SH'
#!/usr/bin/env bash
case " $* " in
  *" list-units "*) printf '%s\n' "onyx-restore-drill-41000-$DEAD_PID.scope loaded active running onyx restore drill node" \
    "● onyx-restore-drill-41001-$LIVE_PID.scope loaded active running onyx restore drill node" "other-thing.scope loaded active running x";;
  *" stop "*) echo "$*" >>"$SCOPE_LOG";;
esac
SH
  chmod 0755 "$t/systemctl"
  true & dead_pid=$!
  wait "$dead_pid"
  bash -c 'exec -a restore-drill.sh sleep 60' & live_pid=$!
  with SCOPE_LOG="$t/scope.log" DEAD_PID="$dead_pid" LIVE_PID="$live_pid" ONYX_BACKUP_SYSTEMCTL="$t/systemctl" -- 2026-01-04T000006Z 3
  kill "$live_pid" 2>/dev/null; wait "$live_pid" 2>/dev/null || true
  check "a drill node scope whose drill is gone is stopped before the lock, a running drill's is not (BAK-R2-L1)" '[ "$rc" = 0 ] && [ "$(cat "$t/scope.log")" = "--user stop onyx-restore-drill-41000-$dead_pid.scope" ] && [[ $out == *"stopped 1 restore-drill node scope(s) whose drill was gone"* ]]'

  mkdir -m 0755 "$t/open"
  out=$(ONYX_BACKUP_LOCAL_ROOT=$src ONYX_BACKUP_NOW=2026-01-05T000000Z "$self" --dest "$t/open" 2>&1) && rc=0 || rc=$?
  check "a DEST that is not 0700 is refused, nothing written" '[ "$rc" = 2 ] && [ -z "$(ls -A "$t/open")" ]'
  mkdir -p "$t/repo/.git" && mkdir -m 0700 "$t/repo/backups"
  out=$(ONYX_BACKUP_LOCAL_ROOT=$src ONYX_BACKUP_NOW=2026-01-05T000000Z "$self" --dest "$t/repo/backups" 2>&1) && rc=0 || rc=$?
  check "a DEST inside a git checkout is refused" '[ "$rc" = 2 ] && [ -z "$(ls -A "$t/repo/backups")" ] && [[ $out == *"git checkout"* ]]'
  out=$(ONYX_BACKUP_LOCAL_ROOT=$src ONYX_BACKUP_NOW=2026-01-05T000000Z "$self" --dest "$t/repo/new" 2>&1) && rc=0 || rc=$?
  check "a missing DEST inside a git checkout is refused and not made" '[ "$rc" = 2 ] && [ ! -e "$t/repo/new" ]'
  out=$(ONYX_BACKUP_LOCAL_ROOT=$src ONYX_BACKUP_NOW=2026-01-05T000000Z "$self" --dest "$t/fresh/node" 2>&1) && rc=0 || rc=$?
  check "a missing DEST is made 0700" '[ "$rc" = 0 ] && [ "$(stat -c %a "$t/fresh/node")" = 700 ] && [ "$(stat -c %a "$t/fresh")" = 700 ]'
  out=$("$self" --dest "$d" --keep 0 2>&1) && rc=0 || rc=$?
  check "--keep 0 is refused" '[ "$rc" = 2 ]'
  out=$("$self" --bogus 2>&1) && rc=0 || rc=$?
  check "an unknown argument is refused" '[ "$rc" = 2 ]'
  check "no planted secret was ever printed" '! grep -qF "$mark" "$t/all-output" && [[ $out != *"$mark"* ]]'

  echo "backup-node self-test: $pass passed, $failed failed"
  [ "$failed" = 0 ]
}

while [ $# -gt 0 ]; do
  case $1 in
    --self-test) [ $# = 1 ] || die "--self-test takes no other argument"; self_test; exit;;
    --dest) [ $# -ge 2 ] || die "--dest needs a folder"; dest=$2; shift 2;;
    --keep) [ $# -ge 2 ] || die "--keep needs a number"; keep=$2; shift 2;;
    --weeks) [ $# -ge 2 ] || die "--weeks needs a number"; weeks=$2; shift 2;;
    --months) [ $# -ge 2 ] || die "--months needs a number"; months=$2; shift 2;;
    --bwlimit) [ $# -ge 2 ] || die "--bwlimit needs a number"; bwlimit=$2; shift 2;;
    --allow-shrink) allow_shrink=1; shift;;
    --no-verify) verify=; shift;;
    --check-fresh) [ $# -ge 2 ] || die "--check-fresh needs hours"; fresh_hours=$2; shift 2;;
    *) die "unknown argument: $1 (usage: $0 [--dest DIR] [--keep N] [--weeks W] [--months M] [--bwlimit KIB] [--allow-shrink] [--no-verify] | --check-fresh HOURS | --self-test)";;
  esac
done
[[ $keep =~ ^[1-9][0-9]{0,3}$ ]] || die "--keep must be a whole number from 1 to 9999"
[[ $weeks =~ ^[0-9]{1,3}$ ]] || die "--weeks must be a whole number from 0 to 999"
[[ $months =~ ^[0-9]{1,3}$ ]] || die "--months must be a whole number from 0 to 999"
[[ $bwlimit =~ ^[0-9]{1,7}$ ]] || die "--bwlimit must be KiB/s, 0 for no cap"
[[ $tries =~ ^[1-9]$ ]] || die "ONYX_BACKUP_TRIES must be 1 to 9"
[[ $retry_secs =~ ^[0-9]{1,4}$ ]] || die "ONYX_BACKUP_RETRY_SECS must be 0 to 9999"
[[ $lock_wait =~ ^[0-9]{1,6}$ ]] || die "ONYX_BACKUP_LOCK_WAIT must be seconds"
umask 077
dest=$(realpath -m -- "$dest")
list_snaps() { find "$dest" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' 2>/dev/null | grep -E "$snap_re" | sort || true; }

# --check-fresh: the newest complete snapshot (one with SNAPSHOT.json) against now.
if [ -n "$fresh_hours" ]; then
  [[ $fresh_hours =~ ^[1-9][0-9]{0,4}$ ]] || die "--check-fresh needs whole hours"
  newest=
  while read -r s; do [ -f "$dest/$s/SNAPSHOT.json" ] && newest=$s; done < <(list_snaps)
  rc=0
  python3 -I - "$newest" "$fresh_hours" "$now" "$dest" <<'PY' || rc=$?
import datetime as dt, sys
newest, hours, now, dest = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
fmt = "%Y-%m-%dT%H%M%SZ"
t = dt.datetime.strptime(now, fmt) if now else dt.datetime.now(dt.timezone.utc).replace(tzinfo=None)
if not newest:
    print(f"backup-node: stale: no complete snapshot in {dest}")
    sys.exit(1)
age = (t - dt.datetime.strptime(newest, fmt)).total_seconds() / 3600
# A snapshot named in the future says nothing about now: a wrong clock, now or when it was taken.
if age < -1:
    print(f"backup-node: stale: the newest snapshot {newest} is named {-age:.1f} h in the future: "
          "the clock (now or when it was taken) is wrong")
    sys.exit(1)
if age > hours:
    print(f"backup-node: stale: the newest snapshot {newest} is {age:.1f} h old (limit {hours} h)")
    sys.exit(1)
print(f"backup-node: fresh: the newest snapshot {newest} is {age:.1f} h old (limit {hours} h)")
PY
  exit "$rc"
fi

# DEST: never inside a git checkout (a snapshot holds node.env), 0700, ours.
inside_git() {
  local p=$1
  while :; do
    [ -e "$p/.git" ] && return 0
    [ "$p" = / ] && return 1
    p=$(dirname "$p")
  done
}
inside_git "$dest" && die "$dest is inside a git checkout: snapshots hold node.env and the state; pick a folder outside every repository"
if [ ! -e "$dest" ]; then
  mkdir -p -- "$dest"   # umask 077: every folder it makes is 0700
fi
[ -d "$dest" ] && [ ! -L "$dest" ] || die "$dest is not a folder"
[ "$(stat -c %a "$dest")" = 700 ] || die "$dest must be mode 0700 (it is $(stat -c %a "$dest")): chmod 700 it first"
[ "$(stat -c %u "$dest")" = "$(id -u)" ] || die "$dest is not owned by you"

# A drill's node runs in a scope of its own (systemd-run --scope), outside the drill's cgroup: a
# drill killed by SIGKILL, or a backup killed with its drill, leaves it running for up to its
# RuntimeMaxSec. Each scope is onyx-restore-drill-<port>-<drill pid>.scope; one whose drill process
# is gone is stopped (review BAK-R2-L1). A drill still running keeps its node and its shared lock.
stop_stale_drills() {
  local sc=${systemctl_cmd:-systemctl} u p n=0
  [ -n "$systemctl_cmd" ] || [ -z "$local_root" ] || return 0
  while read -r u; do
    [[ $u =~ ^onyx-restore-drill-[0-9]+-([0-9]+)\.scope$ ]] || continue
    p=${BASH_REMATCH[1]}
    if [ -r "/proc/$p/cmdline" ] && tr '\0' ' ' <"/proc/$p/cmdline" 2>/dev/null | grep -q restore-drill; then
      continue
    fi
    "$sc" --user stop "$u" >/dev/null 2>&1 || true
    n=$((n + 1))
  done < <("$sc" --user list-units --type=scope --all --plain --no-legend 'onyx-restore-drill-*' 2>/dev/null \
             | grep -oE 'onyx-restore-drill-[0-9]+-[0-9]+\.scope' || true)
  [ "$n" = 0 ] || echo "backup-node: note: stopped $n restore-drill node scope(s) whose drill was gone" >&2
}
stop_stale_drills

# The lock is held on the open file, not by the file: a .lock a killed run left is no lock. A drill
# holds it shared (scripts/restore-drill.sh), so no run prunes a snapshot under a drill.
exec 9>"$dest/.lock"
flock -w "$lock_wait" 9 || { echo "backup-node: a drill or another backup-node run holds $dest/.lock (waited $lock_wait s); the next run tries again" >&2; exit 1; }

name=${now:-$(date -u +%Y-%m-%dT%H%M%SZ)}
[[ $name =~ $snap_re ]] || die "bad snapshot name: $name"
[ ! -e "$dest/$name" ] || die "$dest/$name exists already"
prev=$(list_snaps | tail -n 1)
# A clock behind the newest snapshot would sort the new one oldest, and pruning would delete it.
if [ -n "$prev" ] && [[ ! $name > $prev ]]; then
  die "the clock says $name, at or before the newest snapshot $prev: fix the clock first"
fi
# A drill killed before its trap ran leaves its scratch folder (with a node.env copy).
find "$dest" -mindepth 1 -maxdepth 1 -type d -name '.drill.*' -mmin +1440 -exec rm -rf -- {} +
part=$dest/.partial-$name
# A failed run's partial folder is the starting point (its files are already here); older ones go.
resumed=
old_partials=$(find "$dest" -mindepth 1 -maxdepth 1 -type d -name '.partial-*' -printf '%f\n' | sort)
if [ -n "$old_partials" ]; then
  resumed=$(printf '%s\n' "$old_partials" | tail -n 1)
  mv -- "$dest/$resumed" "$part"
  printf '%s\n' "$old_partials" | head -n -1 | while read -r p; do [ -n "$p" ] && rm -rf -- "${dest:?}/$p"; done
else
  mkdir -- "$part"
fi
t0=$SECONDS
started=$(date -u +%Y-%m-%dT%H:%M:%SZ)

ssh_opts=(-o BatchMode=yes -o ConnectTimeout=20 -o ServerAliveInterval=15 -o ServerAliveCountMax=4)
if [ -n "$local_root" ]; then
  src_of() { printf '%s%s/' "$local_root" "$1"; }
  rs_remote=()
  source_label=local
else
  src_of() { printf '%s:%s/' "$host" "$1"; }
  rs_remote=(-e "ssh ${ssh_opts[*]}" --rsync-path="sudo -n nice -n 19 ionice -c3 rsync")
  source_label=$host
fi
rs_cap=()
[ "$bwlimit" = 0 ] || rs_cap=(--bwlimit="$bwlimit")
stats=$part/.rsync-stats
: >"$stats"
last_changed=0
pull() { # part source-path [rsync filter args...]: one rsync, tried again after a dropped connection
  local p=$1 from=$2 rc try=1 one=$part/.rsync-one
  shift 2
  local link=()
  [ -n "$prev" ] && [ -d "$dest/$prev/$p" ] && link=(--link-dest="$dest/$prev/$p")
  mkdir -p -- "$part/$p"
  while :; do
    rc=0
    # -rlpt: no owner, group or device files; --chmod makes every file 0600 and folder 0700.
    "$rsync_cmd" -rlpt --chmod=D0700,F0600 --delete --numeric-ids --stats --timeout=300 \
      "${rs_cap[@]}" "${rs_remote[@]}" "${link[@]}" "$@" "$(src_of "$from")" "$part/$p/" >"$one" || rc=$?
    cat "$one" >>"$stats"
    case $rc in
      0) break;;
      # A file vanished during the copy (a tier move, an erase): the rest is whole.
      24) echo "backup-node: note: files vanished during the copy of $p (moved by the node); kept the rest" >&2; break;;
      # A dropped or stalled connection (socket, protocol stream, timeout, ssh).
      10|12|30|35|255)
        if [ "$try" -lt "$tries" ]; then
          echo "backup-node: rsync of $p: connection lost (exit $rc), try $((try + 1)) of $tries in $((retry_secs * try)) s" >&2
          sleep "$((retry_secs * try))"
          try=$((try + 1))
          continue
        fi;;
    esac
    echo "backup-node: rsync of $p failed (exit $rc, try $try of $tries); the partial copy stays in $part for the next run" >&2
    exit 1
  done
  # Files this pull changed: copied plus deleted (rsync --stats).
  last_changed=$(python3 -I -c '
import re, sys
n = 0
for line in open(sys.argv[1]):
    m = re.match(r"Number of (regular files transferred|deleted files): ([\d,]+)", line)
    if m:
        n += int(m.group(2).replace(",", ""))
print(n)' "$one")
  rm -f -- "$one"
}

# The server's build (the binary's sha256), read before and after the copy: a deploy in between
# could mix two builds' stores in one snapshot, so it fails the run.
server_build() {
  local try=1 out
  if [ -n "$local_root" ]; then
    out=$(sha256sum "$local_root/usr/local/bin/onyx-node" 2>/dev/null) || out=
    printf '%s' "${out%% *}"
    return 0
  fi
  while :; do
    if out=$(ssh "${ssh_opts[@]}" "$host" 'sha256sum /usr/local/bin/onyx-node' 2>/dev/null); then
      printf '%s' "${out%% *}"
      return 0
    fi
    [ "$try" -lt "$tries" ] || { echo "backup-node: could not read the server's build over ssh ($tries tries)" >&2; return 1; }
    sleep "$((retry_secs * try))"
    try=$((try + 1))
  done
}
bin_sha=$(server_build) || exit 1
health=
[ -n "$local_root" ] || health=$(ssh "${ssh_opts[@]}" "$host" 'curl -fsS -m 10 http://127.0.0.1:8080/v1/health' 2>/dev/null) || health=

# Bulk store: tiers.json first, then the rest, again until a pass changes nothing (see the header).
passes=0
quiet=false
while [ "$passes" -lt "$max_passes" ]; do
  passes=$((passes + 1))
  pull bulk /var/lib/onyx/bulk --include=/tiers.json --exclude='*'
  changed=$last_changed
  pull bulk /var/lib/onyx/bulk --exclude=/tiers.json
  changed=$((changed + last_changed))
  if [ "$passes" -gt 1 ] && [ "$changed" = 0 ]; then quiet=true; break; fi
done
if [ "$quiet" != true ]; then
  echo "backup-node: the bulk store was still changing after $max_passes passes (an upload in progress); nothing was pruned, the partial copy stays in $part for the next run" >&2
  exit 1
fi
pull state /var/lib/onyx/state
pull cas /var/lib/onyx/cas
pull etc-onyx /etc/onyx --exclude='*.key'
# The binary and its unit (and drop-ins); nothing else of either folder.
pull server /usr/local/bin --include=/onyx-node --exclude='*'
pull server /etc/systemd/system --include=/onyx-node.service --include='/onyx-node.service.d/***' --exclude='*'
bin_after=$(server_build) || exit 1
copied_sha=$(sha256sum "$part/server/onyx-node" 2>/dev/null | cut -d' ' -f1) || copied_sha=
if [ "$bin_after" != "$bin_sha" ] || { [ -n "$copied_sha" ] && [ "$copied_sha" != "$bin_sha" ]; }; then
  echo "backup-node: the server's onyx-node changed during the copy (a deploy); the partial copy stays in $part for the next run" >&2
  exit 1
fi

# SNAPSHOT.json: counts, bytes and hashes only, never a file's content. Exit 3: the copied
# tiers.json names a shard the copy does not hold; exit 4: the copy looks like a loss on the server
# (a required file missing, a shrink against the kept snapshots: see --allow-shrink above).
rm -f -- "$part/SNAPSHOT.json" "$part/DRILL.txt"
rc=0
summary=$(HEALTH=$health python3 -I - "$part" "$name" "$started" "$((SECONDS - t0))" "$source_label" "${prev:-}" "$bin_sha" "$passes" "$quiet" "$bwlimit" "$resumed" "$dest" "$allow_shrink" <<'PY'
import hashlib, json, os, re, sys
part, name, started, secs, source, prev, bin_sha, passes, quiet, bwlimit, resumed, dest, allow_shrink = sys.argv[1:14]
def walk(root):
    files = size = 0
    for d, _, fs in os.walk(root):
        for f in fs:
            st = os.lstat(os.path.join(d, f))
            files += 1
            size += st.st_size
    return {"files": files, "bytes": size}
def names(root, p):
    """The part's files by relative name; a shard by its kind and root alone (a tier move renames its
    path, not the shard). Leftover *.tmp files and the journals' *.lock files do not count."""
    out = set()
    for d, _, fs in os.walk(root):
        for f in fs:
            if f.endswith((".tmp", ".lock")):
                continue
            rel = os.path.relpath(os.path.join(d, f), root)
            m = re.fullmatch(r"(?:hot|warm|cold)/(bulk|rows)/([0-9a-f]{64})", rel) if p == "bulk" else None
            out.add(f"{m.group(1)}:{m.group(2)}" if m else rel)
    return out
PARTS = ("bulk", "state", "cas", "etc-onyx", "server")
# A copy missing what a node needs is a likely loss on the server, not a backup. The node makes
# wants.salt at its first start and reads it at every start (crates/onyx-api/src/wants.rs).
need = ["bulk/tiers.json", "etc-onyx/node.env", "etc-onyx/deployment.json", "state/wants.salt",
        "server/onyx-node", "server/onyx-node.service"]
lost = [f"no {p}" for p in need if not os.path.isfile(os.path.join(part, p))]
parts = {p: walk(os.path.join(part, p)) for p in PARTS}
# The kept snapshots the copy is compared with (review BAK-R2-M1): every one from the newest taken
# with --allow-shrink (a loss the operator accepted by hand) on, not only the previous one, so a loss
# of a few percent a day is refused once it adds up to more than 10%.
snap_re = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{6}Z")
kept = sorted(n for n in os.listdir(dest) if snap_re.fullmatch(n) and os.path.isdir(os.path.join(dest, n)))
info = {}
for s in kept:
    try:
        with open(os.path.join(dest, s, "SNAPSHOT.json")) as f:
            info[s] = json.load(f)
    except (OSError, ValueError):
        info[s] = {}
base = kept[max([i for i, s in enumerate(kept) if info[s].get("allowedShrink")], default=0):]
counted = {}
def count(s, p):
    if (s, p) not in counted:
        try:
            c = info[s]["parts"][p]
            counted[s, p] = {"files": int(c["files"]), "bytes": int(c["bytes"])}
        except (KeyError, TypeError, ValueError):
            counted[s, p] = walk(os.path.join(dest, s, p))
    return counted[s, p]
compared = set()
# The bulk store and the row batches: totals against the largest kept snapshot, and the files gone
# by name since the previous, the largest and the oldest one (new uploads do not hide a loss).
for p in ("bulk", "cas"):
    if not base:
        break
    for k in ("files", "bytes"):
        top = max(base, key=lambda s: count(s, p)[k])
        was, now = count(top, p)[k], parts[p][k]
        compared.add(top)
        if now < was * 0.9:
            lost.append(f"{p} {k} {now} against {was} in {top} (down more than 10%)")
    have = names(os.path.join(part, p), p)
    largest = max(base, key=lambda s: count(s, p)["files"])
    for s in dict.fromkeys(x for x in (prev, largest, base[0]) if x):
        if not os.path.isdir(os.path.join(dest, s, p)):
            continue
        had = names(os.path.join(dest, s, p), p)
        gone = len(had - have)
        compared.add(s)
        if gone > len(had) * 0.1:
            lost.append(f"{p}: {gone} of the {len(had)} files of {s} are gone (more than 10%; new files do not count)")
# state/ and etc-onyx/ (review BAK-R2-M2): every file of the previous snapshot is still there (the
# journals, the salt, the config), and no state file was emptied. Names only, never content.
if prev:
    for p in ("state", "etc-onyx"):
        gone = sorted(names(os.path.join(dest, prev, p), p) - names(os.path.join(part, p), p))
        if gone:
            lost.append(f"{p}: {len(gone)} file{'' if len(gone) == 1 else 's'} of {prev} gone ("
                        + ", ".join(gone[:5]) + (", ..." if len(gone) > 5 else "") + ")")
    for f in sorted(names(os.path.join(part, "state"), "state")):
        a = os.path.join(dest, prev, "state", f)
        if os.path.isfile(a) and os.path.getsize(a) > 0 and os.path.getsize(os.path.join(part, "state", f)) == 0:
            lost.append(f"state/{f} is empty (it was not in {prev})")
# The files node.env names under /etc/onyx (the deployment, the catalog, observations, the
# publication track) must be in the copy: the setting's name is printed, never its value. A *.key
# is never copied (the owner's offline copy).
env = os.path.join(part, "etc-onyx", "node.env")
if os.path.isfile(env):
    missing_for = []
    with open(env, encoding="utf-8", errors="replace") as f:
        for raw in f:
            line = raw.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = (x.strip() for x in line.split("=", 1))
            if len(v) >= 2 and v[0] == v[-1] and v[0] in "\"'":
                v = v[1:-1]
            for item in (i.strip() for i in v.split(",")):
                if item.startswith("/etc/onyx/") and not item.endswith(".key"):
                    rel = os.path.normpath(item[len("/etc/onyx/"):])
                    if (rel.startswith("..") or not os.path.exists(os.path.join(part, "etc-onyx", rel))) \
                            and k not in missing_for:
                        missing_for.append(k)
    lost += [f"etc-onyx: the file {k} names is not in the copy" for k in missing_for]
if lost and not allow_shrink:
    print("backup-node: refused, the copy looks like a loss on the server: " + "; ".join(lost)
          + ". Nothing was pruned; check the server, or run again with --allow-shrink to take it", file=sys.stderr)
    sys.exit(4)
# TieredStore::load: each entry "<tier>" (rows) or "bulk:<tier>" (a shard); hot and warm must be on
# disk at <bulk>/<tier>/<rows|bulk>/<root hex>, a non-empty regular file.
bulk = os.path.join(part, "bulk")
tiers = {"hot": 0, "warm": 0, "cold": 0}
missing = 0
tf = os.path.join(bulk, "tiers.json")
if os.path.exists(tf):
    with open(tf) as f:
        t = json.load(f)
    for root, tier in t.items():
        kind, _, where = tier.rpartition(":")
        if kind not in ("", "bulk") or where not in tiers or (kind and where == "cold") \
                or not re.fullmatch(r"[0-9a-f]{64}", root):
            print("backup-node: tiers.json holds an entry the node does not write", file=sys.stderr)
            sys.exit(3)
        tiers[where] += 1
        if where == "cold":
            continue
        p = os.path.join(bulk, where, "bulk" if kind else "rows", root)
        if not (os.path.isfile(p) and not os.path.islink(p) and os.path.getsize(p) > 0):
            missing += 1
if missing:
    print(f"backup-node: tiers.json names {missing} shard(s) not in the copy (an upload or a tier move "
          "during the copy, or a warm folder outside the bulk store)", file=sys.stderr)
    sys.exit(3)
copied_files = copied_bytes = 0
for line in open(os.path.join(part, ".rsync-stats")):
    m = re.match(r"Number of regular files transferred: ([\d,]+)", line)
    if m:
        copied_files += int(m.group(1).replace(",", ""))
    m = re.match(r"Total transferred file size: ([\d,]+) bytes", line)
    if m:
        copied_bytes += int(m.group(1).replace(",", ""))
state = os.path.join(part, "state")
state_sha = {}
for f in sorted(os.listdir(state)):
    p = os.path.join(state, f)
    if os.path.isfile(p) and not os.path.islink(p):
        with open(p, "rb") as h:
            state_sha[f] = hashlib.sha256(h.read()).hexdigest()
health = None
try:
    h = json.loads(os.environ.get("HEALTH") or "null")
    if isinstance(h, dict):
        health = {k: h.get(k) for k in ("status", "chainId", "contract", "rights")}
except ValueError:
    pass
snap = {
    "format": 2,
    "name": name,
    "started": started,
    "seconds": int(secs),
    "source": source,
    # The node names no commit (/v1/health has no build field): the build is the binary's sha256,
    # and the binary itself is server/onyx-node.
    "build": f"sha256:{bin_sha}" if bin_sha else None,
    "serverBinarySha256": bin_sha or None,
    "health": health,
    "linkedFrom": prev or None,
    "resumedFrom": resumed or None,
    "copied": {"files": copied_files, "bytes": copied_bytes},
    "bulkPasses": int(passes),
    "quiet": quiet == "true",
    "bwlimitKiBps": int(bwlimit) or None,
    "allowedShrink": lost or None,
    "comparedWith": sorted(compared) or None,
    "parts": parts,
    "tiers": tiers,
    "stateSha256": state_sha,
    "excluded": ["etc-onyx: *.key (an offline copy of the keys is the owner's job)"],
}
fd = os.open(os.path.join(part, "SNAPSHOT.json"), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, "w") as f:
    json.dump(snap, f, indent=1, sort_keys=True)
    f.write("\n")
total_f = sum(p["files"] for p in parts.values())
total_b = sum(p["bytes"] for p in parts.values())
print(f"copied {copied_files} files, {copied_bytes} bytes; snapshot {total_f} files, {total_b} bytes", end="")
print(f" (rest hard-linked from {prev})" if prev else "", end="")
print(f"; continued {resumed}" if resumed else "", end="")
print(f"; {passes} bulk passes", end="")
print("; taken with --allow-shrink" if lost else "", end="")
PY
) || rc=$?
case $rc in
  0) ;;
  4) exit 4;;
  *) echo "backup-node: the copy is not a store the node can open; the partial copy stays in $part for the next run" >&2; exit 1;;
esac

# The restore drill on the copy (scripts/restore-drill.sh --full): only a copy a node opens and
# serves as the live node does becomes a snapshot. It runs under this run's lock.
drill_line=
if [ -n "$verify" ] && [ -n "$verify_cmd" ]; then
  rc=0
  # The drill compares with the live node over ssh to this host when the public URL fails.
  # shellcheck disable=SC2086 # the command and its flag, split on purpose
  ONYX_DRILL_UNDER_BACKUP=1 ONYX_DRILL_SSH_HOST=$host $verify_cmd "$part" >"$part/DRILL.txt" 2>&1 || rc=$?
  chmod 0600 "$part/DRILL.txt"
  drill_line=$(tail -n 1 "$part/DRILL.txt")
  if [ "$rc" != 0 ]; then
    grep -E '^FAIL' "$part/DRILL.txt" | cut -c1-300 >&2 || true
    echo "backup-node: the restore drill on the copy failed ($drill_line); nothing was pruned, the partial copy stays in $part for the next run" >&2
    exit 1
  fi
fi
rm -f -- "$stats"
# Flushed to disk before it counts as a snapshot, and the rename after it.
"$sync_cmd" -f "$part"
mv -- "$part" "$dest/$name"
"$sync_cmd" -f "$dest"

# Retention (PYKEEP above); nothing else in DEST is touched.
pruned=0
while read -r old; do
  [ -n "$old" ] || continue
  [[ $old =~ $snap_re ]] || continue
  [ "$old" != "$name" ] || continue
  rm -rf -- "${dest:?}/$old"
  pruned=$((pruned + 1))
done < <(list_snaps | python3 -I -c "$PYKEEP" "$keep" "$weeks" "$months")
echo "backup ok: $name in $((SECONDS - t0)) s: $summary${drill_line:+; drill: $drill_line}; kept $(list_snaps | wc -l), pruned $pruned"
