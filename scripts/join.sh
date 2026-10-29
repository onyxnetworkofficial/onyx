#!/usr/bin/env bash
# The join kit (north-star loop NL16, docs/join.md): one command for an operator outside Onyx to
# take part in a level-2 round as an independent observer, on a test network only.
#
#   join.sh key <key file>                       make your observer key (0600) and print its address
#   join.sh address <key file>                   print the address of an existing key
#   join.sh observe <round.json> <key file> [--rpc URL]
#                                                observe the round the coordinator announced
#
# You send the coordinator your address only; the key never leaves your machine. The round
# announcement (round.json) is the coordinator's `level2-coordinator --announce` file: the chain,
# the registry, the fact, the window and the pool snapshot. The observer checks every part of it
# against the chain and its own copy of the source's template before it commits to anything, fetches
# the public source itself over HTTPS, posts its own commit on chain, and seals its reveal on chain
# (no file transfer to the coordinator).
#
# Needs: bash, curl, and the binaries in ONYX_JOIN_BIN (default: the folder of this script's
# `bin/`): `level2-observer-<source>` for the round's source and Foundry's `cast`. Build them from
# the Onyx source with `scripts/join-build.sh` (docs/join.md "Get the programs").
# Env: ONYX_JOIN_HOME (your state, default ~/.onyx-join), ONYX_JOIN_BIN.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
BIN=${ONYX_JOIN_BIN:-$here/bin}
HOME_DIR=${ONYX_JOIN_HOME:-$HOME/.onyx-join}
usage() { sed -n '4,10p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }
[ $# -ge 2 ] || usage

# Any observer binary prints a key's address (all share the key format).
any_observer() {
  local b
  for b in "$BIN"/level2-observer-*; do [ -x "$b" ] && { echo "$b"; return; }; done
  echo "no level2-observer-<source> in $BIN (build it: docs/join.md)" >&2
  exit 1
}

case "$1" in
  key)
    key=$2
    [ ! -e "$key" ] || { echo "$key exists: refusing to overwrite a key" >&2; exit 1; }
    umask 077
    mkdir -p "$(dirname "$key")"
    # 32 random bytes from the kernel, as hex; the observer refuses a value that is not a key.
    head -c 32 /dev/urandom | od -An -v -tx1 | tr -d ' \n' >"$key"
    chmod 600 "$key"
    "$(any_observer)" --key-file "$key" --address
    ;;
  address)
    "$(any_observer)" --key-file "$2" --address
    ;;
  observe)
    [ $# -ge 3 ] || usage
    ann=$2 key=$3
    shift 3
    rpc=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --rpc) rpc=$2; shift 2 ;;
        *) usage ;;
      esac
    done
    [ -s "$ann" ] || { echo "$ann: no round announcement" >&2; exit 1; }
    # The announcement names the round's metric; its source picks the program.
    metric=$(sed -n 's/^ *"metric": *"\([a-z0-9._]*\)",\{0,1\}$/\1/p' "$ann" | head -1)
    series=$(sed -n 's/^ *"series": *"\([a-z0-9._]*\)",\{0,1\}$/\1/p' "$ann" | head -1)
    case "$metric" in
      us.treasury.*) src=fiscaldata ;;
      usgs.*) src=usgs ;;
      noaa.*) src=ncei ;;
      *) echo "$ann: unknown metric '$metric'" >&2; exit 1 ;;
    esac
    obs=$BIN/level2-observer-$src
    [ -x "$obs" ] || { echo "$obs: missing (build it: docs/join.md)" >&2; exit 1; }
    cast=$BIN/cast
    [ -x "$cast" ] || cast=$(command -v cast) || { echo "no cast in $BIN or on PATH" >&2; exit 1; }
    me=$("$obs" --key-file "$key" --address)
    state=$HOME_DIR/rounds/$(basename "$ann" .json)-$me
    mkdir -p -m 700 "$state"
    args=(--key-file "$key" --announcement "$ann" --state "$state" --cast "$cast")
    [ -z "$rpc" ] || args+=(--rpc "$rpc")
    echo "observer $me: $metric${series:+ ($series)}, state $state"
    ONYX_L2_SERIES=$series exec "$obs" "${args[@]}"
    ;;
  *) usage ;;
esac
