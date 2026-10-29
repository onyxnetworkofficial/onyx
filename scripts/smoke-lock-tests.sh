#!/usr/bin/env bash
# Tests for scripts/smoke-lock.sh, the machine-wide lock of scripts/docker-smoke.sh (no Docker;
# about 14 s; run by check-all). Uses its own lock file in a temp directory, never the real one,
# so it cannot block or be blocked by a smoke run. Checks:
#   1. a free lock is taken at once and the holder's label is written into the file;
#   2. while it is held, a second taker with a 1 s timeout fails (exit 1) within about 1 s,
#      printing one waiting line that names the holder, then a timeout line;
#   3. four takers racing for the lock, 5 rounds each, are never inside it at the same time,
#      and every one of them gets it;
#   4. smoke_unlock releases the lock even while a child that inherited the fd still runs;
#   5. a timeout that is not a whole number of seconds is refused (exit 2) without waiting;
#   6. the default lock path does not follow $XDG_RUNTIME_DIR (two shells, one lock), falls back
#      to /tmp/onyx-docker-smoke-UID.lock, and a new lock file is mode 0600;
#   7. a symbolic-link lock file is refused (exit 2, its target untouched) and a holder label with
#      terminal escapes is printed without them;
#   8. a holder SIGKILLed while a process it started through smoke_without_lock_fd still runs
#      frees the lock at once (the control: a process started without it keeps the lock);
#   9. docker-smoke.sh's own cleanup, entered by an INT and hit by an INT, a TERM and a HUP, or by a
#      set -e abort, removes everything while the lock is held, then releases it;
#  10. the port wait names a host listener and a container, fails at its deadline, returns once
#      the port is freed, and a hung `docker ps` cannot stall it.
set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=scripts/smoke-lock.sh
. scripts/smoke-lock.sh
dir=$(mktemp -d)
lock=$dir/lock
pids=()
strays=()
cleanup() {
  for p in "${pids[@]}" "${strays[@]}"; do kill "$p" 2>/dev/null || true; done
  rm -rf "$dir"
}
trap cleanup EXIT
fail() { echo "FAIL smoke-lock: $*" >&2; exit 1; }
ok() { echo "ok   smoke-lock: $*"; }

# 1. Free lock.
smoke_lock "$lock" 5 "holder-one" || fail "a free lock was not taken"
[ "$(cat "$lock")" = holder-one ] || fail "the holder label is not in the lock file"
smoke_unlock
[ ! -s "$lock" ] || fail "smoke_unlock left the holder label in the lock file"
ok "a free lock is taken at once, labelled, and cleared on release"

# 2. Held lock: a second taker times out cleanly. The holder is a separate process (flock locks
# are per open file, so this shell must not hold it itself).
(smoke_lock "$lock" 5 "holder-two pid $BASHPID" && touch "$dir/held" && exec sleep 30) &
pids+=($!)
for _ in $(seq 50); do [ -e "$dir/held" ] && break; sleep 0.1; done
[ -e "$dir/held" ] || fail "the background holder did not take the lock"
t0=$(date +%s%N)
set +e
out=$(smoke_lock "$lock" 1 "holder-three" 2>"$dir/err")
rc=$?
set -e
ms=$((($(date +%s%N) - t0) / 1000000))
[ "$rc" = 1 ] || fail "a taker of a held lock returned $rc, want 1 (timed out)"
[ "$ms" -ge 900 ] && [ "$ms" -lt 4000 ] || fail "the 1 s timeout took $ms ms"
[ "$(wc -l <<<"$out")" = 1 ] && grep -q '^waiting up to 1 s for the lock .*, held by: holder-two pid ' <<<"$out" \
  || fail "the waiting line does not name the holder: $out"
grep -q '^timed out after 1 s waiting for the lock .*, still held by: holder-two' "$dir/err" \
  || fail "no clear timeout message: $(cat "$dir/err")"
[ "$(cat "$lock")" = "holder-two pid ${pids[0]}" ] || fail "the timed-out taker changed the holder label"
kill "${pids[0]}"
wait "${pids[0]}" 2>/dev/null || true
pids=()
ok "a held lock times out after 1 s (exit 1, $ms ms), one waiting line names the holder"

# 3. Mutual exclusion: each taker records entering and leaving; no two may overlap.
taker() {
  local n=$1 r
  for r in 1 2 3 4 5; do
    smoke_lock "$lock" 30 "taker $n round $r" >/dev/null || exit 1
    if ! mkdir "$dir/inside" 2>/dev/null; then
      echo overlap >>"$dir/overlaps"
    fi
    echo "$n" >>"$dir/entries"
    sleep 0.05
    rmdir "$dir/inside"
    smoke_unlock
  done
}
for n in 1 2 3 4; do
  taker "$n" &
  pids+=($!)
done
for p in "${pids[@]}"; do wait "$p" || fail "a racing taker did not get the lock"; done
pids=()
[ ! -e "$dir/overlaps" ] || fail "two holders were inside the lock at once ($(wc -l <"$dir/overlaps") times)"
[ "$(wc -l <"$dir/entries")" = 20 ] || fail "$(wc -l <"$dir/entries") of 20 rounds held the lock"
ok "4 racing takers x 5 rounds: 20 holds, never two at once"

# 4. A child that inherited the fd (a server left running) does not keep the lock held.
smoke_lock "$lock" 5 "parent" || fail "the lock was not free"
sleep 30 &
child=$!
pids+=("$child")
smoke_unlock
smoke_lock "$lock" 1 "next" >/dev/null || fail "the lock stayed held by a child after smoke_unlock"
smoke_unlock
kill "$child"
wait "$child" 2>/dev/null || true
pids=()
ok "smoke_unlock releases the lock while a child holding the fd still runs"

# 5. A bad timeout is refused at once.
for bad in "" 1.5 -1 abc; do
  set +e
  smoke_lock "$lock" "$bad" "bad" 2>"$dir/err"
  rc=$?
  set -e
  [ "$rc" = 2 ] && grep -q 'not a whole number of seconds' "$dir/err" || fail "timeout '$bad' gave $rc"
done
ok "a timeout that is not a whole number of seconds is refused (exit 2)"

# 6. One lock whatever the environment; private file mode.
# A real directory of this user as XDG_RUNTIME_DIR, so a path that follows it would differ.
mkdir -m 0700 "$dir/xdg"
a=$(XDG_RUNTIME_DIR=$dir/xdg smoke_lock_default_path)
b=$(env -u XDG_RUNTIME_DIR bash -c '. scripts/smoke-lock.sh && smoke_lock_default_path')
[ "$a" = "$b" ] || fail "the default lock path follows XDG_RUNTIME_DIR: '$a' vs '$b'"
run=/run/user/$(id -u)
if [ -d "$run" ] && [ -O "$run" ] && [ -w "$run" ]; then want=$run/onyx-docker-smoke.lock; else want=/tmp/onyx-docker-smoke-$(id -u).lock; fi
[ "$a" = "$want" ] || fail "default lock path $a, want $want"
c=$(smoke_lock_default_path "$dir/no-run-root" "$dir")
[ "$c" = "$dir/onyx-docker-smoke-$(id -u).lock" ] || fail "no /run/user fallback gave $c"
smoke_lock "$dir/fresh.lock" 1 "fresh" || fail "a new lock file was not taken"
[ "$(stat -c %a "$dir/fresh.lock")" = 600 ] || fail "a new lock file is mode $(stat -c %a "$dir/fresh.lock"), want 600"
smoke_unlock
ok "one default lock path with or without XDG_RUNTIME_DIR ($a), /tmp fallback, new file mode 0600"

# 7. A symbolic link is refused; a label with terminal escapes is printed without them.
ln -s "$dir/link-target" "$dir/link.lock"
set +e
smoke_lock "$dir/link.lock" 1 "via link" 2>"$dir/err"
rc=$?
set -e
[ "$rc" = 2 ] && grep -q 'symbolic link' "$dir/err" || fail "a symbolic-link lock file gave $rc"
[ ! -e "$dir/link-target" ] || fail "the symbolic link's target was created"
(smoke_lock "$lock" 5 $'evil\e]0;title\a\e[2J label' && touch "$dir/held7" && exec sleep 30) &
pids+=($!)
for _ in $(seq 50); do [ -e "$dir/held7" ] && break; sleep 0.1; done
[ -e "$dir/held7" ] || fail "the background holder did not take the lock"
set +e
out=$(smoke_lock "$lock" 1 "waiter" 2>&1)
set -e
grep -q 'held by: evil\]0;title\[2J label' <<<"$out" || fail "the escaped label was not printed as text: $out"
! grep -q $'[\e\a]' <<<"$out" || fail "a terminal escape reached the output"
kill "${pids[0]}"
wait "${pids[0]}" 2>/dev/null || true
pids=()
ok "a symbolic-link lock file is refused (exit 2); a label's terminal escapes are stripped"

# 8. A holder dies by SIGKILL while a process it started still runs.
# $1: run the starter through smoke_without_lock_fd (1) or not (0). The starter is a command that
# leaves a process behind and returns, as scripts/demo-testnet.sh leaves its driver.
killed_holder() {
  rm -f "$dir/held8" "$dir/stray"
  (
    smoke_lock "$lock" 5 "killed holder" || exit 1
    starter=(bash -c 'sleep 30 >/dev/null 2>&1 & echo $! >"$0"' "$dir/stray")
    if [ "$1" = 1 ]; then smoke_without_lock_fd "${starter[@]}"; else "${starter[@]}"; fi
    touch "$dir/held8"
    exec sleep 30
  ) &
  local holder=$!
  for _ in $(seq 50); do [ -e "$dir/held8" ] && break; sleep 0.1; done
  [ -e "$dir/held8" ] && [ -s "$dir/stray" ] || fail "the holder did not start its process"
  strays+=("$(cat "$dir/stray")")
  kill -KILL "$holder"
  wait "$holder" 2>/dev/null || true
}
killed_holder 0
set +e
smoke_lock "$lock" 1 "after a leaked fd" >/dev/null 2>&1
rc=$?
set -e
[ "$rc" = 1 ] || fail "control: a process holding the fd should keep the lock (got $rc); the test cannot see a leak"
kill "${strays[-1]}"
for _ in $(seq 50); do kill -0 "${strays[-1]}" 2>/dev/null || break; sleep 0.1; done
killed_holder 1
smoke_lock "$lock" 1 "after a closed fd" >/dev/null || fail "a process started through smoke_without_lock_fd kept the lock"
smoke_unlock
kill "${strays[-1]}" 2>/dev/null || true
ok "a SIGKILLed holder's process started through smoke_without_lock_fd does not keep the lock (without it, it does)"

# 9. docker-smoke.sh's own cleanup and traps, with a fake docker that records whether the lock was
# held at each call and takes 0.5 s, so the signals land inside cleanup.
mkdir "$dir/bin"
cat >"$dir/bin/docker" <<'FAKE'
#!/usr/bin/env bash
if flock -n "$FAKE_LOCK" true; then state=free; else state=held; fi
echo "$1 $state" >>"$FAKE_LOG"
sleep 0.5
FAKE
chmod +x "$dir/bin/docker"
cleanup_src=$(sed -n '/^cleanup() {$/,/^}$/p' scripts/docker-smoke.sh)
traps=$(grep -E '^trap ' scripts/docker-smoke.sh)
[ -n "$cleanup_src" ] && [ "$(wc -l <<<"$traps")" = 2 ] || fail "no cleanup() or traps found in docker-smoke.sh"
# $1: how the run ends (signals: three signals inside cleanup; abort: a set -e failure).
run_cleanup() {
  rm -f "$dir/log9" "$dir/ready9"
  mkdir -p "$dir/work9"
  # Through python: a background job of a non-interactive shell starts with SIGINT ignored, and
  # bash cannot trap a signal ignored on entry; docker-smoke.sh runs in the foreground.
  FAKE_LOCK=$lock FAKE_LOG=$dir/log9 PATH=$dir/bin:$PATH python3 -I -c 'import os, signal, sys
signal.signal(signal.SIGINT, signal.SIG_DFL); os.execvp("bash", ["bash"] + sys.argv[1:])' -c '
    set -euo pipefail
    . scripts/smoke-lock.sh
    eval "$1"; eval "$2"
    id=onyx-smoke-test work=$4
    smoke_lock "$5" 5 "cleanup test" >/dev/null
    touch "$3"
    if [ "$6" = abort ]; then false; fi
    sleep 30 & echo $! >"$3.sleep"; wait
  ' _ "$cleanup_src" "$traps" "$dir/ready9" "$dir/work9" "$lock" "$1" &
  local runner=$!
  for _ in $(seq 50); do [ -e "$dir/ready9" ] && break; sleep 0.1; done
  if [ "$1" = signals ]; then
    for _ in $(seq 50); do [ -s "$dir/ready9.sleep" ] && break; sleep 0.1; done
    strays+=("$(cat "$dir/ready9.sleep")")
    # Only the first must land: without the guard in cleanup the run is gone after the second.
    kill -INT "$runner"; sleep 0.25
    kill -INT "$runner" 2>/dev/null || true; sleep 0.5
    kill -TERM "$runner" 2>/dev/null || true; sleep 0.5
    kill -HUP "$runner" 2>/dev/null || true
  fi
  set +e
  wait "$runner"
  rc=$?
  set -e
  [ "$(cat "$dir/log9" 2>/dev/null)" = $'rm held\nvolume held\nnetwork held' ] \
    || fail "cleanup ($1): docker calls were not all made with the lock held: $(tr '\n' ' ' <"$dir/log9" 2>/dev/null)"
  [ ! -e "$dir/work9" ] || fail "cleanup ($1) left the work directory"
  smoke_lock "$lock" 0 "after cleanup" >/dev/null || fail "cleanup ($1) did not release the lock"
  smoke_unlock
}
run_cleanup signals
[ "$rc" = 130 ] || fail "an interrupted run exited $rc, want 130"
run_cleanup abort
[ "$rc" = 1 ] || fail "a set -e abort exited $rc, want 1"
ok "docker-smoke.sh cleanup: entered by INT (then INT, TERM, HUP inside it) or a set -e abort, removes all, then releases"

# 10. The port wait.
python3 -I -c 'import socket, sys, time
s = socket.socket(); s.bind(("127.0.0.1", 0)); s.listen()
open(sys.argv[1], "w").write(str(s.getsockname()[1])); time.sleep(30)' "$dir/port" &
listener=$!
strays+=("$listener")
for _ in $(seq 50); do [ -s "$dir/port" ] && break; sleep 0.1; done
port=$(cat "$dir/port")
cat >"$dir/bin/docker" <<'FAKE'
#!/usr/bin/env bash
case "$FAKE_DOCKER" in
  hang) exec sleep 30 ;;
  name) [ "$3" = "publish=$FAKE_PORT" ] && echo onyx-smoke-1-anvil ;;
esac
exit 0
FAKE
export SMOKE_PORT_POLL=0.2 SMOKE_DOCKER_TIMEOUT=1
set +e
out=$(FAKE_DOCKER=none PATH=$dir/bin:$PATH smoke_wait_ports "$(($(date +%s) + 1))" "$port" 2>&1)
rc=$?
set -e
[ "$rc" = 1 ] && grep -q "ports still in use at the deadline: $port (a host process)" <<<"$out" \
  || fail "a host listener: rc $rc, $out"
set +e
out=$(FAKE_DOCKER=name FAKE_PORT=$port PATH=$dir/bin:$PATH smoke_wait_ports "$(date +%s)" "$port" 2>&1)
rc=$?
set -e
[ "$rc" = 1 ] && grep -q "$port (onyx-smoke-1-anvil)" <<<"$out" || fail "a container on the port: rc $rc, $out"
t0=$(date +%s)
set +e
out=$(FAKE_DOCKER=hang PATH=$dir/bin:$PATH smoke_wait_ports "$((t0 + 1))" "$port" 2>&1)
rc=$?
set -e
[ "$rc" = 1 ] && [ $(($(date +%s) - t0)) -le 6 ] || fail "a hung docker ps: rc $rc after $(($(date +%s) - t0)) s"
(sleep 1; kill "$listener") &
pids+=($!)
out=$(FAKE_DOCKER=none PATH=$dir/bin:$PATH smoke_wait_ports "$(($(date +%s) + 10))" "$port" 2>&1) \
  || fail "the port wait did not return once the port was freed: $out"
grep -q '^ports free after ' <<<"$out" || fail "no 'ports free' line: $out"
wait "${pids[0]}" 2>/dev/null || true
pids=()
unset SMOKE_PORT_POLL SMOKE_DOCKER_TIMEOUT
ok "the port wait names a host process and a container, fails at its deadline, survives a hung docker, returns once free"
echo "smoke-lock tests passed"
