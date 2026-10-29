#!/usr/bin/env bash
# Run scripts/check-all.sh for a worktree on the remote build host (ROADMAP E13,
# docs/ops/remote-check.md; THREATS REMOTE-1, REMOTE-2, REMOTE-3, REMOTE-4, REMOTE-6, REMOTE-7,
# REMOTE-8, REMOTE-9, REMOTE-10), so the heavy gate leaves this machine.
#   scripts/remote-check.sh [--log FILE] <worktree> [ONYX_NAME=VALUE ...]
#       secret checks here, sync the worktree's files, check-all on the host under its limits;
#       the log streams into FILE (default: ${TMPDIR:-/tmp}/remote-check-<branch>-<time>.log) and the
#       exit status is check-all's (2 refused here, 3 refused by the host, 4 stopped by its disk
#       watchdog, 255 ssh or the copy to the host failed)
#   scripts/remote-check.sh --dry-run <worktree>   the local refusals and the file list, no host
#   scripts/remote-check.sh --clean <branch>       delete ~/onyx-build/src/<branch> and
#                                                  ~/onyx-build/target/<branch> on the host, only
#   scripts/remote-check.sh --setup                copy this machine's tools and fixture blocks
#                                                  to ~/onyx-tools and ~/onyx-build (idempotent)
#   scripts/remote-check.sh --self-test            parsing, refusals, file list (no host)
# Host: ONYX_REMOTE_HOST (default `dell-onyx`: the separate `onyx-build` user on the build machine, own key). What is synced: the
# worktree's tracked files (`git ls-files`, deleted ones left out, submodule files included) plus
# untracked files that are not ignored; never .git, an env or key file, target/, node_modules/,
# apps/web/build, credential files, or anything outside the worktree (symlinks are copied as links,
# never followed; a path under a symlinked folder, a symlinked submodule included, is refused).
# The Docker smoke never runs there (no Docker): ONYX_DOCKER is always 0 and must run locally.
# The whole script is one { ... } block: bash parses it completely before running, so a checkout,
# merge or edit of this file during a 90-minute run cannot change what the rest of the run executes
# (bash reads a plain script from the file as it goes; an in-place edit once turned check-all's 0
# into exit 1, THREATS REMOTE-9).
{
set -euo pipefail
# Read-only on the worktree: git never refreshes its index here (it may be another agent's).
export GIT_OPTIONAL_LOCKS=0
HERE=$(cd "$(dirname "$0")" && pwd)
HOST=${ONYX_REMOTE_HOST:-dell-onyx}
SSH=(ssh -o BatchMode=yes -o ServerAliveInterval=30 -o ServerAliveCountMax=10)
RUNNER=$HERE/remote-check-runner.sh

die() { echo "remote-check: $*" >&2; exit 2; }
# rsync over the same ssh options (never a password prompt); a failed copy exits 255 like ssh.
copy() {
  rsync -e "${SSH[*]}" "$@" || { echo "remote-check: copy to $HOST failed (rsync exit $?)" >&2; exit 255; }
}
reachable() {
  "${SSH[@]}" "$HOST" true </dev/null || { echo "remote-check: ssh to $HOST failed (exit 255)" >&2; exit 255; }
}
# The synced tree: exactly the list (no directory walk), symlinks as links. Second layer behind the
# refusals in prepare: these names are excluded even if listed. $1 lists folder, $2 destination.
sync_tree() {
  rsync -e "${SSH[*]}" -a --mkpath --from0 --files-from="$1/files.list" \
    --include='.env.example' --exclude='.env*' --exclude='*.env' --exclude='.envrc' --exclude='*.key' \
    --exclude='*.pem' --exclude='.git' --exclude='/target/' --exclude='node_modules/' \
    --exclude='/apps/web/build/' --exclude='/.onyx-testnet/' \
    "$WT/" "$2" || { echo "remote-check: copy to $2 failed (rsync exit $?)" >&2; exit 255; }
}
usage() {
  sed -n '5,15p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

# Same rules as the runner's (it checks them again on the host).
valid_branch() { [[ $1 =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$ ]] && [[ $1 != *..* ]]; }
check_env() { # NAME=VALUE -> refuses with a reason
  local kv=$1
  [[ $kv =~ ^ONYX_[A-Z0-9_]+=[A-Za-z0-9_.:,/+-]*$ ]] ||
    die "refused '$kv': only ONYX_NAME=VALUE with letters, digits and _.:,/+- in the value"
  case ${kv%%=*} in
    ONYX_DOCKER) die "refused ONYX_DOCKER: the Docker smoke never runs on the build host (no Docker); run ONYX_DOCKER=1 scripts/check-all.sh locally" ;;
    ONYX_REMOTE_* | ONYX_RESERVE_DIR | ONYX_LIVE_DIR) die "refused ${kv%%=*}: set by the host runner" ;;
  esac
}

# The files to sync, written NUL-separated into $2: files.list (everything), tracked.list (the
# outer index, gitlinks included), submodules.list. Refuses deleted-only states it cannot mirror.
build_lists() { # worktree outdir
  python3 -I - "$1" "$2" <<'PY'
import os, subprocess, sys
wt, out = sys.argv[1], sys.argv[2]
FORBIDDEN = ("target/", "node_modules/", "apps/web/build/", ".onyx-testnet/", ".git/")
def git(cwd, *a):
    return subprocess.run(["git", "-C", cwd, *a], check=True, capture_output=True).stdout
def nul(b):
    return [x.decode() for x in b.split(b"\0") if x]
def bad(p):
    parts = p.split("/")
    return (p.startswith("/") or ".." in parts or ".git" in parts
            or any(p.startswith(f) or ("/" + f) in ("/" + p) for f in FORBIDDEN if f != ".git/"))
def walk(root, prefix, files, subs):
    deleted = set(nul(git(root, "ls-files", "-z", "--deleted")))
    tracked = []
    for line in nul(git(root, "ls-files", "-z", "--stage")):
        meta, path = line.split("\t", 1)
        if meta.split()[0] == "160000":
            sub = os.path.join(root, path)
            top = subprocess.run(["git", "-C", sub, "rev-parse", "--show-toplevel"], capture_output=True)
            if top.returncode != 0 or os.path.realpath(top.stdout.decode().strip()) != os.path.realpath(sub):
                sys.exit(f"remote-check: submodule {prefix}{path} is not checked out (git submodule update --init {prefix}{path})")
            subs.append(prefix + path)
            tracked.append(path)
            walk(sub, prefix + path + "/", files, subs)
        elif path not in deleted:
            tracked.append(path)
            files.append(prefix + path)
    for path in nul(git(root, "ls-files", "-z", "--others", "--exclude-standard")):
        if not path.endswith("/"):
            files.append(prefix + path)
    return tracked
files, subs = [], []
tracked = walk(wt, "", files, subs)
bads = sorted({p for p in files if bad(p)})
if bads:
    sys.exit("remote-check: REFUSED: paths that must never be synced:\n" + "\n".join("  " + p for p in bads))
# rsync follows a symlinked folder on the way to a listed file (a submodule folder replaced by a
# link to another checkout lists that checkout's files): every folder must resolve to itself.
real, seen = os.path.realpath(wt), {}
def escapes(p):
    d = os.path.dirname(p)
    if d and d not in seen:
        seen[d] = os.path.realpath(os.path.join(wt, d)) != os.path.join(real, d)
    return bool(d) and seen[d]
outs = sorted({p for p in files if escapes(p)})
if outs:
    sys.exit("remote-check: REFUSED: paths under a symbolic link to a folder (rsync would follow it):\n" +
             "\n".join("  " + p for p in outs[:20]))
for name, items in (("files.list", sorted(set(files))), ("tracked.list", tracked), ("submodules.list", subs)):
    with open(os.path.join(out, name), "wb") as h:
        h.write(b"".join(p.encode() + b"\0" for p in items))
PY
}

# Every refusal that needs no host: the worktree's own secret scan, the name and content scan of
# the list, and check-all's first gate (recipes/out against the index), which the host cannot
# see because it rebuilds the index from the synced files. Sets WT, BRANCH, LISTS.
prepare() { # worktree
  [ -d "$1" ] || die "no such worktree: $1"
  WT=$(cd "$1" && pwd -P)
  local top
  top=$(git -C "$WT" rev-parse --show-toplevel 2>/dev/null) || die "$WT is not a git worktree"
  [ "$(cd "$top" && pwd -P)" = "$WT" ] || die "$WT is not the top of its worktree ($top is)"
  local b
  b=$(git -C "$WT" symbolic-ref -q --short HEAD) || die "$WT has a detached HEAD: check out a branch"
  BRANCH=${b//\//-}
  valid_branch "$BRANCH" || die "branch '$b' does not map to a safe folder name"
  [ -x "$WT/scripts/check-all.sh" ] || die "$WT has no scripts/check-all.sh"
  echo "remote-check: secret scan of $WT (its own scripts/check-secrets.sh)"
  "$WT/scripts/check-secrets.sh" || die "REFUSED: the worktree's secret scan failed"
  LISTS=$(mktemp -d)
  build_lists "$WT" "$LISTS" || exit 2
  (cd "$WT" && "$HERE/check-secrets.sh" --paths-from "$LISTS/files.list") ||
    die "REFUSED: a file about to be synced looks like a secret (names above, values not shown)"
  if [ -n "$(git -C "$WT" ls-files --others --exclude-standard -- recipes/out)" ] || ! git -C "$WT" diff --quiet -- recipes/out; then
    git -C "$WT" status --short -- recipes/out
    die "REFUSED: recipes/out differs from the index or has untracked files (check-all's first gate would fail here)"
  fi
}

case ${1:-} in
--self-test)
  t=$(mktemp -d)
  trap 'rm -rf "$t"' EXIT
  ok=0
  expect_fail() { # what, command...
    local what=$1
    shift
    if "$@" >"$t/out" 2>&1; then echo "remote-check self-test: $what passed (must refuse)"; cat "$t/out"; exit 1; fi
    ok=$((ok + 1))
  }
  expect_ok() {
    local what=$1
    shift
    "$@" >"$t/out" 2>&1 || { echo "remote-check self-test: $what failed"; cat "$t/out"; exit 1; }
    ok=$((ok + 1))
  }
  me=$HERE/remote-check.sh
  # 1. Arguments (no host is contacted: every case stops before ssh).
  export ONYX_REMOTE_HOST=onyx-self-test.invalid
  expect_fail "no arguments" "$me"
  expect_fail "an unknown option" "$me" --bogus
  expect_fail "a missing worktree" "$me" "$t/none"
  expect_fail "--clean with no branch" "$me" --clean
  for b in .. ../x a/b -x 'a b' "" '.hidden'; do expect_fail "--clean '$b'" "$me" --clean "$b"; done
  expect_fail "--log with no file" "$me" --log
  # 2. A worktree: tracked, untracked, ignored, deleted and env files, and a submodule.
  r=$t/repo
  mkdir -p "$r/scripts" "$r/recipes/out" "$r/src" "$r/target/debug" "$r/apps/web/build" "$r/apps/web/node_modules/x"
  git -C "$r" init -q -b feature/x
  cp "$HERE/check-secrets.sh" "$r/scripts/"
  printf '#!/bin/sh\necho stub\n' >"$r/scripts/check-all.sh"
  chmod +x "$r/scripts/check-all.sh"
  printf '/target\n.env\n.env.*\n!.env.example\napps/web/build/\napps/web/node_modules/\n*.log\n' >"$r/.gitignore"
  echo 'fn main() {}' >"$r/src/main.rs"
  echo 'X=' >"$r/.env.example"
  echo gone >"$r/src/gone.rs"
  echo w >"$r/recipes/out/a.wasm"
  git -C "$r" add -A
  git -C "$r" -c user.name=t -c user.email=t@t commit -qm one
  rm "$r/src/gone.rs"
  echo new >"$r/src/new.rs"
  echo 'X=1' >"$r/.env"
  echo 'X=1' >"$r/.env.local"
  echo bin >"$r/target/debug/app"
  echo html >"$r/apps/web/build/index.html"
  echo js >"$r/apps/web/node_modules/x/i.js"
  echo log >"$r/run.log"
  ln -s /etc/hostname "$r/src/outside-link"
  s=$t/sub
  git init -q -b main "$s"
  echo lib >"$s/lib.sol"
  git -C "$s" add -A
  git -C "$s" -c user.name=t -c user.email=t@t commit -qm sub
  git -C "$r" -c protocol.file.allow=always submodule -q add "$s" lib/sub 2>/dev/null
  expect_ok "--dry-run on a clean worktree" "$me" --dry-run "$r"
  want=$(printf '%s\n' .env.example .gitignore .gitmodules lib/sub/lib.sol recipes/out/a.wasm scripts/check-all.sh \
    scripts/check-secrets.sh src/main.rs src/new.rs src/outside-link)
  got=$(sed -n '/^--- files ---$/,/^--- end ---$/p' "$t/out" | sed '1d;$d')
  [ "$got" = "$want" ] || { echo "remote-check self-test: file list"; diff <(echo "$want") <(echo "$got"); exit 1; }
  grep -q '^branch folder: feature-x$' "$t/out" || { echo "remote-check self-test: branch folder"; cat "$t/out"; exit 1; }
  ok=$((ok + 1))
  # 3. Variables: Docker, the runner's own and malformed ones are refused before any sync.
  for kv in ONYX_DOCKER=1 ONYX_DOCKER=0 ONYX_REMOTE_BASE=/ ONYX_RESERVE_DIR=/x 'ONYX_X=$(id)' 'ONYX_X=a b' FOO=1 onyx_web=1 ONYX_WEB; do
    expect_fail "variable '$kv'" "$me" --log "$t/l.log" "$r" "$kv"
  done
  # 4. Secrets: an untracked key, an env file that is not ignored, a tracked key, a key file.
  fake=$(printf 'onyx-remote-check-self-test' | sha256sum | cut -d' ' -f1)
  echo "DEPLOYER_KEY=0x$fake" >"$r/src/notes.txt"
  expect_fail "an untracked file holding a key" "$me" --dry-run "$r"
  grep -q "$fake" "$t/out" && { echo "remote-check self-test: a refusal printed the value"; exit 1; }
  rm "$r/src/notes.txt"
  echo 'X=1' >"$r/deploy.env"
  expect_fail "an untracked deploy.env" "$me" --dry-run "$r"
  rm "$r/deploy.env"
  echo k >"$r/src/actor.key"
  expect_fail "an untracked .key file" "$me" --dry-run "$r"
  git -C "$r" add src/actor.key
  expect_fail "a tracked .key file" "$me" --dry-run "$r"
  git -C "$r" rm -q --cached src/actor.key
  rm "$r/src/actor.key"
  git -C "$r" add -f .env
  expect_fail "a tracked .env" "$me" --dry-run "$r"
  git -C "$r" rm -q --cached .env
  # 5. Paths that must never leave: a tracked file under target/ or apps/web/build.
  git -C "$r" add -f target/debug/app
  expect_fail "a tracked target/ file" "$me" --dry-run "$r"
  git -C "$r" rm -q --cached target/debug/app
  git -C "$r" add -f apps/web/node_modules/x/i.js
  expect_fail "a tracked node_modules/ file" "$me" --dry-run "$r"
  git -C "$r" rm -q --cached apps/web/node_modules/x/i.js
  # 6. check-all's recipes/out gate, mirrored here; a submodule that is not checked out.
  echo changed >"$r/recipes/out/a.wasm"
  expect_fail "an unstaged recipes/out change" "$me" --dry-run "$r"
  git -C "$r" checkout -q -- recipes/out/a.wasm
  echo extra >"$r/recipes/out/b.wasm"
  expect_fail "an untracked recipes/out file" "$me" --dry-run "$r"
  rm "$r/recipes/out/b.wasm"
  git -C "$r" update-index --add --cacheinfo "160000,$(git -C "$s" rev-parse HEAD),lib/empty"
  expect_fail "a submodule that is not checked out" "$me" --dry-run "$r"
  git -C "$r" rm -q --cached lib/empty
  git -C "$r" checkout -q --detach
  expect_fail "a detached HEAD" "$me" --dry-run "$r"
  git -C "$r" checkout -q feature/x
  expect_ok "--dry-run after the cleanup" "$me" --dry-run "$r"
  # 6b. Credential files and token forms (THREATS REMOTE-6), each refused alone, no value printed.
  tok=$(printf 'onyx-remote-check-token' | sha256sum | cut -c1-36)
  pem="-----BEGIN OPENSSH PRIVATE $(echo KEY)-----" # split so this file never matches the scan
  refuse_file() { # what, path, content
    mkdir -p "$(dirname "$r/$2")"
    printf '%s\n' "$3" >"$r/$2"
    expect_fail "$1" "$me" --dry-run "$r"
    if grep -q "$tok" "$t/out"; then echo "remote-check self-test: $1 printed the value"; exit 1; fi
    rm -f "$r/$2"
  }
  refuse_file "an upper-case .ENV" .ENV 'X=1'
  refuse_file "a .Env.Local the ignore rules miss" config/.Env.Local 'X=1'
  refuse_file "a file under secrets/" secrets/notes.txt 'nothing secret by content'
  refuse_file "an ssh key by name" deploy/id_ed25519 'not a key'
  refuse_file "an ssh key by content" deploy/notes.txt "$(printf '%s\n%s' "$pem" "$tok")"
  refuse_file "a wallet key store" keys/wallet-backup.json '{"version":3}'
  refuse_file "a geth key store" keys/UTC--2026-10-19T00-00-00Z--0000 '{}'
  refuse_file "an .npmrc" .npmrc "//registry.npmjs.org/:_authToken=npm_$tok"
  refuse_file "a .p12 key store" certs/signing.p12 'binary'
  refuse_file "an untracked GitHub token" src/cfg.txt "GH=ghp_$tok"
  refuse_file "an untracked env-style password" scripts/run.sh "export DB_PASSWORD=$tok"
  refuse_file "a URL holding a password" src/db.txt "postgres://onyx:$tok@db.invalid/x"
  refuse_file "a quoted token" src/conf.json "{\"apiToken\": \"$tok\"}"
  # 6c. Symlinks: a link to a folder outside goes as a link (its files never), a submodule folder
  # replaced by a link is refused, an ignored file holding a key never leaves; the real rsync into a
  # local folder copies exactly the list.
  mkdir -p "$t/fakehome/.ssh"
  echo "$pem" >"$t/fakehome/.ssh/id_ed25519"
  ln -s "$t/fakehome/.ssh" "$r/src/sshdir"
  echo "DEPLOYER_KEY=0x$fake" >"$r/secret.log"
  expect_ok "--dry-run with a link to an outside folder and an ignored key" "$me" --dry-run "$r"
  grep -qx 'src/sshdir' "$t/out" && ! grep -q 'secret.log\|id_ed25519' "$t/out" ||
    { echo "remote-check self-test: link or ignored file listed wrongly"; cat "$t/out"; exit 1; }
  ok=$((ok + 1))
  (
    WT=$r
    sed -n '/^--- files ---$/,/^--- end ---$/p' "$t/out" | sed '1d;$d' | tr '\n' '\0' >"$t/files.list"
    sync_tree "$t" "$t/dest/"
  ) >"$t/sync.out" 2>&1 || { echo "remote-check self-test: local sync failed"; cat "$t/sync.out"; exit 1; }
  got=$(cd "$t/dest" && find . \( -type f -o -type l \) | sed 's|^\./||' | sort)
  want=$(sed -n '/^--- files ---$/,/^--- end ---$/p' "$t/out" | sed '1d;$d' | sort)
  [ "$got" = "$want" ] && [ -L "$t/dest/src/sshdir" ] && [ -z "$(find "$t/dest" -name id_ed25519 -o -name secret.log)" ] ||
    { echo "remote-check self-test: the synced tree differs from the list"; diff <(echo "$want") <(echo "$got"); exit 1; }
  ok=$((ok + 1))
  rm "$r/src/sshdir" "$r/secret.log"
  mv "$r/lib/sub" "$t/sub-moved"
  ln -s "$s" "$r/lib/sub"
  expect_fail "a submodule folder replaced by a link to an outside checkout" "$me" --dry-run "$r"
  grep -q 'symbolic link to a folder' "$t/out" || { echo "remote-check self-test: wrong refusal"; cat "$t/out"; exit 1; }
  rm "$r/lib/sub"
  mv "$t/sub-moved" "$r/lib/sub"
  expect_ok "--dry-run after the link cleanup" "$me" --dry-run "$r"
  # 6d. An unreachable host: ssh's 255 before anything is copied.
  if "$me" --log "$t/l.log" "$r" >"$t/out" 2>&1; then rc=0; else rc=$?; fi
  [ "$rc" = 255 ] || { echo "remote-check self-test: an unreachable host gave exit $rc, want 255"; cat "$t/out"; exit 1; }
  ok=$((ok + 1))
  # 6e. check-all's status reaches the caller unchanged (no pipe, no command after it changes it),
  # also when this file is rewritten in place during the run (REMOTE-9): a copy of this script with
  # a fake ssh (the host's status, and the in-place rewrite) and a fake rsync.
  cpd=$t/copy
  mkdir -p "$cpd/bin"
  cp "$RUNNER" "$HERE/check-secrets.sh" "$cpd/"
  cat >"$cpd/bin/ssh" <<'SH'
#!/bin/sh
case "$*" in
*" run "*)
  if [ -n "$FAKE_REWRITE" ]; then # same inode, same size, every line `exit 7`
    n=$(stat -c %s "$FAKE_REWRITE")
    yes 'exit 7' | head -c "$n" >"$FAKE_REWRITE"
  fi
  echo "remote-check: wall 1 s on fake, exit $FAKE_RC"
  exit "$FAKE_RC"
  ;;
esac
exit 0
SH
  printf '#!/bin/sh\nexit 0\n' >"$cpd/bin/rsync"
  chmod +x "$cpd/bin/ssh" "$cpd/bin/rsync"
  for c in 0: 0:rewrite 1: 1:rewrite 4:; do
    want=${c%%:*}
    cp "$me" "$cpd/remote-check.sh"
    rw=""
    [ "${c#*:}" = rewrite ] && rw=$cpd/remote-check.sh
    if FAKE_RC=$want FAKE_REWRITE=$rw PATH="$cpd/bin:$PATH" "$cpd/remote-check.sh" --log "$t/rc.log" "$r" >"$t/out" 2>&1; then got=0; else got=$?; fi
    [ "$got" = "$want" ] || { echo "remote-check self-test: check-all status $want came back as $got (${c#*:})"; cat "$t/out"; exit 1; }
    ok=$((ok + 1))
  done
  # 7. The host's start rules and --clean, run locally on a fake base.
  for g in "61 100 10" "60.01 100 10" "10 59 10" "10 100 81"; do
    # shellcheck disable=SC2086 # three numbers, split on purpose
    expect_fail "host guard '$g'" bash "$RUNNER" guard $g
  done
  for g in "60 60 80" "0.5 137 3"; do
    # shellcheck disable=SC2086
    expect_ok "host guard '$g'" bash "$RUNNER" guard $g
  done
  base=$t/base
  mkdir -p "$base/src/a/x" "$base/target/a/y" "$base/src/b" "$base/target/b" "$base/fixtures-live" "$base/home"
  for b in .. ../b a/x '' -rf; do
    expect_fail "runner clean '$b'" env ONYX_REMOTE_BASE="$base" bash "$RUNNER" clean "$b"
  done
  expect_fail "runner run with ONYX_DOCKER" env ONYX_REMOTE_BASE="$base" bash "$RUNNER" run a ONYX_DOCKER=1
  expect_ok "runner clean a" env ONYX_REMOTE_BASE="$base" ONYX_REMOTE_TOOLS="$t/tools" bash "$RUNNER" clean a
  left=$(cd "$base" && find . -mindepth 1 -maxdepth 2 | sort | paste -sd' ')
  [ "$left" = "./fixtures-live ./home ./src ./src/b ./target ./target/b" ] && [ ! -e "$base/src/a" ] ||
    { echo "remote-check self-test: clean left '$left'"; exit 1; }
  ok=$((ok + 1))
  # 7a. REMOTE-10: two runs hold both build slots, a third is refused after its wait; one ends and
  # the next takes its slot. The holders are this test's own flock processes, stopped by PID.
  mkdir -p "$base/locks"
  flock -o "$base/locks/slot-1" sleep 60 &
  h1=$!
  flock -o "$base/locks/slot-2" sleep 60 &
  h2=$!
  sleep 0.5
  expect_fail "a third run with both slots held" env ONYX_REMOTE_BASE="$base" bash "$RUNNER" slot 0
  grep -q 'REFUSED: 2 Onyx runs' "$t/out" || { echo "remote-check self-test: slot refusal"; cat "$t/out"; exit 1; }
  kill "$h2"
  wait "$h2" 2>/dev/null || true
  expect_ok "a run after one slot frees" env ONYX_REMOTE_BASE="$base" bash "$RUNNER" slot 0
  grep -q 'build slot 2 of 2' "$t/out" || { echo "remote-check self-test: slot 2 not taken"; cat "$t/out"; exit 1; }
  kill "$h1"
  wait "$h1" 2>/dev/null || true
  # 7b. REMOTE-7, structure only (systemd cannot run here; the host run checks the behaviour): the
  # scope has no swap, the runner stops its own scope after the job and when its pipe closes, and
  # ignores SIGPIPE only after the job has started.
  grep -q -- '-p MemoryMax=48G -p MemorySwapMax=0 ' "$RUNNER" &&
    [ "$(grep -c '^stop_scope$\|^  stop_scope$' "$RUNNER")" = 2 ] &&
    [ "$(grep -n "^trap '' PIPE" "$RUNNER" | cut -d: -f1)" -gt "$(grep -n '^pid=\$!' "$RUNNER" | cut -d: -f1)" ] ||
    { echo "remote-check self-test: the runner's swap cap, scope stops or PIPE order changed"; exit 1; }
  ok=$((ok + 1))
  # 8. The job's Landlock sandbox (THREATS REMOTE-4) with the job's own rules on the fake base:
  # its branch's folders, HOME and the crate cache are writable; nothing else is, and nothing
  # outside the system and Onyx's two folders can be read. The host refuses to run without Landlock.
  ll="Landlock sandbox"
  if python3 -I -c 'import ctypes as c; l = c.CDLL(None); l.syscall.restype = c.c_long; raise SystemExit(l.syscall(c.c_long(444), None, c.c_long(0), c.c_long(1)) < 1)'; then
    mkdir -p "$base/src/a" "$base/target/a" "$base/tmp/a" "$base/bin" "$t/tools/cargo-home" "$t/tools/bin"
    echo blocks >"$base/fixtures-live/x.raw.hex"
    echo tool >"$t/tools/bin/tool"
    echo private >"$t/outside.txt"
    mapfile -t rules < <(ONYX_REMOTE_BASE="$base" ONYX_REMOTE_TOOLS="$t/tools" bash "$RUNNER" policy a)
    sb() { bash "$RUNNER" sandbox "${rules[@]}" -- bash -c "$1"; }
    expect_ok "sandbox: writes the branch's folders, HOME and the crate cache; reads fixtures and tools" \
      sb "set -e; for d in src/a target/a tmp/a home; do echo x >$base/\$d/f; mkdir $base/\$d/d; mv $base/\$d/f $base/\$d/d/; rm -r $base/\$d/d; done; echo x >$t/tools/cargo-home/f; cat $base/fixtures-live/x.raw.hex $t/tools/bin/tool >/dev/null"
    expect_fail "sandbox: reads a file outside Onyx's folders" sb "cat $t/outside.txt"
    expect_fail "sandbox: lists the home folder" sb "ls $HOME"
    expect_fail "sandbox: writes another branch's sources" sb "echo x >$base/src/b/f"
    expect_fail "sandbox: writes the fixtures" sb "echo x >$base/fixtures-live/x.raw.hex"
    expect_fail "sandbox: deletes a tool" sb "rm $t/tools/bin/tool"
    expect_fail "sandbox: replaces the runner" sb "echo x >$base/bin/remote-check-runner.sh"
    expect_fail "sandbox: makes a file next to Onyx's folders" sb "echo x >$t/new.txt"
    [ "$(cat "$t/outside.txt")" = private ] && [ -f "$t/tools/bin/tool" ] && [ ! -e "$t/new.txt" ] ||
      { echo "remote-check self-test: the sandbox let a write through"; exit 1; }
  else
    ll="Landlock cases skipped: this kernel has no Landlock (the host refuses to run without it)"
  fi
  echo "remote-check self-test ok: $ok cases (arguments, file list, variables, secrets, credential files and tokens, symlinks and the real local sync, unreachable host 255, check-all's status unchanged even with this file rewritten mid-run, never-synced paths, recipes/out and submodule gates, host guard, two build slots, clean; $ll)"
  exit 0
  ;;
--dry-run)
  [ $# = 2 ] || usage
  prepare "$2"
  echo "branch folder: $BRANCH"
  echo "--- files ---"
  tr '\0' '\n' <"$LISTS/files.list"
  echo "--- end ---"
  rm -rf "$LISTS"
  exit 0
  ;;
--clean)
  [ $# = 2 ] || usage
  valid_branch "$2" || die "not a branch folder name: '$2'"
  reachable
  copy -a --mkpath "$RUNNER" "$HOST:onyx-build/bin/remote-check-runner.sh"
  exec "${SSH[@]}" "$HOST" bash onyx-build/bin/remote-check-runner.sh clean "$2" </dev/null
  ;;
--setup)
  [ $# = 1 ] || usage
  # This machine's own copies, never a download (lead rule 2026-10-18).
  repo=$(cd "$HERE/.." && pwd)
  common=$(git -C "$repo" rev-parse --path-format=absolute --git-common-dir)
  chan=$(sed -n 's/^channel = "\(.*\)"$/\1/p' "$repo/rust-toolchain.toml")
  rust=${ONYX_SETUP_RUST:-$HOME/.rustup/toolchains/$chan-x86_64-unknown-linux-gnu}
  node=${ONYX_SETUP_NODE:-$(dirname "$(dirname "$(readlink -f "$(command -v node)")")")}
  pnpm=${ONYX_SETUP_PNPM:-$(dirname "$(dirname "$(readlink -f "$(command -v pnpm)")")")}
  fix=${ONYX_SETUP_FIXTURES:-$(dirname "$common")/fixtures-live}
  [ -d "$rust/lib/rustlib/wasm32-unknown-unknown" ] || die "$rust has no wasm32-unknown-unknown target"
  [ -f "$pnpm/bin/pnpm.cjs" ] || die "no pnpm.cjs under $pnpm"
  [ -f "$fix/node/970543.raw.hex" ] || die "no fixture blocks in $fix"
  # The fixture copy holds Bitcoin block hex and block JSON only (checked before it leaves).
  odd=$(cd "$fix" && find . -mindepth 1 \( -type f -o -type l \) ! -name '*.raw.hex' ! -name '*.v3.json.gz' | head -5)
  [ -z "$odd" ] || die "unexpected files in $fix (only *.raw.hex and *.v3.json.gz may go): $odd"
  R=(nice -n 19 ionice -c 3 rsync -e "${SSH[*]}" -a --mkpath)
  "${R[@]}" --delete "$rust/" "$HOST:onyx-tools/rust/"
  "${R[@]}" --delete --include 'lib/node_modules/corepack/***' --include 'lib/node_modules/npm/***' --exclude 'lib/node_modules/*' "$node/" "$HOST:onyx-tools/node/"
  "${R[@]}" --delete "$pnpm/" "$HOST:onyx-tools/pnpm/"
  "${R[@]}" -L "$HOME/.foundry/bin/anvil" "$HOME/.foundry/bin/cast" "$HOME/.foundry/bin/forge" "$HOME/.foundry/bin/chisel" "$HOST:onyx-tools/foundry/bin/"
  "${R[@]}" "$HOME/.local/share/svm/" "$HOST:onyx-tools/svm/"
  "${R[@]}" "$(command -v wasm-bindgen)" "$(command -v cargo-deny)" "$HOST:onyx-tools/bin/"
  "${R[@]}" "$fix/" "$HOST:onyx-build/fixtures-live/"
  # shellcheck disable=SC2016 # expanded on the host
  "${SSH[@]}" "$HOST" bash -s <<'SH'
set -euo pipefail
T=$HOME/onyx-tools B=$HOME/onyx-build
find "$T/node/bin" -xtype l -delete
printf '#!/bin/sh\nexec "%s/node/bin/node" "%s/pnpm/bin/pnpm.cjs" "$@"\n' "$T" "$T" >"$T/bin/pnpm"
chmod +x "$T/bin/pnpm"
mkdir -p "$T/cargo-home" "$B/home" "$B/src" "$B/target" "$B/logs" "$B/bin"
# The week folder links into day/ by absolute paths of the source machine: relink inside the copy.
find "$B/fixtures-live" -type l | while IFS= read -r l; do
  ln -sfn "../$(basename "$(dirname "$(readlink "$l")")")/$(basename "$(readlink "$l")")" "$l"
done
broken=$(find "$B/fixtures-live" -xtype l | wc -l)
[ "$broken" = 0 ] || { echo "setup: $broken broken fixture links"; exit 1; }
for t in rust/bin/cargo rust/bin/rustc node/bin/node bin/pnpm foundry/bin/forge foundry/bin/anvil bin/wasm-bindgen bin/cargo-deny; do
  printf '%-22s %s\n' "$t" "$("$T/$t" --version 2>&1 | head -1)"
done
echo "solc: $("$T/svm/0.8.30/solc-0.8.30" --version | tail -1)"
echo "Onyx on the host: $(du -sc -B1G "$B" "$T" | awk 'END { print $1 }') GB; / free: $(df -BG --output=avail / | sed -n 2p)"
SH
  exit 0
  ;;
"") usage ;;
esac

# Run mode: [--log FILE] <worktree> [ONYX_NAME=VALUE ...]
log=""
if [ "$1" = --log ]; then
  [ $# -ge 2 ] || usage
  log=$2
  shift 2
fi
[ $# -ge 1 ] || usage
case $1 in -*) die "unknown option $1" ;; esac
wt=$1
shift
for kv in "$@"; do check_env "$kv"; done
prepare "$wt"
trap 'rm -rf "$LISTS"' EXIT
if [ -z "$log" ]; then
  log=${TMPDIR:-/tmp}/remote-check-$BRANCH-$(date -u +%Y%m%dT%H%M%SZ).log
fi
mkdir -p "$(dirname "$log")"
n=$(tr -cd '\0' <"$LISTS/files.list" | wc -c)
echo "remote-check: syncing $n files of $WT to $HOST:onyx-build/src/$BRANCH/"
reachable
copy -a --mkpath "$RUNNER" "$HOST:onyx-build/bin/remote-check-runner.sh"
sync_tree "$LISTS" "$HOST:onyx-build/src/$BRANCH/"
# shellcheck disable=SC2029 # BRANCH is checked by valid_branch; expanded here on purpose
"${SSH[@]}" "$HOST" "mkdir -p onyx-build/src/$BRANCH/.git/onyx-remote && cd onyx-build/src/$BRANCH/.git/onyx-remote && { [ ! -f files.list ] || mv -f files.list files.prev; }" </dev/null
copy -a "$LISTS/files.list" "$LISTS/tracked.list" "$LISTS/submodules.list" "$HOST:onyx-build/src/$BRANCH/.git/onyx-remote/"
echo "remote-check: check-all on $HOST ($*${*:+ }ONYX_DOCKER=0), log $log"
echo "remote-check: the Docker smoke does not run there: run ONYX_DOCKER=1 scripts/check-all.sh locally for it"
set +e
"${SSH[@]}" "$HOST" bash onyx-build/bin/remote-check-runner.sh run "$BRANCH" "$@" </dev/null >"$log" 2>&1
rc=$?
set -e
grep -E '^remote-check: (wall|REFUSED|STOPPED|stopped)' "$log" || true
if [ "$rc" = 255 ]; then echo "remote-check: ssh to $HOST failed (exit 255)"; fi
echo "remote-check: exit $rc, log $log"
exit "$rc"
}
