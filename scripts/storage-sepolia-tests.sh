#!/usr/bin/env bash
# Tests for scripts/storage-sepolia.sh (THREATS STORAGE-R3-3, STORAGE-S16-R8, STORAGE-S16-R9): a
# chain read that fails never reads as "nothing on chain yet", so no step sends a second bond,
# deposit or deploy. No network and no real key: a copy of the script runs in a temp folder (its
# own state file, keys and a .env with a made-up key) with a fake cast, forge, onyx-publish and
# onyx-storage that record every send. Each case starts from a fresh state file. The fake chain
# is a folder of small files (a node's status, bond and exit epoch; a deal's end and whether it is
# closed; an address's tONYX): the fake tools change them as the contract would, so a second run
# sees what the first one did. About 23 s; run by check-all. Checks:
#   1. register: the head read fails, answers something that is not a number, or answers a block
#      before the deploy block (a lagging endpoint); a log read fails on the first or a later
#      200-block range; the recorded deploy block is not a number; a transaction of the node's
#      key is not yet mined: it stops with the reason and registers nothing;
#   2. register: with the reads answering, each node registers once; a second run sends nothing;
#      a registration already on chain is refused; an answer without NODE_ID is shown, not
#      recorded;
#   3. deal and deal-catalog (ADR-0067): a failed head read, an unmined transaction of the payer,
#      a bad root or a deal already on chain pays nothing; with the reads answering it pays once;
#   4. deploy: a recorded earlier deploy whose code read fails, or whose first block cannot be
#      found because a read fails part way, is not adopted and nothing is deployed in its place;
#      `deploy --yes` stops with the reason when the deployer's nonce cannot be read;
#   5. fund: an address that cannot be read stops it before anything is sent;
#   6. close: a deal end that is not a number closes nothing;
#   7. the bond (ADR-0067 slice 5): `register` refuses a node that holds less tONYX than the
#      contract's bond (a node slashed on the old contract) and sends nothing; `fund --yes` tops it
#      up to a target at or above the bond, then `register` goes through; a target below the bond
#      funds nothing;
#   8. deploy records the reader contract that the constructor created (`anchor()`), and refuses an
#      answer that is no address or has no code;
#   9. the move off an old contract (ADR-0067 section 5), `exit-old`, `withdraw-old`,
#      `archive-old`: the whole path; without --yes nothing is sent; a deal still running blocks
#      the exit; too early to withdraw says how long is left and sends nothing; a second run of
#      each sends nothing; a send that fails for the third node leaves the first two done and a
#      second run finishes the rest; a contract of the newest rules is never treated as old; a
#      node with its bond in, a deal not closed or earnings left block the archive; the archive
#      keeps every entry under old_v3 and `plan` then starts fresh (it does not adopt the old
#      deploy).
set -euo pipefail
src=$(cd "$(dirname "$0")" && pwd)/storage-sepolia.sh
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
mkdir -p "$t/repo/scripts" "$t/repo/contracts" "$t/repo/.onyx-testnet/keys" "$t/bin" "$t/foundry" "$t/calls" "$t/ds" "$t/chain"
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
  if [ -n "$b" ] && [ "$b" -lt "${FAKE_CODE_FROM:-0}" ]; then echo 0x; elif [ "$2" = "${FAKE_NOCODE_AT:-none}" ]; then echo 0x; else echo 0x6080; fi ;;
nonce)
  [ "${FAKE_NONCE_FAIL:-0}" = 0 ] || { echo "Error: HTTP request failed: Connection reset"; exit 1; }
  if [ "${3:-}" = --block ] && [ "${4:-}" = pending ]; then echo "${FAKE_PENDING:-${FAKE_NONCE:-3}}"; else echo "${FAKE_NONCE:-3}"; fi ;;
compute-address) echo "Computed Address: 0x000000000000000000000000000000000000c0de" ;;
call)
  [ "${FAKE_CALL_FAIL:-none}" != "${3%%(*}" ] || { echo "Error: HTTP request failed: Connection reset"; exit 1; }
  case "$3" in
  'currentEpoch()(uint64)') echo "${FAKE_EPOCH:-20}" ;;
  'SETTLE_EPOCHS()(uint64)') echo 2 ;;
  'RULES_VERSION()(uint8)') echo "${FAKE_RULES:-4}" ;;
  'anchor()(address)') echo "${FAKE_ANCHOR:-0x00000000000000000000000000000000000A9C02}" ;;
  # epochs of 7,200 blocks from block 100: epoch 20 began at block 144,100.
  params*) printf '%s\n' 7200 "1000000000000000 [1e15]" "10000000000000000 [1e16]" "10000000000000000000 [1e19]" 100000 2 3 100 ;;
  'bondFor(uint64)(uint256)') echo "${FAKE_BOND:-10000000000000000000} [1e19]" ;;
  'balanceOf(address)(uint256)')
    # An address's tONYX: its file, or 20 tONYX.
    if [ -f "$FAKE_CHAIN/tonyx_$4" ]; then echo "$(cat "$FAKE_CHAIN/tonyx_$4") [x]"; else echo "20000000000000000000 [2e19]"; fi ;;
  'dealCount()(uint256)') find "$FAKE_CHAIN" -name 'deal_*' | wc -l ;;
  dealInfo*)
    # A deal's file holds "END CLOSED"; without one: the deal of the older cases.
    end=${FAKE_END:-10} closed=false
    [ ! -f "$FAKE_CHAIN/deal_$4" ] || read -r end closed <"$FAKE_CHAIN/deal_$4"
    printf '%s\n' 0x01 0x02 0x03 0x04 1 3 100 2 "$end" 5 4 "$closed" ;;
  nodeInfo*)
    # A node's file holds "STATUS BOND REMOVED_FROM"; no file: an unknown id (status 0).
    st=0 bond=0 from=0
    [ ! -f "$FAKE_CHAIN/node_$4" ] || read -r st bond from <"$FAKE_CHAIN/node_$4"
    printf '%s\n' 0x0000000000000000000000000000000000000001 "$st" 6 "$bond [x]" 0 "$from" 0 0 ;;
  'earned(address)(uint256)') echo "${FAKE_EARNED:-0}" ;;
  *) echo true ;;
  esac ;;
codesize) echo 23098 ;;
balance) echo 0.0001 ;;
*) echo "fake cast: unexpected $1" >&2; exit 3 ;;
esac
EOF
cat >"$t/foundry/forge" <<'EOF'
#!/usr/bin/env bash
echo "forge $*" >>"$FAKE_CALLS/sent.log"
# FundStorage tops each address's tONYX up to the target (never above, never down), as
# contracts/script/FundStorage.s.sol does; the target it was given is kept for the test.
case "$*" in *FundStorage*)
  echo "$ONYX_FUND_TONYX $ONYX_FUND_TO" >>"$FAKE_CALLS/fund.log"
  for a in ${ONYX_FUND_TO//,/ }; do
    have=20000000000000000000
    [ ! -f "$FAKE_CHAIN/tonyx_$a" ] || have=$(cat "$FAKE_CHAIN/tonyx_$a")
    python3 -I -c 'import sys; print(max(int(sys.argv[1]), int(sys.argv[2])))' "$have" "$ONYX_FUND_TONYX" >"$FAKE_CHAIN/tonyx_$a"
  done ;;
esac
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
cmd=$1
case "$1" in
register)
  [ "${FAKE_NO_ID:-0}" = 0 ] || { echo "sent, then the activation answer was lost"; exit 0; }
  printf 'NODE_ID=0x%064d\n' "$(wc -l <"$FAKE_CALLS/sent.log")" ;;
deal) echo DEAL_ID=7 ;;
close | withdraw) echo done ;;
exit | withdraw-bond)
  # ... --node ID: the contract's rules for the node's file ("STATUS BOND REMOVED_FROM").
  id=
  while [ $# -gt 0 ]; do [ "$1" != --node ] || id=$2; shift; done
  [ "${FAKE_SEND_FAIL:-none}" != "$id" ] || { echo "onyx-storage: the transaction was not mined"; exit 1; }
  read -r st bond from <"$FAKE_CHAIN/node_$id"
  now=${FAKE_EPOCH:-20}
  case "$cmd" in
  exit)
    [ "$st" = 1 ] || { echo "onyx-storage: reverted: NotActive"; exit 1; }
    [ "${FAKE_LAG:-0}" != 0 ] || echo "2 $bond $((now + 1))" >"$FAKE_CHAIN/node_$id"
    echo "EXITING tx=0xe1" ;;
  *)
    { [ "$st" = 2 ] || [ "$st" = 3 ]; } || { echo "onyx-storage: reverted: NotActive"; exit 1; }
    [ "$now" -gt $((from + 2)) ] || { echo "onyx-storage: reverted: TooEarly"; exit 1; }
    echo "4 0 $from" >"$FAKE_CHAIN/node_$id"
    echo "BOND_WITHDRAWN tx=0xb0" ;;
  esac ;;
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
  : >"$t/calls/fund.log"
  set +e
  out=$(cd "$t" && env -u BASE_SEPOLIA_RPC_URL -u ETH_RPC_URL -u FOUNDRY_ETH_RPC_URL FAKE_CALLS="$t/calls" FAKE_CHAIN="$t/chain" \
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
fresh() {
  rm -f "$t/chain"/*
  printf '{"contract": "0x%040d", "block": "990"}\n' 9 >"$state"
}

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

# 3b. deal-catalog (ADR-0067): the same guards for a manifest that a catalog anchors.
root=0x$(printf 'ab%.0s' $(seq 32))
fresh
run "deal-catalog needs --yes" 2 "deal-catalog --yes ROOT" deal-catalog "$root"
nothing_sent "deal-catalog without --yes"
fresh
run "deal-catalog refuses what is no manifest root" 2 "a manifest root is 0x and 64 lowercase hex digits" deal-catalog --yes 0xABC
nothing_sent "deal-catalog with a bad root"
fresh
FAKE_PENDING=5 run "deal-catalog waits for an unmined transaction of the payer" 1 "not yet mined" deal-catalog --yes "$root"
nothing_sent "deal-catalog over an unmined transaction"
fresh
FAKE_LOGS='[{"data": "0x'"${root#0x}"'"}]' run "a catalog deal already on chain is not paid again" 1 "already has a deal for this manifest" deal-catalog --yes "$root"
nothing_sent "deal-catalog over a deal on chain"
fresh
run "deal-catalog pays once with answering reads" 0 "DEAL_ID=7" deal-catalog --yes "$root"
[ "$(sent)" = 1 ] && ok "  ... one deal" || bad "deal-catalog sent $(sent), not 1"
run "a second deal-catalog run pays nothing" 0 "already has deal 7" deal-catalog --yes "$root"
nothing_sent "a second deal-catalog run"

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

# 7. the bond: a node that holds less tONYX than the bond is not registered; fund covers the gap.
fresh
echo 7938000000000000000 >"$t/chain/tonyx_0x0000000000000000000000000000000000000001"
run "register refuses a node short of the bond" 1 "holds 7938000000000000000 tONYX units, the bond is 10000000000000000000: run fund --yes first" register
nothing_sent "register with a node short of the bond"
FAKE_BOND=30000000000000000000 run "fund refuses a target below the bond" 1 "below the bond 30000000000000000000" fund --yes
nothing_sent "fund with a target below the bond"
run "fund tops the slashed node up" 0 "n1 0x0000000000000000000000000000000000000001 eth=0.0001 tONYX=20000000000000000000" fund --yes
[ "$(sent)" = 2 ] && ok "  ... two funding runs (the nodes, the payer)" || bad "fund sent $(sent), not 2"
[ "$(sed -n 1p "$t/calls/fund.log")" = "20000000000000000000 0x0000000000000000000000000000000000000001,0x0000000000000000000000000000000000000002,0x0000000000000000000000000000000000000003" ] \
  && ok "  ... each node to 20 tONYX, at or above the bond of 10" || bad "the nodes' funding target: $(sed -n 1p "$t/calls/fund.log")"
run "register goes through after the top-up" 0 "n3 NODE_ID=0x" register
[ "$(sent)" = 3 ] && ok "  ... three registrations" || bad "register after fund sent $(sent), not 3"
fresh
FAKE_CALL_FAIL=bondFor run "register stops when the bond cannot be read" 1 "cast call failed" register
nothing_sent "register after a failed bond read"

# 8. deploy records the reader contract that the constructor created.
has() { python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1])); [d := d.get(k, {}) if isinstance(d, dict) else {} for k in sys.argv[2].split("/")]; sys.exit(0 if d == sys.argv[3] else 1)' "$state" "$1" "$2"; }
lacks() { python3 -I -c 'import json,sys; sys.exit(1 if sys.argv[2] in json.load(open(sys.argv[1])) else 0)' "$state" "$1"; }
reader=0x00000000000000000000000000000000000a9c02
rm -f "$t/chain"/*
printf '{}\n' >"$state"
run "deploy --yes records the reader contract" 0 "OnyxStorageAnchor $reader (23098 bytes)" deploy --yes
has contract 0x000000000000000000000000000000000000c0de && has anchor "$reader" && ok "  ... both addresses recorded" || bad "deploy did not record the contract and its reader"
[ "$(sent)" = 1 ] && ok "  ... one deploy for both" || bad "deploy sent $(sent), not 1"
run "a second deploy sends nothing" 1 "already deployed: 0x000000000000000000000000000000000000c0de" deploy --yes
nothing_sent "a second deploy"
printf '{}\n' >"$state"
FAKE_ANCHOR=0x0000000000000000000000000000000000000000 run "deploy refuses a reader that is the zero address" 1 "not an address" deploy --yes
lacks anchor && ok "  ... no reader recorded" || bad "a zero reader was recorded"
run "a later run records the reader it could not read before" 1 "OnyxStorageAnchor $reader" plan
has anchor "$reader" && ok "  ... recorded" || bad "the reader was not recorded on the later run"
nothing_sent "recording the reader later"
printf '{}\n' >"$state"
FAKE_NOCODE_AT=$reader run "deploy refuses a reader without code" 1 "no code at the reader $reader" deploy --yes
lacks anchor && ok "  ... no reader recorded" || bad "a reader without code was recorded"

# 9. the move off an old contract: exit-old, withdraw-old, archive-old.
id() { printf '0x%064d' "$1"; }
node() { cut -d' ' -f1 "$t/chain/node_$(id "$1")"; }
nodes_are() { [ "$(node 1)$(node 2)$(node 3)" = "$1" ] && ok "  ... node statuses $1" || bad "$2: node statuses $(node 1)$(node 2)$(node 3), not $1"; }
# An old contract (rules 3) as on the test network: three nodes, active, one with its whole bond and
# two slashed; two deals that ended at epoch 10 and are closed; the current epoch is 20.
old() {
  rm -f "$t/chain"/*
  printf '{"contract": "0x%040d", "block": "990", "epoch_blocks": "7200", "pending_contract": "0x%040d", "pending_block": "980", "deal_a.test": "0", "deal_b.test": "1", "node_n1": "%s", "node_n2": "%s", "node_n3": "%s"}\n' \
    3 3 "$(id 1)" "$(id 2)" "$(id 3)" >"$state"
  echo "1 9800000000000000000 0" >"$t/chain/node_$(id 1)"
  echo "1 8820000000000000000 0" >"$t/chain/node_$(id 2)"
  echo "1 7938000000000000000 0" >"$t/chain/node_$(id 3)"
  echo "10 true" >"$t/chain/deal_0"
  echo "10 true" >"$t/chain/deal_1"
}
export FAKE_RULES=3

# 9a. exit-old.
old
run "exit-old without --yes says what it would send" 2 "exit-old would send one requestExit from each of: n1 n2 n3. Nothing sent: exit-old --yes" exit-old
nothing_sent "exit-old without --yes"
nodes_are 111 "exit-old without --yes"
echo "30 false" >"$t/chain/deal_1"
run "a deal still running blocks the exit" 1 "still running on 0x0000000000000000000000000000000000000003: deal 1 (until epoch 30)" exit-old --yes
nothing_sent "exit-old over a running deal"
nodes_are 111 "exit-old over a running deal"
old
FAKE_RULES=4 run "exit-old never treats the newest contract as old" 1 "follows rules 4, above 3: it is not an old contract" exit-old --yes
nothing_sent "exit-old on a rules-4 contract"
FAKE_CALL_FAIL=RULES_VERSION run "exit-old stops when the rules cannot be read" 1 "cast call failed" exit-old --yes
nothing_sent "exit-old after a failed rules read"
FAKE_CALL_FAIL=nodeInfo run "exit-old stops when a node cannot be read" 1 "cast call failed" exit-old --yes
nothing_sent "exit-old after a failed node read"
FAKE_CALL_FAIL=dealCount run "exit-old stops when the deals cannot be read" 1 "cast call failed" exit-old --yes
nothing_sent "exit-old after a failed deal read"
FAKE_PENDING=4 run "exit-old waits for an unmined transaction of a node" 1 "not yet mined" exit-old --yes
nothing_sent "exit-old over an unmined transaction"
rm "$t/chain/node_$(id 2)"
run "exit-old refuses a recorded id that is no node of the contract" 1 "is no node of 0x0000000000000000000000000000000000000003 (status 0)" exit-old --yes
nothing_sent "exit-old with an unknown node id"
old
FAKE_LAG=1 run "an exit the endpoint does not show yet is not recorded" 1 "still reads the node as active" exit-old --yes
lacks old_exit_n1 && ok "  ... not recorded" || bad "an exit was recorded on a lagging read"
old
FAKE_SEND_FAIL=$(id 2) run "an exit that fails for the second node stops there" 1 "the transaction was not mined" exit-old --yes
nodes_are 211 "exit-old stopped at the second node"
has old_exit_n1 21 && ok "  ... the first node's exit is recorded (off the list from epoch 21)" || bad "the first exit was not recorded"
run "a second exit-old finishes the rest" 0 "n1: already exiting (off the list from epoch 21; bond free from epoch 24)" exit-old --yes
[ "$(sent)" = 2 ] && ok "  ... two exits, none for the first node" || bad "the second exit-old sent $(sent), not 2"
nodes_are 222 "the second exit-old"
old
echo "10 false" >"$t/chain/deal_1"
run "exit-old sends every exit (a deal past its end does not block it)" 0 "exit-old: done. Next: withdraw-old --yes, 3 to 4 epochs from now" exit-old --yes
[ "$(sent)" = 3 ] && ok "  ... three exits" || bad "exit-old sent $(sent), not 3"
nodes_are 222 "exit-old"
has old_exit_n3 21 && ok "  ... recorded" || bad "exit-old did not record the exits"
run "a second exit-old sends nothing" 0 "exit-old: nothing to send" exit-old --yes
nothing_sent "a second exit-old"

# 9b. withdraw-old (the nodes left in epoch 20: off the list from 21, bonds free from epoch 24).
FAKE_HEAD=147700 run "withdraw-old too early says how long is left" 1 "n1: too early: its bond is free from epoch 24 (now 20): 4 epoch(s), 25200 blocks, about 84 minute(s) at 0.2 s a block" withdraw-old --yes
nothing_sent "withdraw-old in the exit epoch"
FAKE_EPOCH=23 FAKE_HEAD=172899 run "withdraw-old is too early one block before the bond is free" 1 "1 epoch(s), 1 blocks, about 1 minute(s)" withdraw-old --yes
nothing_sent "withdraw-old one block early"
export FAKE_EPOCH=24 FAKE_HEAD=172900
run "withdraw-old without --yes says what it would send" 2 "withdraw-old would send one withdrawBond from each of: n1 n2 n3. Nothing sent: withdraw-old --yes" withdraw-old
nothing_sent "withdraw-old without --yes"
FAKE_RULES=4 run "withdraw-old never treats the newest contract as old" 1 "it is not an old contract" withdraw-old --yes
nothing_sent "withdraw-old on a rules-4 contract"
FAKE_CALL_FAIL=params run "withdraw-old stops when the epoch length cannot be read" 1 "cast call failed" withdraw-old --yes
nothing_sent "withdraw-old after a failed read"
FAKE_PENDING=4 run "withdraw-old waits for an unmined transaction of a node" 1 "not yet mined" withdraw-old --yes
nothing_sent "withdraw-old over an unmined transaction"
FAKE_SEND_FAIL=$(id 3) run "a withdrawal that fails for the third node stops there" 1 "the transaction was not mined" withdraw-old --yes
nodes_are 442 "withdraw-old stopped at the third node"
has old_bond_out_n2 8820000000000000000 && lacks old_bond_out_n3 && ok "  ... two bonds recorded as withdrawn" || bad "the record after a failed withdrawal is wrong"
run "a second withdraw-old finishes the rest" 0 "n3: withdrew its bond of 7938000000000000000" withdraw-old --yes
[ "$(sent)" = 1 ] && ok "  ... one withdrawal, none for the first two" || bad "the second withdraw-old sent $(sent), not 1"
nodes_are 444 "the second withdraw-old"
run "a third withdraw-old sends nothing and prints the books" 0 "conserved: true; the old contract still holds 20000000000000000000 tONYX units" withdraw-old --yes
nothing_sent "a third withdraw-old"
old
run "withdraw-old refuses while a node is still active" 1 "n2: still active: run exit-old --yes first" withdraw-old --yes
nothing_sent "withdraw-old before exit-old"

# 9c. archive-old: every bond out, every deal closed, no earnings left; else nothing changes.
run "archive-old refuses while a node has its bond in" 1 "n1 still has its bond in 0x0000000000000000000000000000000000000003 (status 1)" archive-old
has contract 0x0000000000000000000000000000000000000003 && lacks old_v3 && ok "  ... the state file is unchanged" || bad "archive-old changed the state file with bonds in"
for i in 1 2 3; do echo "4 0 21" >"$t/chain/node_$(id "$i")"; done
echo "10 false" >"$t/chain/deal_1"
run "archive-old refuses while a deal is not closed" 1 "deal 1 is not closed (ended, end epoch 10): run close" archive-old
lacks old_v3 && ok "  ... the state file is unchanged" || bad "archive-old changed the state file over an open deal"
echo "10 true" >"$t/chain/deal_1"
FAKE_EARNED=5 run "archive-old refuses while a node has earnings left" 1 "n1 still has earnings of 5" archive-old
lacks old_v3 && ok "  ... the state file is unchanged" || bad "archive-old changed the state file over earnings"
FAKE_RULES=4 run "archive-old never archives the newest contract" 1 "it is not an old contract" archive-old
lacks old_v3 && ok "  ... the state file is unchanged" || bad "archive-old archived a rules-4 contract"
FAKE_CALL_FAIL=earned run "archive-old stops when a read fails" 1 "cast call failed" archive-old
lacks old_v3 && ok "  ... the state file is unchanged" || bad "archive-old changed the state file after a failed read"
run "archive-old moves every entry under old_v3" 0 "archived 10 entries of 0x0000000000000000000000000000000000000003 under old_v3" archive-old
nothing_sent "archive-old"
lacks contract && lacks block && lacks pending_contract && lacks node_n1 && lacks deal_a.test \
  && ok "  ... nothing of the old contract is left at the top" || bad "archive-old left old entries at the top"
has old_v3/contract 0x0000000000000000000000000000000000000003 && has old_v3/node_n3 "$(id 3)" && has old_v3/deal_b.test 1 \
  && has old_v3/pending_block 980 && has old_v3/block 990 \
  && ok "  ... and every one of them is kept under old_v3" || bad "archive-old lost an entry"
run "a second archive-old changes nothing" 0 "archive-old: the old contract is already archived (old_v3): nothing to do" archive-old
run "exit-old after the archive sends nothing" 0 "already archived (old_v3): nothing to do" exit-old --yes
nothing_sent "exit-old after the archive"
run "withdraw-old after the archive sends nothing" 0 "already archived (old_v3): nothing to do" withdraw-old --yes
nothing_sent "withdraw-old after the archive"

# 9d. after the archive the deploy starts fresh, and the old entries stay.
unset FAKE_EPOCH FAKE_HEAD
export FAKE_RULES=4
out=$(cd "$t" && env -u BASE_SEPOLIA_RPC_URL -u ETH_RPC_URL -u FOUNDRY_ETH_RPC_URL FAKE_CALLS="$t/calls" FAKE_CHAIN="$t/chain" \
  FOUNDRY_BIN="$t/foundry" ONYX_STORAGE_BIN_DIR="$t/bin" STORAGE_NODES=3 "$t/repo/scripts/storage-sepolia.sh" plan 2>&1) || true
grep -qF "found the earlier deploy" <<<"$out" && bad "plan adopted the old contract after the archive" || ok "plan after the archive does not adopt the old deploy"
run "plan after the archive is a dry run of a new contract" 0 "PLAN ONLY: nothing sent" plan
lacks contract && ok "  ... nothing recorded" || bad "plan recorded a contract"
run "deploy --yes after the archive deploys the new contract" 0 "OnyxStorage 0x000000000000000000000000000000000000c0de at block 1000" deploy --yes
has contract 0x000000000000000000000000000000000000c0de && has anchor "$reader" && has old_v3/contract 0x0000000000000000000000000000000000000003 \
  && ok "  ... the new contract and its reader are recorded, old_v3 is kept" || bad "the state after the new deploy is wrong"
run "exit-old on the new contract is refused" 1 "it is not an old contract" exit-old --yes
nothing_sent "exit-old on the new contract"
printf '{"contract": "0x%040d", "block": "990", "old_v3": {"contract": "0x01"}}\n' 3 >"$state"
for i in 1 2 3; do rm -f "$t/chain/node_$(id "$i")"; done
FAKE_RULES=3 run "archive-old refuses to overwrite an earlier archive" 1 "already has old_v3" archive-old
has old_v3/contract 0x01 && has contract 0x0000000000000000000000000000000000000003 && ok "  ... the state file is unchanged" || bad "archive-old overwrote an earlier archive"
printf '{}\n' >"$state"
run "exit-old with nothing recorded says so" 1 "no contract is recorded: nothing to move" exit-old --yes
nothing_sent "exit-old with an empty state file"

if [ "$fails" != 0 ]; then
  echo "STORAGE-SEPOLIA TESTS FAILED ($fails)" >&2
  exit 1
fi
echo "STORAGE-SEPOLIA TESTS PASSED"
