#!/usr/bin/env bash
# The proposal review queue of the API node (READINESS B2, ROADMAP S5; docs/ops/proposal-review.md):
# list the proposals waiting for a decision, show one, and decide it (clear, hold, drop), by running
# `onyx-node proposals` on the server over ssh. The node keeps running: the tool reads the intake
# journal without its lock and appends each decision to the decisions journal beside it.
#
# Usage: scripts/proposal-review.sh list [--all]
#        scripts/proposal-review.sh show ID
#        scripts/proposal-review.sh clear ID --terms-reread yes --no-personal-data yes \
#                                   --entities-only yes --source-public yes --rights-level allowed|restricted
#        scripts/proposal-review.sh hold ID --reason CODE
#        scripts/proposal-review.sh drop ID --reason CODE
#        scripts/proposal-review.sh --self-test    (offline: a fake ssh, sudo and onyx-node)
# CODE:  terms_forbid, personal_data, not_entities, duplicate, unreachable_source, out_of_scope, spam,
#        or other:CODE (1 to 32 of a-z, 0-9 and _). ID: the receipt id, sub_ and 32 hex digits.
# Env:   ONYX_REVIEW_HOST (default ONYX_SITE_HOST, else onyx-web; `local` runs on this machine, no ssh
#        and no sudo), ONYX_REVIEW_BIN (default /usr/local/bin/onyx-node), ONYX_REVIEW_JOURNAL (the
#        node's ONYX_SUBMISSIONS, default /var/lib/onyx/state/submissions.journal).
# Exit:  0 done; 1 the journals could not be read or written, the journal's name is a link or
#        belongs to root, or the server could not be reached; 2 a usage error (nothing was sent);
#        3 the decision was refused (unknown id, already decided, an incomplete checklist, a reason
#        outside the list), nothing written.
#
# Who may decide: whoever may run this on the server, which is whoever can write the node's state
# directory (THREATS REV-Q-1). The tool never works as root there (THREATS REVQ-R1-H1): the node's own
# user owns that directory and could plant a link under a name the tool opens. So on the server a
# small root step (`sudo -n sh`) only goes into the journal's directory, reads the owner of the
# journal's name without following it (the node runs under a user systemd makes up, so the number is
# read on every run), refuses a link, a file of root, a file with a second name (a hard link) or one
# that is not its folder's owner's (THREATS REVQ-R2-L2), and then starts the tool as that owner with
# `setpriv`, no groups, no new privileges, on `./submissions.journal`. The root step sets its own PATH
# first: it never runs a program from the node's folder (REVQ-R2-L1). The tool itself refuses links and, if it is ever
# started as root, gives up root for the journal's owner before it opens anything. `show` prints a
# proposal's text: it is private to the reviewer, never paste it anywhere public. `clear` verifies
# and publishes nothing.
set -euo pipefail
cd "$(dirname "$0")/.."

# THREATS REV-Q-10: nothing but the fixed shapes checked below ever reaches the server's shell.
fail() { echo "proposal-review: $1" >&2; exit "${2:-1}"; }

# The arguments, checked here so that nothing but these fixed shapes is ever sent to the server.
check_args() { # VERB ARGS...: prints nothing; exit 2 with a line on a wrong argument
  local verb=$1; shift
  local id='' reason='' n=0
  case "$verb" in
    list)
      [ $# = 0 ] || { [ $# = 1 ] && [ "$1" = --all ]; } || fail "list takes only --all" 2
      return 0 ;;
    show|clear|hold|drop) ;;
    *) fail "unknown command '$verb' (list, show, clear, hold, drop; --help)" 2 ;;
  esac
  [ $# -ge 1 ] || fail "$verb needs a receipt id (sub_ and 32 hex digits)" 2
  id=$1; shift
  [[ $id =~ ^sub_[0-9a-f]{32}$ ]] || fail "'$id' is not a receipt id (sub_ and 32 hex digits)" 2
  case "$verb" in
    show) [ $# = 0 ] || fail "show takes only the receipt id" 2 ;;
    hold|drop)
      [ $# = 2 ] && [ "$1" = --reason ] || fail "$verb needs --reason CODE and nothing else" 2
      reason=$2
      [[ $reason =~ ^(terms_forbid|personal_data|not_entities|duplicate|unreachable_source|out_of_scope|spam|other:[a-z0-9_]{1,32})$ ]] \
        || fail "'$reason' is not a reason code (terms_forbid, personal_data, not_entities, duplicate, unreachable_source, out_of_scope, spam, other:CODE)" 2 ;;
    clear)
      while [ $# -gt 0 ]; do
        case "$1" in
          --terms-reread|--no-personal-data|--entities-only|--source-public)
            [ $# -ge 2 ] && [[ $2 =~ ^(yes|no)$ ]] || fail "$1 is answered yes or no" 2 ;;
          --rights-level)
            [ $# -ge 2 ] && [[ $2 =~ ^(allowed|restricted)$ ]] || fail "--rights-level is allowed or restricted" 2 ;;
          *) fail "clear does not take '$1' (the checklist lines: --terms-reread, --no-personal-data, --entities-only, --source-public, --rights-level)" 2 ;;
        esac
        n=$((n + 1)); shift 2
      done
      # A missing or "no" line is left to the tool: it refuses the clear and names the line (exit 3).
      ;;
  esac
}

# The root step on the server (POSIX sh; arguments: the binary, the journal, then the tool's own
# arguments, each already checked here). It opens nothing in the state directory itself.
REMOTE=$(cat <<'SH'
PATH=/usr/sbin:/usr/bin:/sbin:/bin; export PATH
set -eu
bin=$1; j=$2; shift 2
d=${j%/*}; f=${j##*/}
cd -- "$d" || { echo "proposal-review: cannot enter $d" >&2; exit 1; }
o=$(stat -c '%u %g %h %F' -- "$f") || { echo "proposal-review: no intake journal $j (the node writes it when ONYX_SUBMISSIONS is set)" >&2; exit 1; }
u=${o%% *}; r=${o#* }; g=${r%% *}; r=${r#* }; n=${r%% *}; t=${r#* }
case "$t" in
  "regular file" | "regular empty file") ;;
  *) echo "proposal-review: $j is a $t, not a regular file: refused, nothing was opened" >&2; exit 1 ;;
esac
case "$u$g$n" in *[!0-9]* | "") echo "proposal-review: the owner of $j could not be read" >&2; exit 1 ;; esac
[ "$u" != 0 ] || { echo "proposal-review: $j belongs to root: refused (the tool never works as root on the state directory)" >&2; exit 1; }
[ "$n" = 1 ] || { echo "proposal-review: $j has $n names (a hard link): refused, it may be another user's file" >&2; exit 1; }
du=$(stat -c '%u' -- .) || { echo "proposal-review: the owner of $d could not be read" >&2; exit 1; }
[ "$du" = "$u" ] || { echo "proposal-review: $j does not belong to the owner of its folder: refused" >&2; exit 1; }
command -v setpriv >/dev/null 2>&1 || { echo "proposal-review: no setpriv on the server (util-linux); on the server itself run: sudo $bin proposals ... --journal $j (the tool gives up root for the journal's owner by itself)" >&2; exit 1; }
exec setpriv --reuid="$u" --regid="$g" --clear-groups --no-new-privs -- "$bin" proposals "$@" --journal "./$f"
SH
)

run() { # VERB ARGS...: run the tool where the node is; the tool's exit code, with a line on failure
  local host bin journal v rc=0
  host=${ONYX_REVIEW_HOST:-${ONYX_SITE_HOST:-onyx-web}}
  bin=${ONYX_REVIEW_BIN:-/usr/local/bin/onyx-node}
  journal=${ONYX_REVIEW_JOURNAL:-/var/lib/onyx/state/submissions.journal}
  for v in "$bin" "$journal"; do
    [[ $v =~ ^/[A-Za-z0-9._/-]+$ ]] || fail "ONYX_REVIEW_BIN and ONYX_REVIEW_JOURNAL are plain absolute paths, not '$v'" 2
  done
  # THREATS REVQ-R1-L3: a name that starts with a letter or a digit, so it can never be read as an option.
  [[ $host =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || fail "ONYX_REVIEW_HOST is a host name or ssh alias, not '$host'" 2
  check_args "$@"
  if [ "$host" = local ]; then
    "$bin" proposals "$@" --journal "$journal" || rc=$?
  else
    # shellcheck disable=SC2029  # the command is built here on purpose; every part is checked above
    ssh -o BatchMode=yes -- "$host" "sudo -n sh -c $(printf '%q' "$REMOTE") sh $(printf '%q ' "$bin" "$journal" "$@")" || rc=$?
  fi
  case "$rc" in
    0) return 0 ;;
    3) echo "proposal-review: the decision was refused and nothing was written (see above)" >&2 ;;
    2) echo "proposal-review: $bin did not take these arguments (see above; is the node's binary older than this script?)" >&2 ;;
    255) echo "proposal-review: $host could not be reached over ssh (see above); nothing was decided" >&2; rc=1 ;;
    *) echo "proposal-review: $1 failed on $host with exit $rc (see above; it needs sudo -n for sh, setpriv, and a node with ONYX_SUBMISSIONS)" >&2; rc=1 ;;
  esac
  return "$rc"
}
self_test() {
  local t ok=0 bad=0 out rc id
  t=$(mktemp -d "${TMPDIR:-/tmp}/onyx-proposal-review.XXXXXX")
  trap 'rm -rf "$t"' RETURN
  check() { if eval "$2"; then ok=$((ok + 1)); else bad=$((bad + 1)); echo "FAIL: $1" >&2; fi; }
  id=sub_0123456789abcdef0123456789abcdef
  mkdir -p "$t/bin"
  # The fakes: ssh runs the remote command line in a shell of its own (as sshd does), sudo drops -n,
  # setpriv logs its arguments and runs what follows `--`, and onyx-node logs its arguments one per
  # line and exits with the code in $t/exit. The journal is a real file in a real folder, so the
  # root step's own stat reads this user as its owner.
  cat >"$t/bin/ssh" <<FAKE
#!/usr/bin/env bash
printf '%s\n' "\$@" >"$t/ssh.args"
[ -f "$t/ssh.down" ] && { echo "ssh: connect to host: fake failure" >&2; exit 255; }
[ "\$3" = -- ] || { echo "fake ssh: no -- before the host" >&2; exit 98; }
shift 4
PATH="$t/bin:\$PATH" exec bash -c "\$1"
FAKE
  cat >"$t/bin/setpriv" <<FAKE
#!/usr/bin/env bash
: >"$t/setpriv.args"
while [ "\$1" != -- ]; do printf '%s\n' "\$1" >>"$t/setpriv.args"; shift; done
shift
pwd >"$t/setpriv.cwd"
exec "\$@"
FAKE
  cat >"$t/bin/sudo" <<FAKE
#!/usr/bin/env bash
printf '%s\n' "\$@" >"$t/sudo.args"
[ "\$1" = -n ] || exit 97
# The root step must set its own PATH before anything else (THREATS REVQ-R2-L1). The fake checks that
# first line, then puts the fakes' folder in front so that stat and setpriv below are the fakes.
first=\$(printf '%s\n' "\$4" | sed -n 1p)
[ "\$2 \$3" = "sh -c" ] && [ "\$first" = "PATH=/usr/sbin:/usr/bin:/sbin:/bin; export PATH" ] || { echo "fake sudo: the root step does not start with its PATH line" >&2; exit 96; }
: >"$t/path-line.ok"
body=\$(printf '%s\n' "\$4" | sed "1s#.*#PATH=$t/bin:/usr/bin:/bin; export PATH#")
shift 4
exec sh -c "\$body" "\$@"
FAKE
  cat >"$t/onyx-node" <<FAKE
#!/usr/bin/env bash
printf '%s\n' "\$@" >"$t/node.args"
echo "fake tool ran: \$2"
exit "\$(cat "$t/exit")"
FAKE
  chmod +x "$t/bin/ssh" "$t/bin/sudo" "$t/bin/setpriv" "$t/onyx-node"
  echo 0 >"$t/exit"
  mkdir -p "$t/state" "$t/elsewhere"
  echo journal >"$t/state/submissions.journal"
  local J=$t/state/submissions.journal R="--journal ./submissions.journal "
  go() { # ARGS...: run the script's own `run` with the fakes; sets out and rc
    rc=0
    out=$(PATH="$t/bin:$PATH" ONYX_REVIEW_HOST="${host:-fake-host}" ONYX_REVIEW_BIN="$t/onyx-node" \
      ONYX_REVIEW_JOURNAL="$J" bash "$0" "$@" 2>&1) || rc=$?
  }
  args() { tr '\n' ' ' <"$t/$1.args"; }
  local host=fake-host

  go list
  check "list runs the tool over ssh, on the configured journal by its name inside its own folder" \
    '[ "$rc" = 0 ] && [[ $out == *"fake tool ran: list"* ]] && [ "$(args node)" = "proposals list $R" ] && [[ $(args ssh) == "-o BatchMode=yes -- fake-host sudo -n sh -c "* ]] && [ "$(cat "$t/setpriv.cwd")" = "$t/state" ]'
  check "root only runs sh; the tool is started as the journal's owner, read on this run, with no groups (B2-R1 H1)" \
    '[[ $(args sudo) == "-n sh -c "* ]] && [ "$(args setpriv)" = "--reuid=$(id -u) --regid=$(id -g) --clear-groups --no-new-privs " ]'
  check "the root step sets its own PATH on its first line, before it enters the folder of the node (B2-R2 L1)" '[ -e "$t/path-line.ok" ] && [ "$(sed -n 1p <<<"$REMOTE")" = "PATH=/usr/sbin:/usr/bin:/sbin:/bin; export PATH" ]'
  go list --all
  check "list --all is passed on" '[ "$rc" = 0 ] && [ "$(args node)" = "proposals list --all $R" ]'
  go show "$id"
  check "show names the receipt" '[ "$rc" = 0 ] && [ "$(args node)" = "proposals show $id $R" ]'
  go clear "$id" --terms-reread yes --no-personal-data yes --entities-only yes --source-public yes --rights-level restricted
  check "clear carries every checklist answer as given" \
    '[ "$rc" = 0 ] && [ "$(args node)" = "proposals clear $id --terms-reread yes --no-personal-data yes --entities-only yes --source-public yes --rights-level restricted $R" ]'
  go hold "$id" --reason unreachable_source
  check "hold carries its reason code" '[ "$rc" = 0 ] && [ "$(args node)" = "proposals hold $id --reason unreachable_source $R" ]'
  go drop "$id" --reason other:no_answer_2
  check "drop takes other:CODE" '[ "$rc" = 0 ] && [ "$(args node)" = "proposals drop $id --reason other:no_answer_2 $R" ]'

  # Review B2-R1 H1: the root step refuses a journal name that is a link, missing, or root's, and
  # then starts nothing.
  rm -f "$t/node.args"
  mv "$J" "$t/elsewhere/real.journal"; ln -s "$t/elsewhere/real.journal" "$J"; go list
  check "a journal name that is a link is refused by the root step, and the tool is not started" '[ "$rc" = 1 ] && [[ $out == *"is a symbolic link, not a regular file: refused"* ]] && [ ! -e "$t/node.args" ]'
  rm -f "$J"; go list
  check "a missing journal is said, and the tool is not started" '[ "$rc" = 1 ] && [[ $out == *"no intake journal $J"* ]] && [ ! -e "$t/node.args" ]'
  mkdir "$J"; go list; rmdir "$J"
  check "a folder under the journal's name is refused" '[ "$rc" = 1 ] && [[ $out == *"is a directory, not a regular file"* ]] && [ ! -e "$t/node.args" ]'
  mv "$t/elsewhere/real.journal" "$J"
  printf '#!/bin/sh\necho "0 0 1 regular file"\n' >"$t/bin/stat"; chmod +x "$t/bin/stat"; go drop "$id" --reason spam; rm -f "$t/bin/stat"
  check "a journal that belongs to root is refused: the tool never runs as root there" '[ "$rc" = 1 ] && [[ $out == *"belongs to root: refused"* ]] && [ ! -e "$t/node.args" ]'
  printf '#!/bin/sh\necho "x; id 1 regular file"\n' >"$t/bin/stat"; chmod +x "$t/bin/stat"; go list; rm -f "$t/bin/stat"
  check "an owner that is not a number is refused" '[ "$rc" = 1 ] && [[ $out == *"could not be read"* ]] && [ ! -e "$t/node.args" ]'
  ln "$J" "$t/elsewhere/second-name"; go list; rm -f "$t/elsewhere/second-name"
  check "a journal with a second name (a hard link) is refused (B2-R2 L2)" '[ "$rc" = 1 ] && [[ $out == *"has 2 names (a hard link): refused"* ]] && [ ! -e "$t/node.args" ]'
  printf '#!/bin/sh\ncase "$*" in *" .") echo 4242 ;; *) echo "%s %s 1 regular file" ;; esac\n' "$(id -u)" "$(id -g)" >"$t/bin/stat"; chmod +x "$t/bin/stat"; go list; rm -f "$t/bin/stat"
  check "a journal that is not its folder's owner's is refused (B2-R2 L2)" '[ "$rc" = 1 ] && [[ $out == *"does not belong to the owner of its folder: refused"* ]] && [ ! -e "$t/node.args" ]'
  go list
  check "with the journal back in place the tool runs again" '[ "$rc" = 0 ] && [ "$(args node)" = "proposals list $R" ]'

  # Nothing but the fixed shapes is sent: each of these is refused here, exit 2, with a line.
  rm -f "$t/ssh.args"
  local n=0 refused=0 a
  while IFS= read -r a; do
    n=$((n + 1))
    # shellcheck disable=SC2086  # the planted line is split into arguments on purpose
    go $a
    if [ "$rc" = 2 ] && [[ $out == *"proposal-review: "* ]] && [ ! -e "$t/ssh.args" ]; then refused=$((refused + 1)); else echo "  not refused: $a (exit $rc: $out)" >&2; fi
  done <<BAD
decide $id
list --everything
list --all --all
show
show sub_123
show SUB_0123456789ABCDEF0123456789ABCDEF
show $id extra
hold $id
hold $id --reason
hold $id --reason because
hold $id --reason other
hold $id --reason other:Has-Dash
hold $id --reason spam extra
drop $id --reason spam;id
drop \$(id) --reason spam
clear $id --terms-reread maybe
clear $id --rights-level forbidden
clear $id --reason spam
clear $id --terms-reread
BAD
  check "a wrong command, id, reason or checklist answer is refused before anything is sent ($refused of $n)" '[ "$refused" = "$n" ]'
  go clear "$id" --terms-reread no
  check "a no on a checklist line is sent on, for the tool to refuse by the line's name" '[ "$rc" = 0 ] && [ "$(args node)" = "proposals clear $id --terms-reread no $R" ]'

  # Every failure prints a clear line and keeps its meaning.
  echo 3 >"$t/exit"; go drop "$id" --reason spam
  check "a refused decision is exit 3 with its line" '[ "$rc" = 3 ] && [[ $out == *"proposal-review: the decision was refused and nothing was written"* ]]'
  echo 1 >"$t/exit"; go list
  check "a journal that cannot be read is exit 1 with its line" '[ "$rc" = 1 ] && [[ $out == *"proposal-review: list failed on fake-host with exit 1"*"sudo -n"* ]]'
  echo 2 >"$t/exit"; go list
  check "an older binary that does not know the command is exit 2 with its line" '[ "$rc" = 2 ] && [[ $out == *"did not take these arguments"* ]]'
  echo 0 >"$t/exit"; : >"$t/ssh.down"; rm -f "$t/node.args"; go list
  check "a server that cannot be reached is exit 1 with its line, and nothing ran" '[ "$rc" = 1 ] && [[ $out == *"fake-host could not be reached over ssh"*"nothing was decided"* ]] && [ ! -e "$t/node.args" ]'
  rm -f "$t/ssh.down" "$t/ssh.args" "$t/sudo.args" "$t/setpriv.args"
  host=local; go show "$id"
  check "ONYX_REVIEW_HOST=local runs the tool here as this user, with no ssh, no sudo and no setpriv" '[ "$rc" = 0 ] && [ "$(args node)" = "proposals show $id --journal $J " ] && [ ! -e "$t/ssh.args" ] && [ ! -e "$t/sudo.args" ] && [ ! -e "$t/setpriv.args" ]'
  host=fake-host
  rc=0; out=$(ONYX_REVIEW_BIN='/bin/x;id' bash "$0" list 2>&1) || rc=$?
  check "a binary path that is not a plain path is refused" '[ "$rc" = 2 ] && [[ $out == *"plain absolute paths"* ]]'
  local h refused_hosts=0
  for h in 'host -oProxyCommand=x' '-oProxyCommand=x' '-v' '.host' 'a;b' 'a@b' ''; do
    rc=0; out=$(PATH="$t/bin:$PATH" ONYX_SITE_HOST= ONYX_REVIEW_HOST="${h:- }" bash "$0" list 2>&1) || rc=$?
    if [ "$rc" = 2 ] && [[ $out == *"host name or ssh alias"* ]] && [ ! -e "$t/ssh.args" ]; then refused_hosts=$((refused_hosts + 1)); else echo "  host not refused: '$h'" >&2; fi
  done
  check "a host that is not a plain name, or starts like an option, is refused before ssh (B2-R1 L3)" '[ "$refused_hosts" = 7 ]'
  rc=0; out=$(bash "$0" 2>&1) || rc=$?
  check "no command prints the usage, exit 2" '[ "$rc" = 2 ] && [[ $out == *"Usage: scripts/proposal-review.sh list"* ]]'
  echo "proposal-review self-test: $ok passed, $bad failed"
  [ "$bad" = 0 ]
}

case "${1:-}" in
  --self-test) self_test; exit ;;
  -h|--help) sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
  '') sed -n '7,13p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2 ;;
esac
run "$@"
