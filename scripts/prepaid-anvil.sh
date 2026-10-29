#!/usr/bin/env bash
# Prepaid balances end to end on a local anvil (pre-public loop PP10 part 2, ROADMAP S7,
# ADR-0046 amendment 2). Deploys a mock USDC, OnyxRevenue (two registered observation books) and
# OnyxPrepaid; a buyer deposits 5,000; `examples/prepaid-rehearsal.rs` takes three vouchers of
# 1,000 against the buyer's finalized account (books A, A, B), then one reporter pass redeems the
# latest voucher and pays the books, and a second pass does nothing. The chain is then checked:
# the buyer's redeemed total, the empty pool, each book's revenue, and the buyer still able to
# spend the rest. THREATS PRE-10: the second pass pays nothing twice.
# Local only: chain id 31337. The buyer and reporter keys are made fresh for each run and thrown
# away (they hold anvil gas and mock tokens only); the deployer is anvil's unlocked dev account.
# cast runs in a temp dir outside the repo (Foundry loads a `.env` from its working directory).
# Env: FOUNDRY_BIN (~/.foundry/bin).
set -euo pipefail
cd "$(dirname "$0")/.."
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
repo=$PWD
work=$(mktemp -d)
anvil_pid=""
cleanup() {
  if [ -n "$anvil_pid" ]; then
    kill "$anvil_pid" 2>/dev/null || true
    wait "$anvil_pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP
fail() { echo "PREPAID ANVIL FAILED: $*"; exit 1; }
(cd contracts && "$FOUNDRY_BIN/forge" build >"$work/forge.log" 2>&1) || { cat "$work/forge.log"; fail "forge build"; }
cargo build --locked -q -p onyx-api --features http-client --example prepaid-rehearsal
# One slot per epoch: anvil's `finalized` block is then two blocks behind `latest`.
(cd "$work" && exec "$FOUNDRY_BIN/anvil" --host 127.0.0.1 --port 0 --slots-in-an-epoch 1 >"$work/anvil.log" 2>&1) &
anvil_pid=$!
port=""
for _ in $(seq 200); do
  kill -0 "$anvil_pid" 2>/dev/null || { cat "$work/anvil.log"; fail "anvil exited"; }
  port=$(grep -oE 'Listening on 127\.0\.0\.1:[0-9]+' "$work/anvil.log" | head -n1 | sed 's/.*://' || true)
  [ -n "$port" ] && break
  sleep 0.1
done
[ -n "$port" ] || fail "anvil did not report its port"
RPC=http://127.0.0.1:$port
# The node goes right after the subcommand: `send --create` takes every later word as its own.
c() { local sub=$1; shift; (cd "$work" && "$FOUNDRY_BIN/cast" "$sub" --rpc-url "$RPC" "$@"); }
[ "$(c chain-id)" = 31337 ] || fail "not a local anvil"
deployer=$(c rpc eth_accounts | python3 -I -c 'import json,sys; print(json.load(sys.stdin)[0])')
# Fresh keys for this run only, never written to the repo.
newkey() { python3 -I -c 'import secrets; print("0x" + secrets.token_hex(32))'; }
export ONYX_PREPAID_BUYER_KEY ONYX_PREPAID_REPORTER_KEY
ONYX_PREPAID_BUYER_KEY=$(newkey)
ONYX_PREPAID_REPORTER_KEY=$(newkey)
addr() { (cd "$work" && "$FOUNDRY_BIN/cast" wallet address --private-key "$1"); }
buyer=$(addr "$ONYX_PREPAID_BUYER_KEY")
reporter=$(addr "$ONYX_PREPAID_REPORTER_KEY")
for a in "$buyer" "$reporter"; do c rpc anvil_setBalance "$a" 0x56BC75E2D63100000 >/dev/null; done
bytecode() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["bytecode"]["object"])' "$repo/contracts/out/$1"; }
deploy() { # artifact, then the abi-encoded constructor args (hex, may be empty)
  local code; code=$(bytecode "$1")$2
  c send --unlocked --from "$deployer" --create "$code" --json | python3 -I -c 'import json,sys; r=json.load(sys.stdin); assert r["status"]=="0x1", r; print(r["contractAddress"])'
}
enc() { (cd "$work" && "$FOUNDRY_BIN/cast" abi-encode "$@") | sed 's/^0x//'; }
token=$(deploy MockUSDC3009.sol/MockUSDC3009.json "")
ledger=$(deploy OnyxRevenue.t.sol/MockLedgerView.json "")
revenue=$(deploy OnyxRevenue.sol/OnyxRevenue.json "$(enc 'f(address,address,address,address,uint16[4])' "$token" "$ledger" "$reporter" "$deployer" '[5000,2000,1000,2000]')")
prepaid=$(deploy OnyxPrepaid.sol/OnyxPrepaid.json "$(enc 'f(address,address,address,uint64)' "$token" "$revenue" "$reporter" 3600)")
fact_a=0x$(printf 'a%.0s' $(seq 64))
fact_b=0x$(printf 'b%.0s' $(seq 64))
sendk() { local k=$1; shift; c send --private-key "$k" "$@" --json >/dev/null; }
sendk "$ONYX_PREPAID_REPORTER_KEY" "$revenue" 'registerObservation(bytes32)' "$fact_a"
sendk "$ONYX_PREPAID_REPORTER_KEY" "$revenue" 'registerObservation(bytes32)' "$fact_b"
c send --unlocked --from "$deployer" "$token" 'mint(address,uint256)' "$buyer" 10000 --json >/dev/null
sendk "$ONYX_PREPAID_BUYER_KEY" "$token" 'approve(address,uint256)' "$prepaid" 5000
sendk "$ONYX_PREPAID_BUYER_KEY" "$prepaid" 'deposit(uint128)' 5000
c rpc anvil_mine 0x3 >/dev/null
echo "deployed: token $token, revenue $revenue, prepaid $prepaid; buyer $buyer deposited 5000"
bin=${CARGO_TARGET_DIR:-$PWD/target}/debug/examples/prepaid-rehearsal
(cd "$work" && "$bin" "$RPC" "$prepaid" "$work/prepaid.journal" "$fact_a" "$fact_b") 2>&1 | tee "$work/run.log"
obs=18446744073709551615
read -r redeemed pool rev_a rev_b spendable < <(
  echo "$(c call "$prepaid" 'accounts(address)(uint128,uint128,uint64)' "$buyer" | sed -n 2p | awk '{print $1}')" \
    "$(c call "$prepaid" 'pool()(uint256)' | awk '{print $1}')" \
    "$(c call "$revenue" 'books(uint64,bytes32)(bool,uint32,uint32,uint64,bytes32,uint128,uint128,uint128)' $obs "$fact_a" | sed -n 8p | awk '{print $1}')" \
    "$(c call "$revenue" 'books(uint64,bytes32)(bool,uint32,uint32,uint64,bytes32,uint128,uint128,uint128)' $obs "$fact_b" | sed -n 8p | awk '{print $1}')" \
    "$(c call "$prepaid" 'spendable(address)(uint128)' "$buyer" | awk '{print $1}')"
)
echo "chain: redeemed $redeemed, pool $pool, book A $rev_a, book B $rev_b, buyer spendable $spendable"
python3 -I - "$work/run.log" <<'PY' || fail "the passes did not report as expected"
import json, sys
lines = [json.loads(l) for l in open(sys.argv[1]) if l.startswith("{")]
answers = [l for l in lines if "answer" in l]
passes = [l for l in lines if "pass" in l]
assert [a["total"] for a in answers] == ["1000", "2000", "3000"], answers
assert [a["spendable"] for a in answers] == ["4000", "3000", "2000"], answers
p1, p2 = passes
assert (p1["redeemed"], p1["allocated"], p1["allocatedAmount"], p1["waiting"]) == (1, 2, "3000", 0), p1
assert p1["redeemTx"] and p1["allocateTx"], p1
assert (p2["redeemed"], p2["allocated"], p2["redeemTx"], p2["allocateTx"]) == (0, 0, None, None), p2
PY
[ "$redeemed" = 3000 ] || fail "redeemed $redeemed, want 3000"
[ "$pool" = 0 ] || fail "pool $pool, want 0"
[ "$rev_a" = 2000 ] || fail "book A revenue $rev_a, want 2000"
[ "$rev_b" = 1000 ] || fail "book B revenue $rev_b, want 1000"
[ "$spendable" = 2000 ] || fail "buyer spendable $spendable, want 2000"
echo "PREPAID ANVIL PASSED"
