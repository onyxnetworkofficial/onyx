#!/usr/bin/env bash
# The publisher pointers of the API node (READINESS B5, ADR-0066 item 7; docs/ops/publisher-review.md
# "Pointers sent to the node"): list the pointers applicants sent without an account, and print one
# as a pointer file, by running `onyx-node pointers` on the server over ssh. The node keeps running:
# the tool reads the pointer journal without its lock and writes nothing.
#
# Usage: scripts/publisher-pointers.sh list [--all]
#        scripts/publisher-pointers.sh show ID            (the two lines, on stdout)
#        scripts/publisher-pointers.sh save ID FILE       (the two lines into FILE, for
#                                                          onyx-publish verify-application --pointer FILE)
#        scripts/publisher-pointers.sh drop ID            (the file at the origin is not the one this
#                                                          pointer names: free its place, say so on its receipt)
#        scripts/publisher-pointers.sh undrop ID          (take a drop back that was made by mistake)
#        scripts/publisher-pointers.sh --self-test        (offline: a fake ssh, sudo and onyx-node)
# ID:    the receipt id, ptr_ and 32 hex digits.
# Env:   ONYX_POINTERS_HOST (default ONYX_SITE_HOST, else onyx-web; `local` runs on this machine, no
#        ssh and no sudo), ONYX_POINTERS_BIN (default /usr/local/bin/onyx-node), ONYX_POINTERS_JOURNAL
#        (the node's ONYX_PUBLISHER_POINTERS, default /var/lib/onyx/state/publisher-pointers.journal).
# Exit:  0 done; 1 the journal could not be read, its name is a link or belongs to root, the server
#        could not be reached, or the answer is not a pointer; 2 a usage error (nothing was sent);
#        3 no waiting pointer has this id.
#
# drop is for one case only: `verify-application --pointer` refused the pointer because the file at
# the origin hashes to something else (`the file hashes to ..., the pointer names ...`). Anyone can
# point at any origin with any hash, and a host holds three waiting pointers: dropping the wrong
# ones gives the place back to the applicant (THREATS PTR-R1-M2). It writes one line to the drops
# journal beside the pointer journal, as the journal's owner; the node reads it within a request.
#
# This script and the node contact no origin. What a pointer names is fetched later by
# `onyx-publish verify-application`, in the CI job or on a throwaway machine, never on the server and
# never on the reviewer's own machine (docs/ops/publisher-review.md "Where the tool runs").
#
# The tool never works as root on the state directory (THREATS PUBON-PTR-7, the rule of
# REVQ-R1-H1): the node's own user owns that directory and could plant a link under a name the tool
# opens. So on the server a small root step (`sudo -n sh`) only goes into the journal's directory,
# reads the owner of the journal's name without following it, refuses a link, a file of root, a file
# with a second name or one that is not its folder's owner's, and then starts the tool as that owner
# with `setpriv`, no groups, no new privileges, on `./publisher-pointers.journal`. The root step sets
# its own PATH first. What comes back is checked here again, line by line, before it is printed or
# saved: only the list's fixed shapes pass, so nothing a server sends reaches the terminal as it is.
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { echo "publisher-pointers: $1" >&2; exit "${2:-1}"; }

# The arguments, checked here so that nothing but these fixed shapes is ever sent to the server.
check_args() { # VERB ARGS...
  local verb=$1; shift
  case "$verb" in
    list)
      [ $# = 0 ] || { [ $# = 1 ] && [ "$1" = --all ]; } || fail "list takes only --all" 2 ;;
    show|drop|undrop)
      [ $# = 1 ] || fail "$verb takes the receipt id and nothing else" 2
      [[ $1 =~ ^ptr_[0-9a-f]{32}$ ]] || fail "'$1' is not a receipt id (ptr_ and 32 hex digits)" 2 ;;
    *) fail "unknown command '$verb' (list, show, save, drop, undrop; --help)" 2 ;;
  esac
}

# The root step on the server (POSIX sh; arguments: the binary, the journal, then the tool's own
# arguments, each already checked here). It opens nothing in the state directory itself.
REMOTE=$(cat <<'SH'
PATH=/usr/sbin:/usr/bin:/sbin:/bin; export PATH
set -eu
bin=$1; j=$2; shift 2
d=${j%/*}; f=${j##*/}
cd -- "$d" || { echo "publisher-pointers: cannot enter $d" >&2; exit 1; }
o=$(stat -c '%u %g %h %F' -- "$f") || { echo "publisher-pointers: no pointer journal $j (the node writes it when ONYX_PUBLISHER_POINTERS is set)" >&2; exit 1; }
u=${o%% *}; r=${o#* }; g=${r%% *}; r=${r#* }; n=${r%% *}; t=${r#* }
case "$t" in
  "regular file" | "regular empty file") ;;
  *) echo "publisher-pointers: $j is a $t, not a regular file: refused, nothing was opened" >&2; exit 1 ;;
esac
case "$u$g$n" in *[!0-9]* | "") echo "publisher-pointers: the owner of $j could not be read" >&2; exit 1 ;; esac
[ "$u" != 0 ] || { echo "publisher-pointers: $j belongs to root: refused (the tool never works as root on the state directory)" >&2; exit 1; }
[ "$n" = 1 ] || { echo "publisher-pointers: $j has $n names (a hard link): refused, it may be another user's file" >&2; exit 1; }
du=$(stat -c '%u' -- .) || { echo "publisher-pointers: the owner of $d could not be read" >&2; exit 1; }
[ "$du" = "$u" ] || { echo "publisher-pointers: $j does not belong to the owner of its folder: refused" >&2; exit 1; }
command -v setpriv >/dev/null 2>&1 || { echo "publisher-pointers: no setpriv on the server (util-linux); on the server itself run: sudo $bin pointers ... --journal $j (the tool gives up root for the journal's owner by itself)" >&2; exit 1; }
exec setpriv --reuid="$u" --regid="$g" --clear-groups --no-new-privs -- "$bin" pointers "$@" --journal "./$f"
SH
)

# The shapes of the tool's answer (THREATS PUBON-PTR-7): a server that was taken over, or an older
# binary, must not write anything else to the reviewer's terminal or into a pointer file.
ORIGIN_RE='[a-z0-9]([a-z0-9.-]{0,251}[a-z0-9])?(/[a-z0-9][a-z0-9_-]{0,62}){0,4}'
ok_list() { # the list on stdin: 0 when every line has a fixed shape
  local line n=0
  while IFS= read -r line; do
    n=$((n + 1))
    if [ "$n" = 1 ]; then
      [[ $line =~ ^[0-9]{1,6}\ pointer\(s\)\ waiting,\ [0-9]{1,6}\ expired\ and\ kept,\ [0-9]{1,6}\ dropped\;\ the\ journal\ holds\ [0-9]{1,6}\ of\ [0-9]{1,6}\ entries$ ]] || return 1
    elif [[ $line == WARNING\ onyx-pointer-cap-alert:* ]]; then
      [[ $line =~ ^[A-Za-z0-9\ \(\)%.,:\;\"/_-]{1,400}$ ]] || return 1
    else
      [[ $line =~ ^ptr_[0-9a-f]{32}\ \ ([0-9]{1,2}\ d\ [0-9]{1,2}\ h\ left|[0-9]{1,2}\ h\ left|[0-9]{1,2}\ min\ left|expired|dropped)\ \ ${ORIGIN_RE}\ \ 0x[0-9a-f]{64}$ ]] || return 1
    fi
  done
  [ "$n" -ge 1 ]
}
ok_pointer() { # a pointer on stdin: 0 when it is exactly the two lines
  local a b rest
  IFS= read -r a || return 1
  IFS= read -r b || return 1
  ! IFS= read -r rest || return 1
  [ -z "${rest:-}" ] || return 1
  [[ $a =~ ^origin:\ ${ORIGIN_RE}$ ]] && [[ $b =~ ^application\ keccak:\ 0x[0-9a-f]{64}$ ]]
}

ok_drop() { # the answer to a drop on stdin: the receipt and "dropped", then one plain sentence
  local a b rest
  IFS= read -r a || return 1
  IFS= read -r b || return 1
  ! IFS= read -r rest || return 1
  [[ $a =~ ^ptr_[0-9a-f]{32}\ \ (not\ )?dropped$ ]] && [[ $b =~ ^[A-Za-z0-9\ .,:\;-]{1,300}$ ]]
}

# THREATS PTR-R1-L3, PTR-R2-L4: nothing the server (or ssh) wrote reaches the terminal as it is. A
# server that was taken over must not reach the reviewer's terminal with control characters, on
# either channel. stdout: the C0 control characters except tab and line end, DEL and the UTF-8 form
# of the C1 controls become "?" (other text, also non-ASCII, stays). stderr: printable ASCII, tabs
# and line ends only. Both are captured in private files first: stderr is cut to 4 KiB and stdout
# to 4 MiB while they are captured (the rest is read and dropped, so the sender is never stopped by
# a closed pipe), and the files are removed on exit, also on INT and TERM.
clean() { LC_ALL=C sed -e 's/\xc2[\x80-\x9f]/?/g' | LC_ALL=C tr '\000-\010\013-\037\177' '?'; }
clean_err() { LC_ALL=C tr -c '\n\t -~' '?'; }
keep() { head -c "$1"; cat >/dev/null; } # BYTES: the first BYTES of stdin, the rest read and dropped
captured=()
forget() { [ ${#captured[@]} = 0 ] || rm -f "${captured[@]}"; }
trap forget EXIT
trap 'forget; exit 130' INT
trap 'forget; exit 143' TERM
capture() { # NAME: a new private file named in the variable NAME, removed when the script ends
  local f
  f=$(mktemp "${TMPDIR:-/tmp}/onyx-capture.XXXXXX")
  captured+=("$f")
  printf -v "$1" %s "$f"
}
run() { # VERB ARGS...: the tool's stdout in $out (checked), its exit code back with a line on failure
  local host bin journal v rc=0 err raw
  host=${ONYX_POINTERS_HOST:-${ONYX_SITE_HOST:-onyx-web}}
  bin=${ONYX_POINTERS_BIN:-/usr/local/bin/onyx-node}
  journal=${ONYX_POINTERS_JOURNAL:-/var/lib/onyx/state/publisher-pointers.journal}
  for v in "$bin" "$journal"; do
    [[ $v =~ ^/[A-Za-z0-9._/-]+$ ]] || fail "ONYX_POINTERS_BIN and ONYX_POINTERS_JOURNAL are plain absolute paths, not '$v'" 2
  done
  # A name that starts with a letter or a digit, so it can never be read as an option.
  [[ $host =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || fail "ONYX_POINTERS_HOST is a host name or ssh alias, not '$host'" 2
  check_args "$@"
  capture err; capture raw
  if [ "$host" = local ]; then
    "$bin" pointers "$@" --journal "$journal" > >(keep 4194304 >"$raw") 2> >(keep 4096 >"$err") || rc=$?
  else
    # shellcheck disable=SC2029  # the command is built here on purpose; every part is checked above
    ssh -o BatchMode=yes -- "$host" "sudo -n sh -c $(printf '%q' "$REMOTE") sh $(printf '%q ' "$bin" "$journal" "$@")" > >(keep 4194304 >"$raw") 2> >(keep 4096 >"$err") || rc=$?
  fi
  wait # the two readers above
  clean_err <"$err" >&2
  # Filtered before anything else looks at it: what is checked, shown and saved is the same text.
  out=$(clean <"$raw")
  case "$rc" in
    0) ;;
    3) if [ "$1" = undrop ]; then echo "publisher-pointers: no drop of this id is held: nothing was written (list --all shows the dropped ones)" >&2
       else echo "publisher-pointers: no waiting pointer has this id (list --all shows the expired ones)" >&2; fi
       return 3 ;;
    2) echo "publisher-pointers: $bin did not take these arguments (see above; is the node's binary older than this script?)" >&2; return 2 ;;
    255) echo "publisher-pointers: $host could not be reached over ssh (see above)" >&2; return 1 ;;
    *) echo "publisher-pointers: $1 failed on $host with exit $rc (see above; it needs sudo -n for sh, setpriv, and a node with ONYX_PUBLISHER_POINTERS)" >&2; return 1 ;;
  esac
  case "$1" in
    list) ok_list <<<"$out" || { echo "publisher-pointers: the answer of $host is not a pointer list: not shown" >&2; return 1; } ;;
    show) ok_pointer <<<"$out" || { echo "publisher-pointers: the answer of $host is not a pointer: not shown, nothing saved" >&2; return 1; } ;;
    drop|undrop) ok_drop <<<"$out" || { echo "publisher-pointers: the answer of $host to the $1 is not in its shape: not shown (list --all says whether the pointer is dropped)" >&2; return 1; } ;;
  esac
}

self_test() {
  local t ok=0 bad=0 out rc id h
  t=$(mktemp -d "${TMPDIR:-/tmp}/onyx-publisher-pointers.XXXXXX")
  # A private temp folder: the self-tests of the sibling scripts run at the same time and use the same file names.
  mkdir "$t/tmp"; export TMPDIR="$t/tmp"
  trap 'rm -rf "$t"' RETURN
  check() { if eval "$2"; then ok=$((ok + 1)); else bad=$((bad + 1)); echo "FAIL: $1" >&2; fi; }
  id=ptr_0123456789abcdef0123456789abcdef
  h=0x$(printf 'ab%.0s' {1..32})
  mkdir -p "$t/bin"
  # The fakes, as in scripts/proposal-review.sh: ssh runs the remote command line in a shell of its
  # own, sudo checks the root step's PATH line, setpriv logs its arguments, and onyx-node logs its
  # arguments, prints $t/answer and exits with the code in $t/exit.
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
cat "$t/answer"
[ -f "$t/hostile" ] && printf 'evil \033[2J\007 text \302\233 end\n' >&2
[ -f "$t/flood" ] && { head -c 300000 /dev/zero | tr '\0' 'x' >&2; }
exit "\$(cat "$t/exit")"
FAKE
  chmod +x "$t/bin/ssh" "$t/bin/sudo" "$t/bin/setpriv" "$t/onyx-node"
  echo 0 >"$t/exit"
  mkdir -p "$t/state" "$t/elsewhere"
  echo journal >"$t/state/publisher-pointers.journal"
  local J=$t/state/publisher-pointers.journal R="--journal ./publisher-pointers.journal "
  local LIST="2 pointer(s) waiting, 1 expired and kept, 1 dropped; the journal holds 4 of 1000 entries
$id  4 d 23 h left  data.example.org/river-levels  $h
ptr_ffffffffffffffffffffffffffffffff  expired  example.org  $h
ptr_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee  dropped  example.org/a  $h"
  local POINTER="origin: data.example.org/river-levels
application keccak: $h"
  go() { # ARGS...: run the script with the fakes; sets out and rc
    rc=0
    out=$(PATH="$t/bin:$PATH" ONYX_POINTERS_HOST="${host:-fake-host}" ONYX_POINTERS_BIN="$t/onyx-node" \
      ONYX_POINTERS_JOURNAL="$J" bash "$0" "$@" 2>&1) || rc=$?
  }
  args() { tr '\n' ' ' <"$t/$1.args"; }
  local host=fake-host

  printf '%s\n' "$LIST" >"$t/answer"; go list
  check "list runs the tool over ssh, on the configured journal by its name inside its own folder, and prints the list" \
    '[ "$rc" = 0 ] && [ "$out" = "$LIST" ] && [ "$(args node)" = "pointers list $R" ] && [[ $(args ssh) == "-o BatchMode=yes -- fake-host sudo -n sh -c "* ]] && [ "$(cat "$t/setpriv.cwd")" = "$t/state" ]'
  check "root only runs sh; the tool is started as the journal's owner, read on this run, with no groups" \
    '[[ $(args sudo) == "-n sh -c "* ]] && [ "$(args setpriv)" = "--reuid=$(id -u) --regid=$(id -g) --clear-groups --no-new-privs " ]'
  check "the root step sets its own PATH on its first line" '[ -e "$t/path-line.ok" ] && [ "$(sed -n 1p <<<"$REMOTE")" = "PATH=/usr/sbin:/usr/bin:/sbin:/bin; export PATH" ]'
  go list --all
  check "list --all is passed on" '[ "$rc" = 0 ] && [ "$(args node)" = "pointers list --all $R" ]'
  printf '%s\n' "0 pointer(s) waiting, 0 expired and kept, 0 dropped; the journal holds 0 of 1000 entries" >"$t/answer"; go list
  check "an empty list is a list" '[ "$rc" = 0 ] && [[ $out == "0 pointer(s) waiting"* ]]'
  printf '%s\n' "$POINTER" >"$t/answer"; go show "$id"
  check "show prints the two lines of a pointer file" '[ "$rc" = 0 ] && [ "$out" = "$POINTER" ] && [ "$(args node)" = "pointers show $id $R" ]'
  go save "$id" "$t/pointer.txt"
  check "save writes exactly the two lines, owner-only, and says where" \
    '[ "$rc" = 0 ] && [ "$(cat "$t/pointer.txt")" = "$POINTER" ] && [ "$(stat -c %a "$t/pointer.txt")" = 600 ] && [[ $out == *"verify-application --pointer $t/pointer.txt"* ]] && [[ $out == *"throwaway machine"* ]]'
  go save "$id" "$t/pointer.txt"
  check "save never writes over a file that is there" '[ "$rc" = 2 ] && [[ $out == *"is already there"* ]]'

  printf '%s\n%s\n' "$id  dropped" "Its receipt now says: dropped, the file at the origin is not the one this pointer names. Its place is free for that host." >"$t/answer"; go drop "$id"
  check "drop names the receipt and shows the tool's two lines (PTR-R1 M2)" '[ "$rc" = 0 ] && [ "$(args node)" = "pointers drop $id $R" ] && [[ $out == "$id  dropped"*"Its place is free"* ]]'
  printf '%s\n%s\n' "$id  dropped" "It was dropped before; nothing was written." >"$t/answer"; go drop "$id"
  check "a second drop of the same pointer is said as done before" '[ "$rc" = 0 ] && [[ $out == *"dropped before"* ]]'
  printf '%b\n' "$id  dropped\nevil \x1b[2J" >"$t/answer"; go drop "$id"
  check "an answer to a drop that is out of shape is not shown" '[ "$rc" = 1 ] && [[ $out == *"not in its shape: not shown"* ]] && [[ $out != *evil* ]]'
  printf '%s\n%s\n' "$id  not dropped" "The drop was taken back. Its receipt answers by its age again, and a waiting pointer holds its place again." >"$t/answer"; go undrop "$id"
  check "undrop names the receipt and shows the tool's two lines (PTR-R2-L3)" '[ "$rc" = 0 ] && [ "$(args node)" = "pointers undrop $id $R" ] && [[ $out == "$id  not dropped"*"taken back"* ]]'
  echo 3 >"$t/exit"; : >"$t/answer"; go undrop "$id"; echo 0 >"$t/exit"
  check "an undrop of a pointer that is not dropped is exit 3 with its line (PTR-R2-L3)" '[ "$rc" = 3 ] && [[ $out == *"no drop of this id is held"* ]]'
  # THREATS PTR-R2-L4: stdout is filtered before it is checked, shown or saved; a flood on stderr
  # is cut while it is captured; no capture file stays.
  printf 'origin: data.example.org/river-levels\r\napplication keccak: %s\n' "$h" >"$t/answer"; rm -f "$t/p3.txt"; go save "$id" "$t/p3.txt"
  check "a carriage return in a pointer is not shown and nothing is saved (PTR-R2-L4)" '[ "$rc" = 1 ] && [[ $out == *"is not a pointer: not shown"* ]] && [[ $out != *$'"'"'\r'"'"'* ]] && [ ! -e "$t/p3.txt" ]'
  printf '%s\n\033]0;evil\007\302\233\n' "$LIST" >"$t/answer"; go list
  check "a list with an escape sequence, a bell or a C1 byte is not shown, and none reaches the terminal (PTR-R2-L4)" '[ "$rc" = 1 ] && [[ $out == *"is not a pointer list: not shown"* ]] && [[ $out != *$'"'"'\e'"'"'* ]] && [[ $out != *$'"'"'\a'"'"'* ]] && [[ $out != *$'"'"'\302\233'"'"'* ]] && [[ $out != *evil* ]]'
  printf '%s\n' "$LIST" >"$t/answer"; : >"$t/flood"; go list; rm -f "$t/flood"
  check "a flood on stderr is cut at 4 KiB while it is captured, and the list still prints (PTR-R2-L4)" '[ "$rc" = 0 ] && [ "$(grep -o x <<<"${out%%2 pointer*}" | wc -l)" = 4096 ] && [[ $out == *"$id  4 d 23 h left"* ]]'
  check "no capture file is left behind (PTR-R2-L4)" '[ "$(find "${TMPDIR:-/tmp}" -maxdepth 1 -name "onyx-capture.*" -newer "$t/exit" 2>/dev/null | wc -l)" = 0 ]'
  # Review PTR-R1 L3: the server's stderr reaches the reviewer's terminal only as printable text.
  : >"$t/hostile"; printf '%s\n' "$LIST" >"$t/answer"; go list
  check "the server's stderr is shown as printable text only, and the list still prints (PTR-R1 L3)" '[ "$rc" = 0 ] && [[ $out == *"evil ?[2J? text ?? end"* ]] && [[ $out != *$'"'"'\e'"'"'* ]] && [[ $out != *$'"'"'\a'"'"'* ]] && [[ $out == *"$id  4 d 23 h left"* ]]'
  echo 3 >"$t/exit"; : >"$t/answer"; go show "$id"; echo 0 >"$t/exit"
  check "also with a refusal, before the script's own line (PTR-R1 L3)" '[ "$rc" = 3 ] && [[ $out == *"evil ?[2J? text"*"no waiting pointer has this id"* ]] && [[ $out != *$'"'"'\e'"'"'* ]]'
  host=local; printf '%s\n' "$LIST" >"$t/answer"; go list
  check "and when the tool runs on this machine (PTR-R1 L3)" '[ "$rc" = 0 ] && [[ $out == *"evil ?[2J? text"* ]] && [[ $out != *$'"'"'\e'"'"'* ]]'
  host=fake-host; rm -f "$t/hostile"
  # What the server answers is checked before it is shown or saved.
  local n=0 refused=0 a
  while IFS= read -r a; do
    n=$((n + 1))
    printf '%b\n' "$a" >"$t/answer"; rm -f "$t/p2.txt"
    go show "$id"; local r1=$rc o1=$out
    go save "$id" "$t/p2.txt"
    if [ "$r1" = 1 ] && [[ $o1 == *"is not a pointer: not shown"* ]] && [[ $o1 != *evil* ]] && [ "$rc" = 1 ] && [ ! -e "$t/p2.txt" ]; then refused=$((refused + 1)); else echo "  not refused: $a" >&2; fi
  done <<BAD
origin: evil.example.org\napplication keccak: $h\nextra: line
origin: https://evil.example.org\napplication keccak: $h
origin: evil.example.org; id\napplication keccak: $h
origin: evil.example.org\napplication keccak: ${h}00
origin: evil.example.org
application keccak: $h\norigin: evil.example.org
origin: evil.example.org\x1b[2J\napplication keccak: $h
origin: Evil.example.org\napplication keccak: $h
origin: evil.example.org/a/b/c/d/e\napplication keccak: $h
BAD
  check "an answer that is not exactly a pointer's two lines is neither shown nor saved ($refused of $n)" '[ "$refused" = "$n" ]'
  n=0; refused=0
  while IFS= read -r a; do
    n=$((n + 1))
    printf '%b\n' "$a" >"$t/answer"; go list
    if [ "$rc" = 1 ] && [[ $out == *"is not a pointer list: not shown"* ]] && [[ $out != *evil* ]]; then refused=$((refused + 1)); else echo "  list not refused: $a" >&2; fi
  done <<BAD
evil first line
1 pointer(s) waiting, 0 expired and kept, 0 dropped; the journal holds 1 of 1000 entries\n$id  4 d 23 h left  evil.example.org  $h  extra
1 pointer(s) waiting, 0 expired and kept, 0 dropped; the journal holds 1 of 1000 entries\n$id  4 d 23 h left  evil.example.org\x1b[2J  $h
1 pointer(s) waiting, 0 expired and kept, 0 dropped; the journal holds 1 of 1000 entries\n$id  soon  evil.example.org  $h
1 pointer(s) waiting, 0 expired and kept, 0 dropped; the journal holds 1 of 1000 entries\nsub_0123456789abcdef0123456789abcdef  expired  evil.example.org  $h
1 pointer(s) waiting, 0 expired and kept, 0 dropped; the journal holds 1 of 1000 entries\nWARNING evil \$(id)
1 pointer(s) waiting, 0 expired and kept; the journal holds 1 of 1000 entries
BAD
  check "a list with a line out of shape is not shown ($refused of $n)" '[ "$refused" = "$n" ]'
  printf '%s\n%s\n' "1 pointer(s) waiting, 0 expired and kept, 0 dropped; the journal holds 900 of 1000 entries" \
    'WARNING onyx-pointer-cap-alert: the pointer journal holds 900 of 1000 entries (90% of the recorded cap); at the cap new pointers are refused (503 pointer_full). Entries leave by themselves 12 days after they came (docs/ops/publisher-review.md "Pointers sent to the node")' >"$t/answer"; go list
  check "the cap warning of the tool passes" '[ "$rc" = 0 ] && [[ $out == *"WARNING onyx-pointer-cap-alert"* ]]'
  printf '%s\n' "$LIST" >"$t/answer"

  # The root step refuses a journal name that is a link, missing, or root's, and then starts nothing.
  rm -f "$t/node.args"
  mv "$J" "$t/elsewhere/real.journal"; ln -s "$t/elsewhere/real.journal" "$J"; go list
  check "a journal name that is a link is refused by the root step, and the tool is not started" '[ "$rc" = 1 ] && [[ $out == *"is a symbolic link, not a regular file: refused"* ]] && [ ! -e "$t/node.args" ]'
  rm -f "$J"; go list
  check "a missing journal is said, and the tool is not started" '[ "$rc" = 1 ] && [[ $out == *"no pointer journal $J"*"ONYX_PUBLISHER_POINTERS"* ]] && [ ! -e "$t/node.args" ]'
  mv "$t/elsewhere/real.journal" "$J"
  printf '#!/bin/sh\necho "0 0 1 regular file"\n' >"$t/bin/stat"; chmod +x "$t/bin/stat"; go list; rm -f "$t/bin/stat"
  check "a journal that belongs to root is refused: the tool never runs as root there" '[ "$rc" = 1 ] && [[ $out == *"belongs to root: refused"* ]] && [ ! -e "$t/node.args" ]'
  ln "$J" "$t/elsewhere/second-name"; go list; rm -f "$t/elsewhere/second-name"
  check "a journal with a second name (a hard link) is refused" '[ "$rc" = 1 ] && [[ $out == *"has 2 names (a hard link): refused"* ]] && [ ! -e "$t/node.args" ]'
  printf '#!/bin/sh\ncase "$*" in *" .") echo 4242 ;; *) echo "%s %s 1 regular file" ;; esac\n' "$(id -u)" "$(id -g)" >"$t/bin/stat"; chmod +x "$t/bin/stat"; go list; rm -f "$t/bin/stat"
  check "a journal that is not its folder's owner's is refused" '[ "$rc" = 1 ] && [[ $out == *"does not belong to the owner of its folder: refused"* ]] && [ ! -e "$t/node.args" ]'
  go list
  check "with the journal back in place the tool runs again" '[ "$rc" = 0 ] && [ "$(args node)" = "pointers list $R" ]'

  # Nothing but the fixed shapes is sent: each of these is refused here, exit 2, with a line.
  rm -f "$t/ssh.args"
  n=0; refused=0
  while IFS= read -r a; do
    n=$((n + 1))
    # shellcheck disable=SC2086  # the planted line is split into arguments on purpose
    go $a
    if [ "$rc" = 2 ] && [[ $out == *"publisher-pointers: "* ]] && [ ! -e "$t/ssh.args" ]; then refused=$((refused + 1)); else echo "  not refused: $a (exit $rc: $out)" >&2; fi
  done <<BAD
decide $id
list --everything
list --all --all
list $id
show
show ptr_123
show PTR_0123456789ABCDEF0123456789ABCDEF
show sub_0123456789abcdef0123456789abcdef
show $id extra
show $id;id
show \$(id)
drop
drop ptr_123
drop $id --reason spam
drop $id extra
undrop
undrop ptr_123
undrop $id extra
save $id
save $id $t/a $t/b
save ptr_1 $t/a
BAD
  check "a wrong command or id is refused before anything is sent ($refused of $n)" '[ "$refused" = "$n" ]'

  echo 3 >"$t/exit"; : >"$t/answer"; go show "$id"
  check "an unknown or expired receipt is exit 3 with its line" '[ "$rc" = 3 ] && [[ $out == *"no waiting pointer has this id"* ]]'
  echo 1 >"$t/exit"; go list
  check "a journal that cannot be read is exit 1 with its line" '[ "$rc" = 1 ] && [[ $out == *"publisher-pointers: list failed on fake-host with exit 1"*"sudo -n"* ]]'
  echo 2 >"$t/exit"; go list
  check "an older binary that does not know the command is exit 2 with its line" '[ "$rc" = 2 ] && [[ $out == *"did not take these arguments"* ]]'
  echo 0 >"$t/exit"; printf '%s\n' "$LIST" >"$t/answer"; : >"$t/ssh.down"; rm -f "$t/node.args"; go list
  check "a server that cannot be reached is exit 1 with its line, and nothing ran" '[ "$rc" = 1 ] && [[ $out == *"fake-host could not be reached over ssh"* ]] && [ ! -e "$t/node.args" ]'
  rm -f "$t/ssh.down" "$t/ssh.args" "$t/sudo.args" "$t/setpriv.args"
  host=local; go list
  check "ONYX_POINTERS_HOST=local runs the tool here as this user, with no ssh, no sudo and no setpriv" '[ "$rc" = 0 ] && [ "$(args node)" = "pointers list --journal $J " ] && [ ! -e "$t/ssh.args" ] && [ ! -e "$t/sudo.args" ] && [ ! -e "$t/setpriv.args" ]'
  host=fake-host
  rc=0; out=$(ONYX_POINTERS_BIN='/bin/x;id' bash "$0" list 2>&1) || rc=$?
  check "a binary path that is not a plain path is refused" '[ "$rc" = 2 ] && [[ $out == *"plain absolute paths"* ]]'
  local refused_hosts=0
  for h in 'host -oProxyCommand=x' '-oProxyCommand=x' '-v' '.host' 'a;b' 'a@b' ''; do
    rc=0; out=$(PATH="$t/bin:$PATH" ONYX_SITE_HOST= ONYX_POINTERS_HOST="${h:- }" bash "$0" list 2>&1) || rc=$?
    if [ "$rc" = 2 ] && [[ $out == *"host name or ssh alias"* ]] && [ ! -e "$t/ssh.args" ]; then refused_hosts=$((refused_hosts + 1)); else echo "  host not refused: '$h'" >&2; fi
  done
  check "a host that is not a plain name, or starts like an option, is refused before ssh" '[ "$refused_hosts" = 7 ]'
  check "this script names no fetching program: it contacts no origin" '! grep -nE "(^|[^a-z-])(curl|wget|nc|openssl s_client)( |$)" "$0" | grep -v "grep -nE"'
  rc=0; out=$(bash "$0" 2>&1) || rc=$?
  check "no command prints the usage, exit 2" '[ "$rc" = 2 ] && [[ $out == *"Usage: scripts/publisher-pointers.sh list"* ]]'
  echo "publisher-pointers self-test: $ok passed, $bad failed"
  [ "$bad" = 0 ]
}

case "${1:-}" in
  --self-test) self_test; exit ;;
  -h|--help) sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
  '') sed -n '7,14p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2 ;;
  save)
    [ $# = 3 ] || fail "save takes the receipt id and a file" 2
    [[ $2 =~ ^ptr_[0-9a-f]{32}$ ]] || fail "'$2' is not a receipt id (ptr_ and 32 hex digits)" 2
    [ ! -e "$3" ] && [ ! -L "$3" ] || fail "$3 is already there: name a new file" 2
    out=''
    run show "$2" || exit $?
    (umask 077 && set -o noclobber && printf '%s\n' "$out" >"$3") || fail "could not write $3"
    echo "saved to $3. Next, in the CI job or on a throwaway machine (never on the server, never on your own machine): onyx-publish verify-application --pointer $3 ... (docs/ops/publisher-review.md)"
    exit 0 ;;
esac
out=''
run "$@" || exit $?
printf '%s\n' "$out"
