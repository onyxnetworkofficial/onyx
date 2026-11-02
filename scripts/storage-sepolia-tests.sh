#!/usr/bin/env bash
# Tests for scripts/storage-sepolia.sh (THREATS STORAGE-R3-3, STORAGE-S16-R8, STORAGE-S16-R9): a
# chain read that fails never reads as "nothing on chain yet", so no step sends a second bond,
# deposit or deploy. No network and no real key: a copy of the script runs in a temp folder (its
# own state file, keys and a .env with a made-up key) with a fake cast, forge, onyx-publish and
# onyx-storage that record every send. Each case starts from a fresh state file. About 6 s; run
# by check-all. Checks:
#   1. register: the head read fails, answers something that is not a number, or answers a block
#      before the deploy block (a lagging endpoint); a log read fails on the first or a later
#      200-block range; the recorded deploy block is not a number; a transaction of the node's
#      key is not yet mined: it stops with the reason and registers nothing;
#   2. register: with the reads answering, each node registers once; a second run sends nothing;
#      a registration already on chain is refused; an answer without NODE_ID is shown, not
#      recorded;
#   3. deal: a failed head read or a transaction of the payer not yet mined pays nothing; with
#      the reads answering it pays once;
#   4. deploy: a recorded earlier deploy whose code read fails, or whose first block cannot be
#      found because a read fails part way, is not adopted and nothing is deployed in its place;
#      `deploy --yes` stops with the reason when the deployer's nonce cannot be read;
#   5. fund: an address that cannot be read stops it before anything is sent;
#   6. close: a deal end that is not a number closes nothing.
set -euo pipefail
src=$(cd "$(dirname "$0")" && pwd)/storage-sepolia.sh
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
mkdir -p "$t/repo/scripts" "$t/repo/contracts" "$t/repo/.onyx-testnet/keys" "$t/bin" "$t/foundry" "$t/calls" "$t/ds"
cp "$src" "$t/repo/scripts/"
for n in n1 n2 n3 payer; do : >"$t/repo/.onyx-testnet/keys/storage-$n.key"; done
printf '{"dataset": "x.test", "root": "0x%064d"}\n' 1 >"$t/ds/manifest.json"
# A made-up deployer key (not a real one), so the steps that need one go as far as the fake forge.
printf 'BASE_SEPOLIA_PRIVATE_KEY=0x%064d\n' 7 >"$t/repo/.env"

cat >"$t/foundry/cast" <<'EOF'
#!/usr/bin/env bash
case "$1" in
chain-id) echo 84532 ;;
block-number)
  [ "${FAKE_HEAD_FAIL:-0}" = 0 ] || { echo "Error: HTTP request failed: Connection timed out (os error 110)"; exit 1; }
  echo "${FAKE_HEAD:-1000}" ;;
logs)
  # logs --json --from-block F --to-block E ... (F is $4)
  [ "${FAKE_LOGS_FAIL:-0}" = 0 ] || { echo "Error: HTTP 429 Too Many Requests"; exit 1; }
  [ "$4" -lt "${FAKE_LOGS_FAIL_FROM:-999999999}" ] || { echo "Error: HTTP 429 Too Many Requests"; exit 1; }
  echo "$4" >>"$FAKE_CALLS/ranges.log"
  echo "${FAKE_LOGS:-[]}" ;;
code)
  b=""
  [ "${3:-}" = --block ] && b=$4
  if [ -n "$b" ] && [ "$b" -ge "${FAKE_CODE_FAIL_AT:-999999999}" ]; then echo "Error: timed out"; exit 1; fi
  [ "${FAKE_CODE_FAIL:-0}" = 0 ] || { echo "Error: timed out"; exit 1; }
  if [ -n "$b" ] && [ "$b" -lt "${FAKE_CODE_FROM:-0}" ]; then echo 0x; else echo 0x6080; fi ;;
nonce)
  [ "${FAKE_NONCE_FAIL:-0}" = 0 ] || { echo "Error: HTTP request failed: Connection reset"; exit 1; }
  if [ "${3:-}" = --block ] && [ "${4:-}" = pending ]; then echo "${FAKE_PENDING:-${FAKE_NONCE:-3}}"; else echo "${FAKE_NONCE:-3}"; fi ;;
compute-address) echo "Computed Address: 0x000000000000000000000000000000000000c0de" ;;
call)
  case "$3" in
  'currentEpoch()(uint64)') echo 20 ;;
  dealInfo*) printf '%s\n' 0x01 0x02 0x03 0x04 1 3 100 2 "${FAKE_END:-10}" 5 4 false ;;
  'earned(address)(uint256)') echo 0 ;;
  *) echo true ;;
  esac ;;
balance) echo 0.0001 ;;
*) echo "fake cast: unexpected $1" >&2; exit 3 ;;
esac
EOF
cat >"$t/foundry/forge" <<'EOF'
#!/usr/bin/env bash
echo "forge $*" >>"$FAKE_CALLS/sent.log"
EOF
cat >"$t/bin/onyx-publish" <<'EOF'
#!/usr/bin/env bash
# address --key .../storage-<name>.key: a fixed address per name.
case "$3" in
*-"${FAKE_ADDR_FAIL:-none}".key) echo "fake onyx-publish: cannot read the key" >&2; exit 1 ;;
*-n1.key) echo 0x0000000000000000000000000000000000000001 ;;
*-n2.key) echo 0x0000000000000000000000000000000000000002 ;;
*-n3.key) echo 0x0000000000000000000000000000000000000003 ;;
*) echo 0x00000000000000000000000000000000000000aa ;;
esac
EOF
cat >"$t/bin/onyx-storage" <<'EOF'
#!/usr/bin/env bash
echo "onyx-storage $1" >>"$FAKE_CALLS/sent.log"
case "$1" in
register)
  [ "${FAKE_NO_ID:-0}" = 0 ] || { echo "sent, then the activation answer was lost"; exit 0; }
  printf 'NODE_ID=0x%064d\n' "$(wc -l <"$FAKE_CALLS/sent.log")" ;;
deal) echo DEAL_ID=7 ;;
close | withdraw) echo done ;;
*) echo "fake onyx-storage: unexpected $1" >&2; exit 3 ;;
esac
EOF
chmod +x "$t/foundry/cast" "$t/foundry/forge" "$t/bin/onyx-publish" "$t/bin/onyx-storage"

state=$t/repo/.onyx-testnet/storage-sepolia.json
fails=0
ok() { echo "ok: $1"; }
bad() { echo "FAIL: $1" >&2; fails=$((fails + 1)); }
# run NAME EXPECT(0|nonzero) PATTERN ARGS...: runs the copy, checks its exit and that its output
# holds PATTERN.
run() {
  local name=$1 want=$2 pat=$3 out rc
  shift 3
  : >"$t/calls/sent.log"
  : >"$t/calls/ranges.log"
  set +e
  out=$(cd "$t" && env -u BASE_SEPOLIA_RPC_URL -u ETH_RPC_URL -u FOUNDRY_ETH_RPC_URL FAKE_CALLS="$t/calls" \
    FOUNDRY_BIN="$t/foundry" ONYX_STORAGE_BIN_DIR="$t/bin" STORAGE_NODES=3 \
    "$t/repo/scripts/storage-sepolia.sh" "$@" 2>&1)
  rc=$?
  set -e
  if { [ "$want" = 0 ] && [ "$rc" != 0 ]; } || { [ "$want" != 0 ] && [ "$rc" = 0 ]; }; then
    bad "$name: exit $rc"
    printf '%s\n' "$out" | tail -5 >&2
    return
  fi
  if ! grep -qF -- "$pat" <<<"$out"; then
    bad "$name: no \"$pat\" in the output"
    printf '%s\n' "$out" | tail -5 >&2
    return
  fi
  ok "$name"
}
sent() { wc -l <"$t/calls/sent.log" | tr -d ' '; }
nothing_sent() { [ "$(sent)" = 0 ] && ok "  ... nothing sent" || bad "$1: $(sent) send(s)"; }
fresh() { printf '{"contract": "0x%040d", "block": "990"}\n' 9 >"$state"; }

# 1. register: failed reads and unmined transactions send nothing.
fresh
FAKE_HEAD_FAIL=1 run "register stops when the head read fails" 1 "eth_blockNumber failed" register
nothing_sent "register after a failed head read"
fresh
FAKE_HEAD=oops run "register stops when the head is not a number" 1 "not a whole number" register
nothing_sent "register after a bad head"
fresh
FAKE_HEAD=500 run "register stops when the head is before the deploy block" 1 "before the deploy block" register
nothing_sent "register with a lagging head"
fresh
FAKE_LOGS_FAIL=1 run "register stops when a log read fails" 1 "eth_getLogs failed" register
nothing_sent "register after a failed log read"
fresh
FAKE_HEAD=1500 FAKE_LOGS_FAIL_FROM=1190 run "register stops when a later 200-block range fails" 1 "eth_getLogs failed for blocks 1190..1389" register
nothing_sent "register after a later failed range"
[ "$(cat "$t/calls/ranges.log")" = 990 ] && ok "  ... after reading the first range" || bad "ranges read: $(paste -sd, "$t/calls/ranges.log")"
printf '{"contract": "0x%040d", "block": "soon"}\n' 9 >"$state"
run "register stops when the recorded deploy block is not a number" 1 "not a whole number" register
nothing_sent "register with a bad deploy block"
fresh
FAKE_PENDING=4 run "register waits for an unmined transaction of the node" 1 "not yet mined" register
nothing_sent "register over an unmined transaction"
fresh
FAKE_NONCE_FAIL=1 run "register stops when the nonce read fails" 1 "cast nonce failed" register
nothing_sent "register after a failed nonce read"

# 2. register: answering reads register each node once.
fresh
run "register registers each node" 0 "n3 NODE_ID=0x" register
[ "$(sent)" = 3 ] && ok "  ... three registrations" || bad "register sent $(sent), not 3"
run "a second register sends nothing" 0 "n1 already registered" register
nothing_sent "a second register"
fresh
FAKE_LOGS='[{"data": "0x"}]' run "a registration already on chain is refused" 1 "already has 1 registration(s) on chain" register
nothing_sent "register over one on chain"
fresh
FAKE_NO_ID=1 run "an answer without NODE_ID is shown and not recorded" 1 "the activation answer was lost" register
grep -q node_n1 "$state" && bad "a node id was recorded without NODE_ID" || ok "  ... not recorded"

# 3. deal.
fresh
FAKE_HEAD_FAIL=1 run "deal stops when the head read fails" 1 "eth_blockNumber failed" deal "$t/ds"
nothing_sent "deal after a failed head read"
fresh
FAKE_PENDING=5 run "deal waits for an unmined transaction of the payer" 1 "not yet mined" deal "$t/ds"
nothing_sent "deal over an unmined transaction"
fresh
run "deal pays once with answering reads" 0 "DEAL_ID=7" deal "$t/ds"
[ "$(sent)" = 1 ] && ok "  ... one deal" || bad "deal sent $(sent), not 1"
run "a second deal run pays nothing" 0 "already has deal 7" deal "$t/ds"
nothing_sent "a second deal run"

# 4. deploy: an earlier deploy is adopted only on reads that answered; nothing deployed instead.
printf '{"pending_contract": "0x%040d", "pending_block": "100"}\n' 5 >"$state"
FAKE_CODE_FAIL=1 run "a failed code read does not adopt the earlier deploy" 1 "eth_getCode failed" plan
grep -q '"contract"' "$state" && bad "the earlier deploy was recorded on a failed read" || ok "  ... not recorded"
nothing_sent "plan after a failed code read"
FAKE_CODE_FROM=103 FAKE_CODE_FAIL_AT=102 run "a code read failing part way does not adopt it" 1 "eth_getCode failed" plan
grep -q '"contract"' "$state" && bad "the earlier deploy was recorded with a guessed block" || ok "  ... not recorded"
nothing_sent "plan after a code read failed part way"
FAKE_CODE_FROM=103 run "the earlier deploy is adopted at its first block" 0 "found the earlier deploy" plan
python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1])); sys.exit(0 if d.get("block")=="103" else 1)' "$state" \
  && ok "  ... at block 103" || bad "the adopted deploy block is not 103"
nothing_sent "adopting the earlier deploy"
printf '{}\n' >"$state"
FAKE_NONCE_FAIL=1 run "deploy --yes stops when the deployer's nonce read fails" 1 "cast nonce failed" deploy --yes
nothing_sent "deploy after a failed nonce read"

# 5. fund: every address is read before anything is sent.
fresh
FAKE_ADDR_FAIL=n1 run "fund stops when an address cannot be read" 1 "cannot read the key" fund --yes
nothing_sent "fund with an unreadable address"

# 6. close: a deal end that is not a number closes nothing.
fresh
printf '{"contract": "0x%040d", "block": "990", "deal_x.test": "7"}\n' 9 >"$state"
FAKE_END=oops run "close stops when a deal's end is not a number" 1 "not a whole number" close
nothing_sent "close with a bad end epoch"
run "close closes an ended deal" 0 "(deal 7): closed" close
[ "$(sent)" = 1 ] && ok "  ... one close" || bad "close sent $(sent), not 1"

if [ "$fails" != 0 ]; then
  echo "STORAGE-SEPOLIA TESTS FAILED ($fails)" >&2
  exit 1
fi
echo "STORAGE-SEPOLIA TESTS PASSED"
