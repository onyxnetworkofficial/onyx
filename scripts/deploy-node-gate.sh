#!/usr/bin/env bash
# The checks scripts/deploy-api.sh runs ON THE SERVER before it changes anything there (review
# LIM-R1 H1 and M4 of ADR-0066 slice 2). deploy-api.sh copies this file into its staging folder
# and runs it first; a refusal leaves the running node, its binary and /etc/onyx as they were.
#
# Usage: deploy-node-gate.sh STAGE      (STAGE: the staging folder, with node.env, onyx-node,
#                                        onyx-node.service and the files that go to /etc/onyx)
#        deploy-node-gate.sh --self-test (offline: a fake server root and a fake onyx-node)
#
# 1. Limits per publisher are not turned off by accident. When the server has
#    /etc/onyx/publishers.json and STAGE has none, the deploy is refused: an export without the
#    file would give every publisher the node's one quota again (THREATS PUBON-6). To turn the
#    limits off on purpose: ONYX_API_LIMITS_OFF=1. A staged file that is empty is refused.
# 2. The NEW binary reads the NEW settings: `onyx-node --check-config`, the call the unit runs as
#    ExecStartPre, on the staged node.env with each /etc/onyx path replaced by its staged file
#    (a path with no staged file keeps pointing at the live one) and the unit's own Environment=
#    lines. So a limits file the node refuses (a grant over the total, a bad field), or any other
#    setting it refuses, stops the deploy before the old node is stopped. --check-config only
#    reads, and its lines are shown only when it fails.
# 3. Whether the new binary keeps a share of its requests in flight per client (THREATS NL5-3):
#    its --check-config then prints "<number> in flight (ONYX_MAX_INFLIGHT_PER_CLIENT)". Only
#    then does the gate leave the file STAGE/node-shares-in-flight, and only with that file does
#    deploy-api.sh load the firewall's connection cap of 128 per address: in front of a node
#    without the share (an older binary, or the share set to 0) that cap would let one address
#    hold half the node. Without the line the gate says so in capitals and the deploy goes on
#    with the firewall as it is.
#
# Test hooks (the self-test only): ONYX_GATE_ROOT, a folder standing in for the server's root, and
# ONYX_GATE_SUDO, the command put before what needs root (default sudo; empty in the self-test).
set -euo pipefail
self=$(realpath "$0")
repo=$(dirname "$(dirname "$self")")

die() { echo "deploy-node-gate: $*" >&2; exit 1; }

gate() {
  local stage=$1 root=${ONYX_GATE_ROOT:-} out rc=0 key=()
  local -a sudo=() vars=()
  if [ -n "${ONYX_GATE_SUDO-sudo}" ]; then sudo=("${ONYX_GATE_SUDO-sudo}"); fi
  for f in node.env onyx-node onyx-node.service; do
    [ -s "$stage/$f" ] || die "the staging folder has no $f"
  done
  # Never a marker from anywhere else: only this run's check-config may leave it (step 3).
  rm -f "$stage/node-shares-in-flight"
  # 1. Limits per publisher.
  if [ -e "$stage/publishers.json" ]; then
    [ -s "$stage/publishers.json" ] || die "the staged publishers.json is empty: not a limits file. Nothing was changed"
    grep -qx 'ONYX_BULK_PUBLISHERS=/etc/onyx/publishers.json' "$stage/node.env" \
      || die "publishers.json is staged but node.env does not name it. Nothing was changed"
  elif "${sudo[@]}" test -e "$root/etc/onyx/publishers.json"; then
    if [ "${ONYX_API_LIMITS_OFF:-0}" = 1 ]; then
      echo "NOTE: limits per publisher go OFF with this deploy (ONYX_API_LIMITS_OFF=1): one quota for every publisher"
    else
      die "the server has limits per publisher (/etc/onyx/publishers.json) and this export has no publishers.json: the deploy would turn them off. Put the file in the export (scripts/export-from-backup.sh rebuilds one from a backup), or set ONYX_API_LIMITS_OFF=1 to turn them off on purpose. Nothing was changed"
    fi
  fi
  # 2. The new binary on the new settings, before the old node is stopped.
  mapfile -d '' vars < <(python3 -I - "$stage" <<'PY'
import os, sys
stage = sys.argv[1]
def staged(item):
    # `/etc/onyx/x` or `name=/etc/onyx/x`: the staged file when there is one.
    head, sep, path = item.rpartition("=")
    if path.startswith("/etc/onyx/"):
        local = os.path.join(stage, os.path.normpath(path[len("/etc/onyx/"):]))
        if not os.path.normpath(path).startswith("/etc/onyx/") or not os.path.exists(local):
            return item
        return head + sep + local
    return item
out = []
for raw in open(os.path.join(stage, "node.env"), encoding="utf-8"):
    line = raw.rstrip("\n")
    if not line or line.startswith("#") or "=" not in line:
        continue
    k, v = line.split("=", 1)
    out.append(k + "=" + ",".join(staged(i) for i in v.split(",")))
# The unit's own settings (state and row folders); the key path is set by the caller.
for raw in open(os.path.join(stage, "onyx-node.service"), encoding="utf-8"):
    line = raw.strip()
    if line.startswith("Environment=ONYX_") and "%" not in line:
        out.append(line[len("Environment="):])
sys.stdout.write("".join(v + "\0" for v in out))
PY
  )
  [ ${#vars[@]} -gt 0 ] || die "no setting could be read from the staged node.env"
  if [ -f "$stage/operator.key" ] && grep -q '^Environment=ONYX_OPERATOR_KEY_FILE=' "$stage/onyx-node.service"; then
    chmod 600 "$stage/operator.key"
    key=("ONYX_OPERATOR_KEY_FILE=$stage/operator.key")
  fi
  out=$("${sudo[@]}" env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin "${vars[@]}" "${key[@]}" "$stage/onyx-node" --check-config 2>&1) || rc=$?
  if [ "$rc" != 0 ]; then
    printf '%s\n' "$out" >&2
    die "the new onyx-node refuses the new settings (--check-config, exit $rc). The running node was not stopped and nothing was changed"
  fi
  echo "check-config: the new binary takes the new settings"
  # 3. The share of requests in flight per client, in the new binary's own words.
  if grep -qE '(^|[^0-9a-z])[1-9][0-9]* in flight \(ONYX_MAX_INFLIGHT_PER_CLIENT\)' <<<"$out"; then
    : >"$stage/node-shares-in-flight"
    echo "in-flight share: the new binary limits requests in flight per client"
  else
    echo "WARNING: THE NEW BINARY DOES NOT LIMIT REQUESTS IN FLIGHT PER CLIENT (no '<number> in flight (ONYX_MAX_INFLIGHT_PER_CLIENT)' line in its --check-config): the connection cap file will NOT be loaded by this deploy"
  fi
}

self_test() {
  local t pass=0 failed=0 out rc s
  t=$(mktemp -d "${TMPDIR:-/tmp}/deploy-node-gate-test.XXXXXX")
  # shellcheck disable=SC2064
  trap "rm -rf '$t'" EXIT
  ok() { echo "ok: $1"; pass=$((pass + 1)); }
  bad() { echo "FAIL: $1"; failed=$((failed + 1)); }
  check() { if eval "$2"; then ok "$1"; else bad "$1"; fi; }
  # A staging folder as deploy-api.sh leaves it on the server, and a server root.
  stage() { # DIR
    s=$1
    mkdir -p "$s/observations" "$t/root/etc/onyx" "$t/root/var/lib/onyx/state"
    cat >"$s/node.env" <<'EOF'
ONYX_ENV=testnet
ONYX_DEPLOYMENT=/etc/onyx/deployment.json
ONYX_CATALOG=/etc/onyx/catalog.json
ONYX_OBSERVATIONS=/etc/onyx/observations/a.json,/etc/onyx/observations/b.json
ONYX_OBSERVATION_TEMPLATES=m.a=0x0a,m.b=0x0b
ONYX_SIGNED_REPORTS=/etc/onyx/signed/post.log
ONYX_BULK_DIR=/var/lib/onyx/bulk
ONYX_BULK_MAX_GIB=24
EOF
    echo '{}' >"$s/deployment.json" && echo '{}' >"$s/catalog.json"
    echo '{}' >"$s/observations/a.json" && echo '{}' >"$s/observations/b.json"
    cp "$repo/deploy/systemd/onyx-node.service" "$s/onyx-node.service"
    # A fake onyx-node: it records its arguments and settings, and fails when told to.
    cat >"$s/onyx-node" <<EOF
#!/usr/bin/env bash
{ echo "args: \$*"; env | sort; } >"$t/ran.log"
if [ -e "$t/refuse" ]; then echo "onyx-node: ONYX_BULK_PUBLISHERS: the grants exceed the node's total" >&2; exit 2; fi
if [ -e "$t/share" ]; then echo "onyx-node: request limits per client: 1200 a minute (ONYX_READS_PER_MINUTE), \$(cat "$t/share") in flight (ONYX_MAX_INFLIGHT_PER_CLIENT) of 256 for all clients" >&2; fi
echo "onyx-node: config ok"
EOF
    chmod 755 "$s/onyx-node"
  }
  with_limits() { # DIR: the export carries a limits file
    echo '{"reserve_gib":2,"publishers":[]}' >"$1/publishers.json"
    echo 'ONYX_BULK_PUBLISHERS=/etc/onyx/publishers.json' >>"$1/node.env"
  }
  run() { # STAGE [VAR=VALUE...]
    local st=$1; shift
    rm -f "$t/ran.log"
    rc=0; out=$(env ONYX_GATE_ROOT="$t/root" ONYX_GATE_SUDO= STRAY_SETTING=planted "$@" "$self" "$st" 2>&1) || rc=$?
  }

  stage "$t/plain"
  run "$t/plain"
  check "no limits on the server and none in the export: the gate passes and runs --check-config" '[ "$rc" = 0 ] && [[ $out == *"check-config: the new binary takes the new settings"* ]] && grep -qx "args: --check-config" "$t/ran.log"'
  check "the check reads the staged files, not the live /etc/onyx" 'grep -qx "ONYX_DEPLOYMENT=$t/plain/deployment.json" "$t/ran.log" && grep -qx "ONYX_CATALOG=$t/plain/catalog.json" "$t/ran.log" && grep -qx "ONYX_OBSERVATIONS=$t/plain/observations/a.json,$t/plain/observations/b.json" "$t/ran.log"'
  check "a path with no staged file keeps pointing at the live one; other values are untouched" 'grep -qx "ONYX_SIGNED_REPORTS=/etc/onyx/signed/post.log" "$t/ran.log" && grep -qx "ONYX_OBSERVATION_TEMPLATES=m.a=0x0a,m.b=0x0b" "$t/ran.log" && grep -qx "ONYX_BULK_DIR=/var/lib/onyx/bulk" "$t/ran.log"'
  check "the unit's own settings are passed, the credential path is not, and nothing of the caller's environment" 'grep -qx "ONYX_STATE_DIR=/var/lib/onyx/state" "$t/ran.log" && grep -qx "ONYX_CAS_DIR=/var/lib/onyx/cas" "$t/ran.log" && ! grep -q "^ONYX_OPERATOR_KEY_FILE=" "$t/ran.log" && ! grep -q "STRAY_SETTING\|ONYX_GATE_" "$t/ran.log"'

  stage "$t/limits" && with_limits "$t/limits"
  run "$t/limits"
  check "an export with a limits file passes, and the new binary reads the staged file" '[ "$rc" = 0 ] && grep -qx "ONYX_BULK_PUBLISHERS=$t/limits/publishers.json" "$t/ran.log"'

  # The server has limits (an earlier deploy installed them).
  echo '{"reserve_gib":2,"publishers":[]}' >"$t/root/etc/onyx/publishers.json"
  run "$t/limits"
  check "the server has limits and the export carries them: passes" '[ "$rc" = 0 ]'
  run "$t/plain"
  check "the server has limits and the export has none: refused before --check-config runs (LIM-R1 H1)" '[ "$rc" = 1 ] && [[ $out == *"would turn them off"* ]] && [[ $out == *"ONYX_API_LIMITS_OFF=1"* ]] && [[ $out == *"Nothing was changed"* ]] && [ ! -e "$t/ran.log" ]'
  check "the server's file is still there" '[ -s "$t/root/etc/onyx/publishers.json" ]'
  run "$t/plain" ONYX_API_LIMITS_OFF=0
  check "ONYX_API_LIMITS_OFF=0 is no consent" '[ "$rc" = 1 ]'
  run "$t/plain" ONYX_API_LIMITS_OFF=1
  check "ONYX_API_LIMITS_OFF=1 turns them off on purpose, with a NOTE" '[ "$rc" = 0 ] && [[ $out == *"NOTE: limits per publisher go OFF"* ]] && grep -qx "args: --check-config" "$t/ran.log"'

  stage "$t/empty" && with_limits "$t/empty" && : >"$t/empty/publishers.json"
  run "$t/empty"
  check "an empty staged limits file is an error, not 'no limits' (LIM-R1 H1)" '[ "$rc" = 1 ] && [[ $out == *"publishers.json is empty"* ]] && [ ! -e "$t/ran.log" ]'
  stage "$t/unnamed" && echo '{"reserve_gib":2,"publishers":[]}' >"$t/unnamed/publishers.json"
  run "$t/unnamed"
  check "a staged limits file the settings do not name is refused" '[ "$rc" = 1 ] && [[ $out == *"does not name it"* ]] && [ ! -e "$t/ran.log" ]'

  : >"$t/refuse"
  run "$t/limits"
  check "a binary that refuses the settings stops the deploy, with its own line (LIM-R1 M4)" '[ "$rc" = 1 ] && [[ $out == *"the grants exceed the node'"'"'s total"* ]] && [[ $out == *"The running node was not stopped and nothing was changed"* ]]'
  rm "$t/refuse"

  stage "$t/paid" && echo 00 >"$t/paid/operator.key" && chmod 644 "$t/paid/operator.key"
  run "$t/paid" ONYX_API_LIMITS_OFF=1
  check "a staged operator key is named to the check and made private" '[ "$rc" = 0 ] && grep -qx "ONYX_OPERATOR_KEY_FILE=$t/paid/operator.key" "$t/ran.log" && [ "$(stat -c %a "$t/paid/operator.key")" = 600 ]'
  sed -i '/^Environment=ONYX_OPERATOR_KEY_FILE=/d' "$t/paid/onyx-node.service"
  run "$t/paid" ONYX_API_LIMITS_OFF=1
  check "a unit without the key line gets no key path" '[ "$rc" = 0 ] && ! grep -q "^ONYX_OPERATOR_KEY_FILE=" "$t/ran.log"'

  # 3. The share of requests in flight per client decides whether the connection cap is loaded (THREATS NL5-3).
  local mark=$t/paid/node-shares-in-flight
  check "a binary that prints no in-flight share leaves no marker and is named in capitals" '[ "$rc" = 0 ] && [ ! -e "$mark" ] && [[ $out == *"WARNING: THE NEW BINARY DOES NOT LIMIT REQUESTS IN FLIGHT PER CLIENT"* ]] && [[ $out == *"will NOT be loaded"* ]]'
  echo 32 >"$t/share"
  run "$t/paid" ONYX_API_LIMITS_OFF=1
  check "a binary that prints its in-flight share leaves the marker" '[ "$rc" = 0 ] && [ -e "$mark" ] && [[ $out == *"in-flight share: the new binary limits requests in flight per client"* ]] && [[ $out != *WARNING* ]]'
  echo off >"$t/share"
  run "$t/paid" ONYX_API_LIMITS_OFF=1
  check "a share that is off is no share: an earlier marker is removed" '[ "$rc" = 0 ] && [ ! -e "$mark" ] && [[ $out == *"WARNING: THE NEW BINARY DOES NOT LIMIT"* ]]'
  echo 0 >"$t/share"
  run "$t/paid" ONYX_API_LIMITS_OFF=1
  check "a share of 0 is no share" '[ "$rc" = 0 ] && [ ! -e "$mark" ]'
  echo 32 >"$t/share" && : >"$mark" && : >"$t/refuse"
  run "$t/paid" ONYX_API_LIMITS_OFF=1
  check "a refused deploy leaves no marker" '[ "$rc" = 1 ] && [ ! -e "$mark" ]'
  rm "$t/refuse" "$t/share"

  rm "$t/plain/onyx-node"
  run "$t/plain" ONYX_API_LIMITS_OFF=1
  check "a staging folder without the binary is refused" '[ "$rc" = 1 ] && [[ $out == *"no onyx-node"* ]]'

  # deploy-api.sh itself: the gate runs on the server before anything there is changed.
  local d=$repo/scripts/deploy-api.sh first_gate first_change empty_line
  empty_line=$(awk '/publishers.json is empty/ && !n {n = NR} END {print n}' "$d")
  first_gate=$(awk '/bash deploy-node-gate.sh/ && !n {n = NR} END {print n}' "$d")
  first_change=$(awk '/^[[:space:]]*(if ! )?sudo (install|cp|rm|rsync|systemctl (stop|start|restart))/ && !n {n = NR} END {print n}' "$d")
  check "deploy-api.sh sends the gate and runs it before its first change on the server" '[[ $first_gate =~ ^[0-9]+$ ]] && [[ $first_change =~ ^[0-9]+$ ]] && [ "$first_gate" -lt "$first_change" ] && grep -q "scripts/deploy-node-gate.sh .*\$host:\$remote/" "$d"'
  check "deploy-api.sh hands the operator's consent to the gate" 'grep -q "ONYX_API_LIMITS_OFF=\${ONYX_API_LIMITS_OFF:-0} bash deploy-node-gate.sh" "$d"'
  check "deploy-api.sh refuses an empty publishers.json in the export before it sends anything" '[[ $empty_line =~ ^[0-9]+$ ]] && [ "$empty_line" -lt "$(awk "/^remote=/ && !n {n = NR} END {print n}" "$d")" ]'
  check "the node's bulk total is written once in deploy-api.sh (LIM-R1 M4)" '[ "$(grep -c "^bulk_max_gib=[0-9][0-9]*$" "$d")" = 1 ] && ! grep -q "ONYX_BULK_MAX_GIB=[0-9]" "$d" && grep -q "ONYX_BULK_MAX_GIB=\$bulk_max_gib" "$d"'
  # The cap file is loaded only inside the test of the gate's marker and of the installed binary,
  # and the limits line the gate reads is the one the node prints.
  local cap_if cap_load cap_else
  cap_if=$(awk '/if \[ -f node-shares-in-flight \] && sudo cmp -s onyx-node \/usr\/local\/bin\/onyx-node; then/ && !n {n = NR} END {print n}' "$d")
  cap_load=$(awk '/^[[:space:]]*sudo systemctl restart onyx-conn-cap$/ {n = n " " NR} END {print n}' "$d")
  cap_else=$(awk '/CONNECTION CAP NOT LOADED/ && !n {n = NR} END {print n}' "$d")
  check "deploy-api.sh loads the connection cap only for a node that has the in-flight share, and says so when it does not (THREATS NL5-3)" '[[ $cap_if =~ ^[0-9]+$ ]] && [[ $cap_else =~ ^[0-9]+$ ]] && [[ $cap_load =~ ^\ [0-9]+$ ]] && [ "$cap_if" -lt "${cap_load# }" ] && [ "${cap_load# }" -lt "$cap_else" ] && [ "$(grep -c "install -m 0644 onyx-conn-cap.nft" "$d")" = 1 ]'
  check "the line the gate reads is the line the node prints" 'grep -q "{} in flight (ONYX_MAX_INFLIGHT_PER_CLIENT)" "$repo/crates/onyx-node/src/config.rs"'
  check "the unit still runs --check-config before the node, the call the gate makes" 'grep -qx "ExecStartPre=/usr/local/bin/onyx-node --check-config" "$repo/deploy/systemd/onyx-node.service"'

  echo "deploy-node-gate self-test: $pass passed, $failed failed"
  [ "$failed" = 0 ]
}

case "${1:-}" in
--self-test) [ $# = 1 ] || die "--self-test takes no argument"; self_test ;;
"" | -*) echo "usage: $0 STAGE | --self-test" >&2; exit 2 ;;
*)
  [ $# = 1 ] || { echo "usage: $0 STAGE | --self-test" >&2; exit 2; }
  [ -d "$1" ] || die "no staging folder $1"
  gate "$(realpath "$1")"
  ;;
esac
