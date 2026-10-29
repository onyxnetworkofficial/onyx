# shellcheck shell=bash
# Machine-wide lock for scripts/docker-smoke.sh (sourced, not run). The smoke publishes fixed host
# ports (127.0.0.1:18545, 18080, 18081; the demo driver's 18403-18405): two runs that start close
# together failed with Docker exit 125 "port is already allocated" (three times on 2026-10-18).
# One lock file (flock) serialises them across worktrees; tested by scripts/smoke-lock-tests.sh.
#
# smoke_lock_default_path [RUN_ROOT [TMP]]: the default lock file, the same in every shell of this
# user whatever its environment: RUN_ROOT/UID/onyx-docker-smoke.lock (RUN_ROOT /run/user, systemd's
# per-user runtime directory, mode 0700) when that directory exists and is this user's, else
# TMP/onyx-docker-smoke-UID.lock (TMP /tmp). Never $XDG_RUNTIME_DIR: cron, env -i, sudo and some
# sandboxes unset or move it, and two runs on two lock files would both bind the ports.
#
# smoke_lock FILE TIMEOUT LABEL: take an exclusive flock on FILE, waiting at most TIMEOUT whole
# seconds. While waiting it prints one line naming the holder (the LABEL the holder wrote into
# FILE, printable characters only). On success the lock fd is SMOKE_LOCK_FD and LABEL is in FILE;
# on failure it prints why on stderr and returns non-zero (1: timed out, 2: bad timeout or lock
# file). FILE is created mode 0600; a symbolic link, or a file in a sticky world-writable
# directory (/tmp) that another user owns, is refused.
# smoke_unlock: clear FILE and release the lock. It releases by LOCK_UN on the open file, so a
# child that inherited the fd and outlived its parent's work cannot keep the lock held.
# smoke_without_lock_fd CMD...: run CMD with the lock fd closed. For a command that may leave a
# process behind (scripts/demo-testnet.sh starts its driver in the background): a process holding
# the fd keeps the lock after this shell dies without smoke_unlock (SIGKILL, an OOM kill).
# smoke_wait_ports DEADLINE PORT...: with the lock held, wait until no PORT is in use (by a
# container or a host process; a run without the lock, a SIGKILLed one) or the epoch second
# DEADLINE passes: 0 when free, 1 (naming the users on stderr) when not.

smoke_lock_timeout_ok() {
  case "$1" in '' | *[!0-9]*) return 1 ;; esac
}

smoke_lock_default_path() {
  local uid run
  uid=$(id -u)
  run=${1:-/run/user}/$uid
  if [ -d "$run" ] && [ ! -L "$run" ] && [ -O "$run" ] && [ -w "$run" ]; then
    printf '%s\n' "$run/onyx-docker-smoke.lock"
  else
    printf '%s\n' "${2:-/tmp}/onyx-docker-smoke-$uid.lock"
  fi
}

# The holder's label, first line, at most 300 bytes, printable characters only (the file may have
# been written by anyone who can write it; no terminal escapes reach the output).
smoke_lock_holder() {
  head -c 300 "$1" 2>/dev/null | head -1 | LC_ALL=C tr -cd '[:print:]'
}

smoke_lock() {
  local file=$1 timeout=$2 label=$3 holder
  smoke_lock_timeout_ok "$timeout" \
    || { echo "lock timeout '$timeout' is not a whole number of seconds" >&2; return 2; }
  command -v flock >/dev/null || { echo "flock not found (util-linux)" >&2; return 2; }
  [ ! -L "$file" ] || { echo "the lock file $file is a symbolic link; refused" >&2; return 2; }
  (umask 077 && : >>"$file") 2>/dev/null || { echo "cannot open the lock file $file" >&2; return 2; }
  if [ ! -f "$file" ] || [ -L "$file" ] || { [ -k "$(dirname "$file")" ] && [ ! -O "$file" ]; }; then
    echo "the lock file $file is not a regular file of this user in a sticky directory; refused" >&2
    return 2
  fi
  exec {SMOKE_LOCK_FD}>>"$file" || { echo "cannot open the lock file $file" >&2; return 2; }
  SMOKE_LOCK_FILE=$file
  if ! flock -n "$SMOKE_LOCK_FD"; then
    holder=$(smoke_lock_holder "$file")
    echo "waiting up to $timeout s for the lock $file, held by: ${holder:-unknown (no label yet)}"
    if ! flock -w "$timeout" "$SMOKE_LOCK_FD"; then
      holder=$(smoke_lock_holder "$file")
      echo "timed out after $timeout s waiting for the lock $file, still held by: ${holder:-unknown}" >&2
      exec {SMOKE_LOCK_FD}>&-
      unset SMOKE_LOCK_FD SMOKE_LOCK_FILE
      return 1
    fi
  fi
  printf '%s\n' "$label" >"$file"
}

smoke_unlock() {
  [ -n "${SMOKE_LOCK_FD:-}" ] || return 0
  : >"$SMOKE_LOCK_FILE" 2>/dev/null || true
  flock -u "$SMOKE_LOCK_FD" 2>/dev/null || true
  exec {SMOKE_LOCK_FD}>&-
  unset SMOKE_LOCK_FD SMOKE_LOCK_FILE
}

smoke_without_lock_fd() {
  if [ -n "${SMOKE_LOCK_FD:-}" ]; then
    "$@" {SMOKE_LOCK_FD}>&-
  else
    "$@"
  fi
}

# "PORT (who) " for each PORT in use: the containers publishing it (docker ps, bounded by
# SMOKE_DOCKER_TIMEOUT, default 10 s, so a hung daemon cannot stall the wait past its deadline),
# else "a host process" when something accepts a TCP connection on 127.0.0.1:PORT.
smoke_busy_ports() {
  local p who
  for p in "$@"; do
    who=$(timeout "${SMOKE_DOCKER_TIMEOUT:-10}" docker ps --filter "publish=$p" --format '{{.Names}}' \
      2>/dev/null | paste -sd, -)
    if [ -n "$who" ]; then
      printf '%s ' "$p ($who)"
    elif timeout 2 bash -c 'exec 3<>"/dev/tcp/127.0.0.1/$0"' "$p" 2>/dev/null; then
      printf '%s ' "$p (a host process)"
    fi
  done
}

smoke_wait_ports() {
  local deadline=$1 busy t0
  shift
  t0=$(date +%s)
  busy=$(smoke_busy_ports "$@")
  [ -n "$busy" ] || return 0
  echo "lock held, but ports in use by a run without the lock: $busy; waiting until $(date -d "@$deadline" +%T)"
  while [ -n "$busy" ] && [ "$(date +%s)" -lt "$deadline" ]; do
    sleep "${SMOKE_PORT_POLL:-2}"
    busy=$(smoke_busy_ports "$@")
  done
  if [ -n "$busy" ]; then
    echo "ports still in use at the deadline: $busy" >&2
    return 1
  fi
  echo "ports free after $(($(date +%s) - t0)) s"
}
