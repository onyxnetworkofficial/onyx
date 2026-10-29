#!/usr/bin/env bash
# Build-host side of scripts/remote-check.sh (ROADMAP E13, docs/ops/remote-check.md, THREATS
# REMOTE-1..5). scripts/remote-check.sh copies this file to ~/onyx-build/bin/ on the host before
# every call; nothing else on the host runs it. Everything it writes or deletes is under
# ~/onyx-build (sources, targets, logs, a private HOME and TMPDIR) or ~/onyx-tools (copied tools).
#   remote-check-runner.sh run <branch> [ONYX_NAME=VALUE ...]  check-all on ~/onyx-build/src/<branch>
#   remote-check-runner.sh clean <branch>    delete ~/onyx-build/src/<branch> and target/<branch> only
#   remote-check-runner.sh guard <load1> <free_gb> <used_gb>   the start rules alone (self-test)
#   remote-check-runner.sh slot <seconds>    take a build slot and release it (self-test)
#   remote-check-runner.sh sandbox RULE... -- CMD...  CMD under the Landlock rules
#   remote-check-runner.sh policy <branch>   the job's Landlock rules, one per line (self-test)
#   remote-check-runner.sh job ...           internal: the scoped part of `run`
# The host is a shared production machine: every job runs in a systemd user scope capped at 48 GB of
# memory (no swap) and 48 CPUs, at nice 19 and idle I/O priority, under `env -i` (no variable of the
# host's own sessions reaches the build) and a Landlock file-system sandbox (THREATS REMOTE-4: the
# job writes only its branch's folders, the private HOME and the crate cache, reads only the system
# and Onyx's two folders; never the rest of the home, /mnt or /tmp). It refuses to start at a
# 1-minute load above 60, with under 60 GB free on /, or with Onyx's two folders over 80 GB; a
# watchdog stops the job's own scope when either disk rule breaks during the run, and the scope is
# stopped when the job ends (leftover test servers or browsers) or the caller goes away.
# One { ... } block, parsed whole before it runs (THREATS REMOTE-9). Paths it prints start with ~,
# never the host's home folder (it names the host's account).
{
set -uo pipefail

B=${ONYX_REMOTE_BASE:-$HOME/onyx-build}
T=${ONYX_REMOTE_TOOLS:-$HOME/onyx-tools}
MAX_LOAD=60
MIN_FREE_GB=60
BUDGET_GB=80
# During a run the job is stopped below this much free space (the start rule keeps 60 GB).
STOP_FREE_GB=45
GB=1000000000
# The host's home as the caller's log shows it: ~ (the job's own HOME is private, so `run` passes it).
HOST_HOME=${ONYX_REMOTE_HOME:-$HOME}
show() { local p=$1; [ "${p#"$HOST_HOME"/}" = "$p" ] || p="~/${p#"$HOST_HOME"/}"; printf '%s' "$p"; }

die() { echo "remote-check: $*" >&2; exit 2; }

# Landlock launcher (THREATS REMOTE-4; no root, no user namespace: the build machine's AppArmor blocks
# unprivileged user namespaces, so systemd's ProtectHome= and friends are silently ignored in a
# user unit there, checked 2026-10-19). Arguments: KIND:PATH rules, `--`, the command. rx = read and
# execute, dev = read and write files (no create or delete), rw = everything; every other path is
# denied for every handled right. A missing path is skipped. Fails closed (exit 3) when the kernel
# has no Landlock. Not covered by Landlock on the host's kernel 6.8: signals to the same user's
# processes, Unix-socket connects, TCP (REMOTE-4 stays PARTIAL).
LANDLOCK_PY=$(
  cat <<'PY'
import ctypes, os, struct, sys
def refuse(m):
    print("remote-check: REFUSED: " + m, file=sys.stderr)
    sys.exit(3)
libc = ctypes.CDLL(None, use_errno=True)
libc.syscall.restype = ctypes.c_long
L = ctypes.c_long
abi = libc.syscall(L(444), None, L(0), L(1))  # landlock_create_ruleset(NULL, 0, VERSION)
if abi < 1:
    refuse("this host has no Landlock (errno %d), the job's file-system sandbox (THREATS REMOTE-4)" % ctypes.get_errno())
handled = (1 << 13) - 1                 # ABI 1: execute .. make_sym
if abi >= 2: handled |= 1 << 13         # refer (rename or link across folders)
if abi >= 3: handled |= 1 << 14         # truncate
FILE = 1 | 2 | 4 | (1 << 14)            # the rights a rule on a file (not a folder) may hold
KIND = {"rx": 1 | 4 | 8, "dev": 2 | 4 | 8 | (1 << 14), "rw": handled}
rfd = libc.syscall(L(444), ctypes.byref(ctypes.c_uint64(handled)), L(8), L(0))
if rfd < 0:
    refuse("landlock_create_ruleset errno %d" % ctypes.get_errno())
args = sys.argv[1:]
cut = args.index("--")
for spec in args[:cut]:
    kind, path = spec.split(":", 1)
    try:
        fd = os.open(path, os.O_PATH | os.O_CLOEXEC)
    except FileNotFoundError:
        continue
    allowed = KIND[kind] & handled
    if not os.path.isdir(path):
        allowed &= FILE
    rule = ctypes.create_string_buffer(struct.pack("=Qi", allowed, fd), 12)
    if libc.syscall(L(445), L(rfd), L(1), rule, L(0)) != 0:  # LANDLOCK_RULE_PATH_BENEATH
        refuse("landlock_add_rule %s errno %d" % (path, ctypes.get_errno()))
    os.close(fd)
if libc.prctl(38, ctypes.c_ulong(1), ctypes.c_ulong(0), ctypes.c_ulong(0), ctypes.c_ulong(0)) != 0:
    refuse("no_new_privs errno %d" % ctypes.get_errno())
if libc.syscall(L(446), L(rfd), L(0)) != 0:
    refuse("landlock_restrict_self errno %d" % ctypes.get_errno())
os.close(rfd)
os.execvp(args[cut + 1], args[cut + 1:])
PY
)
# The job's rules: the system read-only, Onyx's two folders read-only, and writable only this
# branch's sources, target and TMPDIR, the private HOME (pnpm store, caches) and the crate cache.
policy() { # branch
  local p
  for p in /usr /bin /sbin /lib /lib32 /lib64 /libx32 /opt /etc /sys /run/systemd/resolve /var/cache/fontconfig "$B" "$T"; do
    printf '%s\n' "rx:$p"
  done
  printf '%s\n' dev:/proc dev:/dev rw:/dev/shm "rw:$B/src/$1" "rw:$B/target/$1" "rw:$B/tmp/$1" "rw:$B/home" "rw:$T/cargo-home"
}

valid_branch() { # the name used under src/ and target/ (remote-check.sh maps / to -)
  [[ $1 =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$ ]] && [[ $1 != *..* ]]
}

valid_env() { # NAME=VALUE passed through to check-all
  [[ $1 =~ ^ONYX_[A-Z0-9_]+=[A-Za-z0-9_.:,/+-]*$ ]] || return 1
  case ${1%%=*} in ONYX_REMOTE_* | ONYX_DOCKER | ONYX_RESERVE_DIR | ONYX_LIVE_DIR) return 1 ;; esac
}

guard() { # load1 free_gb used_gb: exit status 0 = may start
  local load=$1 free=$2 used=$3
  if awk -v l="$load" -v m="$MAX_LOAD" 'BEGIN { exit !(l > m) }'; then
    echo "remote-check: REFUSED: the host's 1-minute load is $load (limit $MAX_LOAD); try later"; return 3
  fi
  if [ "$free" -lt "$MIN_FREE_GB" ]; then
    echo "remote-check: REFUSED: / has $free GB free (at least $MIN_FREE_GB GB needed)"; return 3
  fi
  if [ "$used" -gt "$BUDGET_GB" ]; then
    echo "remote-check: REFUSED: Onyx uses $used GB on the host (budget $BUDGET_GB GB); free some with scripts/remote-check.sh --clean <branch>"; return 3
  fi
  return 0
}

free_gb() { echo $(($(df -B1 --output=avail / | sed -n 2p) / GB)); }
used_gb() { # ~/onyx-build plus ~/onyx-tools, disk usage (hard links once)
  local b
  b=$(ionice -c 3 nice -n 19 du -sc -B1 "$B" "$T" 2>/dev/null | awk 'END { print $1 }')
  echo $((${b:-0} / GB))
}
# At most MAX_JOBS runs at once: each scope is capped (48 GB, 48 CPUs), the sum of several is not
# (THREATS REMOTE-10). A run waits for a free slot, a flock on ~/onyx-build/locks/slot-N held by
# this runner on fd 9 (never by the job or its leftovers), up to $1 seconds, then exits 3.
MAX_JOBS=2
take_slot() { # wait seconds
  local waited=0 i
  mkdir -p "$B/locks" || return 3
  while :; do
    for i in $(seq 1 "$MAX_JOBS"); do
      exec 9>>"$B/locks/slot-$i"
      if flock -n 9; then
        echo "remote-check: build slot $i of $MAX_JOBS"
        return 0
      fi
      exec 9>&-
    done
    if [ "$waited" -ge "$1" ]; then
      echo "remote-check: REFUSED: $MAX_JOBS Onyx runs already on the host (waited $waited s); try later"
      return 3
    fi
    [ "$waited" = 0 ] && echo "remote-check: $MAX_JOBS Onyx runs already on the host: waiting for one to end"
    sleep 30
    waited=$((waited + 30))
  done
}
usage_line() { echo "host disk: / $(free_gb) GB free; Onyx uses $(used_gb) GB of $BUDGET_GB GB"; }

# Port of scripts/sweep-target.py over every deps folder of a target dir: for each (crate, kind)
# keep the newest file; older duplicates (old hashes from earlier builds) are deleted; files
# changed in the last 30 minutes are never touched.
sweep() { # target dir, its name in the log
  [ -d "$1" ] || return 0
  python3 -I - "$1" "${2:-$1}" <<'PY'
import os, re, sys, time
total, n, now = 0, 0, time.time()
for top, dirs, _ in os.walk(sys.argv[1]):
    if os.path.basename(top) != "deps":
        continue
    dirs[:] = []
    groups = {}
    for f in os.listdir(top):
        p = os.path.join(top, f)
        if not os.path.isfile(p) or os.path.islink(p):
            continue
        m = re.match(r'^(lib)?(.+?)-([0-9a-f]{16})(\..*)?$', f)
        if not m:
            continue
        groups.setdefault((m.group(1) or '', m.group(2), m.group(4) or ''), []).append((os.path.getmtime(p), p))
    for files in groups.values():
        files.sort(reverse=True)
        for mt, p in files[1:]:
            if now - mt < 1800:
                continue
            total += os.path.getsize(p); n += 1
            os.remove(p)
print(f"sweep {sys.argv[2]}: {n} old duplicate files, {total/1e9:.1f} GB deleted")
PY
}

case ${1:-} in
sandbox)
  shift
  exec python3 -I -c "$LANDLOCK_PY" "$@"
  ;;
policy)
  [ $# = 2 ] || die "policy <branch>"
  valid_branch "$2" || die "not a branch name: '$2'"
  policy "$2"
  exit 0
  ;;
slot)
  [ $# = 2 ] || die "slot <wait seconds>"
  take_slot "$2"
  exit
  ;;
guard)
  [ $# = 4 ] || die "guard <load1> <free_gb> <used_gb>"
  guard "$2" "$3" "$4"
  exit
  ;;
clean)
  [ $# = 2 ] || die "clean <branch>"
  valid_branch "$2" || die "not a branch name: '$2'"
  for d in "$B/src/$2" "$B/target/$2"; do
    if [ -e "$d" ]; then rm -rf -- "$d" && echo "deleted $(show "$d")"; else echo "absent $(show "$d")"; fi
  done
  usage_line
  exit 0
  ;;
job)
  # Inside the scope, under env -i. $2 branch, then NAME=VALUE pairs (checked again here).
  br=$2
  shift 2
  valid_branch "$br" || die "not a branch name: '$br'"
  src=$B/src/$br
  meta=$src/.git/onyx-remote
  cd "$src" || die "no sources in $(show "$src")"
  export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
  export HOME=$B/home XDG_CACHE_HOME=$B/home/.cache XDG_CONFIG_HOME=$B/home/.config XDG_DATA_HOME=$B/home/.local/share
  export TMPDIR=$B/tmp/$br
  export PATH=$T/bin:$T/rust/bin:$T/node/bin:$T/foundry/bin:/usr/local/bin:/usr/bin:/bin
  export CARGO_HOME=$T/cargo-home CARGO_TARGET_DIR=$B/target/$br CARGO_INCREMENTAL=0
  # Line tables only in dev and test builds (backtraces keep file:line): a full-debuginfo target is
  # about 35 GB, two would break the 80 GB budget; no check reads debuginfo (decision 2026-10-19).
  export CARGO_PROFILE_DEV_DEBUG=line-tables-only
  # rayon's global pool takes one thread per CPU: on the 96-thread host it cannot start inside the
  # 700 MB address space of host.rs's OOM probe (THREATS REMOTE-5); 16 is above the dev box's 10.
  export RAYON_NUM_THREADS=16
  export FOUNDRY_BIN=$T/foundry/bin
  export ONYX_RESERVE_DIR=$B/fixtures-live/tail ONYX_LIVE_DIR=$B/fixtures-live/node ONYX_DOCKER=0
  # pnpm uses its store first and the registry only for what is missing (the first run fills it).
  export npm_config_prefer_offline=true
  for kv in "$@"; do
    valid_env "$kv" || die "refused variable '${kv%%=*}'"
    export "${kv?}"
  done
  # TMPDIR, the target and the crate cache were made by `run` (the sandbox cannot make them).
  mkdir -p "$HOME/.local/share" "$HOME/.foundry" || exit 1
  ln -sfn "$T/svm" "$HOME/.local/share/svm"
  ln -sfn "$T/foundry/bin" "$HOME/.foundry/bin"
  step() { echo "== remote-check: $* ($(date -u +%H:%M:%SZ))"; }

  step "index of the synced tree"
  # Files gone since the last sync go (only paths the previous list named, all inside $src).
  if [ -f "$meta/files.prev" ]; then
    python3 -I - "$meta/files.prev" "$meta/files.list" <<'PY' || exit 1
import os, sys
old = set(open(sys.argv[1], 'rb').read().split(b'\0')) - {b''}
new = set(open(sys.argv[2], 'rb').read().split(b'\0')) - {b''}
for p in sorted(old - new):
    if p.startswith(b'/') or b'..' in p.split(b'/') or p.startswith(b'.git/'):
        continue
    if os.path.lexists(p) and not os.path.isdir(p):
        os.remove(p)
PY
  fi
  [ -f .git/HEAD ] || git init -q -b snapshot || exit 1
  # Submodules become nested repositories, so the outer index holds a gitlink as at home.
  while IFS= read -r -d '' sub; do
    (cd "$sub" && { [ -e .git ] || git init -q -b snapshot; } && rm -f .git/index && git add -Af . &&
      git -c user.name=onyx-remote -c user.email=onyx-remote@invalid commit -qm snapshot --allow-empty) || exit 1
  done <"$meta/submodules.list"
  rm -f .git/index
  if [ -s "$meta/tracked.list" ]; then
    git -c advice.addEmbeddedRepo=false add -f --pathspec-from-file="$meta/tracked.list" --pathspec-file-nul || exit 1
  fi
  echo "tracked $(git ls-files | wc -l), untracked $(git ls-files -o --exclude-standard | wc -l)"

  step "cargo fetch --locked"
  cargo fetch --locked -q || exit 1
  if [ "${ONYX_WEB:-0}" = 1 ] || [ "${ONYX_WEB_PUBLIC:-0}" = 1 ]; then
    # The web gates need the demo export with the live range (apps/web/README.md), which is
    # gitignored and so never synced: exported here from the host's copy of the blocks.
    range=$(ls "$ONYX_LIVE_DIR" | sed -n 's/^\([0-9]*\)\.raw\.hex$/\1/p' | sort -n | sed -n '1p;$p' | paste -sd:)
    step "export-demo with ONYX_LIVE=$(show "$ONYX_LIVE_DIR"):$range"
    ONYX_LIVE=$ONYX_LIVE_DIR:$range cargo run --locked -q --release -p onyx-source-btc --example export-demo || exit 1
  fi
  step "scripts/check-all.sh (ONYX_DOCKER=0: the Docker smoke runs only locally)"
  scripts/check-all.sh
  rc=$?
  step "check-all exit $rc"
  sweep "$CARGO_TARGET_DIR" "$(show "$CARGO_TARGET_DIR")"
  exit "$rc"
  ;;
run) ;;
*) die "usage: remote-check-runner.sh run|clean|guard ..." ;;
esac

# run <branch> [NAME=VALUE ...]
[ $# -ge 2 ] || die "run <branch> [ONYX_NAME=VALUE ...]"
br=$2
shift 2
valid_branch "$br" || die "not a branch name: '$br'"
for kv in "$@"; do valid_env "$kv" || die "refused variable '${kv%%=*}'"; done
[ -d "$B/src/$br" ] || die "no sources synced to $(show "$B/src/$br")"
for t in rust/bin/cargo node/bin/node foundry/bin/forge bin/pnpm bin/wasm-bindgen bin/cargo-deny; do
  [ -x "$T/$t" ] || die "missing $(show "$T/$t"): run scripts/remote-check.sh --setup"
done
[ -f "$B/fixtures-live/node/970543.raw.hex" ] || die "no fixture copy in $(show "$B/fixtures-live"): run scripts/remote-check.sh --setup"

take_slot "${ONYX_REMOTE_SLOT_WAIT:-7200}" || exit 3
load=$(cut -d' ' -f1 /proc/loadavg)
guard "$load" "$(free_gb)" "$(used_gb)" || exit 3
echo "remote-check: host $(hostname -s), load $load, $(usage_line)"

mkdir -p "$B/logs"
ts=$(date -u +%Y%m%dT%H%M%SZ)
log=$B/logs/$br-$ts.log
unit=onyx-check-$br-$ts
unit=${unit//[^A-Za-z0-9_.-]/-}
stopped=$B/logs/.$unit.stopped
start=$(date +%s)
echo "remote-check: unit $unit.scope, host log $(show "$log")"
# The folders the sandboxed job writes into but cannot create (a fresh TMPDIR each run).
rm -rf -- "${B:?}/tmp/$br"
mkdir -p "$B/tmp/$br" "$B/target/$br" "$B/home" "$T/cargo-home" || die "cannot make the job's folders"
mapfile -t rules < <(policy "$br")
# MemorySwapMax=0: 48 GB is the job's whole memory (the host's swap is the production work's).
systemd-run --user --scope -q --unit="$unit" -p MemoryMax=48G -p MemorySwapMax=0 -p CPUQuota=4800% \
  nice -n 19 ionice -c 3 env -i ONYX_REMOTE_BASE="$B" ONYX_REMOTE_TOOLS="$T" ONYX_REMOTE_HOME="$HOST_HOME" HOME="$B/home" \
  PATH=/usr/bin:/bin LANG=C.UTF-8 USER="${USER:-onyx}" \
  bash "$B/bin/remote-check-runner.sh" sandbox "${rules[@]}" -- \
  bash "$B/bin/remote-check-runner.sh" job "$br" "$@" >"$log" 2>&1 </dev/null 9>&- &
pid=$!
# Stops this run's own scope, nothing else: every process the job started (a test's anvil or
# browser that outlived it included).
stop_scope() { systemctl --user stop "$unit.scope" 2>/dev/null; }
# The caller went away (Ctrl-C, a signal): stop the scope.
trap 'stop_scope; echo "remote-check: stopped (signal)"; exit 130' HUP INT TERM
# Without a tty a dropped ssh session sends no SIGHUP: the runner learns of it only when a write
# fails. Ignored here (the job above keeps the default), so a failed write ends tail, not the runner.
trap '' PIPE
(
  n=0
  while kill -0 "$pid" 2>/dev/null; do
    sleep 20
    n=$((n + 1))
    f=$(free_gb)
    u=-1
    [ $((n % 6)) = 0 ] && u=$(used_gb)
    if [ "$f" -lt "$STOP_FREE_GB" ] || [ "$u" -gt "$BUDGET_GB" ]; then
      echo "free ${f} GB, Onyx ${u} GB (-1: not measured this time)" >"$stopped"
      systemctl --user stop "$unit.scope" 2>/dev/null
      break
    fi
  done
) 9>&- &
watch=$!
tail -n +1 -F --pid="$pid" "$log" 2>/dev/null
if kill -0 "$pid" 2>/dev/null; then
  # tail ended while the job runs: it could not write, the caller is gone.
  echo "remote-check: stopped: the caller went away (output pipe closed)" >>"$log"
  stop_scope
fi
wait "$pid"
rc=$?
kill "$watch" 2>/dev/null
wait "$watch" 2>/dev/null
stop_scope
rm -rf -- "${B:?}/tmp/$br"
if [ -f "$stopped" ]; then
  echo "remote-check: STOPPED by the disk watchdog ($(cat "$stopped")); limits: $STOP_FREE_GB GB free, $BUDGET_GB GB budget"
  rm -f "$stopped"
  rc=4
fi
# Keep the 20 newest host logs.
ls -1t "$B/logs"/*.log 2>/dev/null | tail -n +21 | while IFS= read -r old; do rm -f -- "$old"; done
echo "remote-check: wall $(($(date +%s) - start)) s on $(hostname -s), exit $rc; $(usage_line)"
exit "$rc"
}
