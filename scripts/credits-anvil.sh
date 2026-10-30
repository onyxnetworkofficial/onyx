#!/usr/bin/env bash
# The credits ledger end to end on a local anvil (contributor loop CL6, ADR-0056, THREATS CL6).
# Deploys a mock USDC with a real EIP-3009 receiveWithAuthorization, OnyxRevenueV2 (one shard book
# whose only miner is `miner`), OnyxPrepaidV2 and an OnyxRegistry with one announced draw. Then:
# - the miner (it holds ETH) earns 80% of a 1,000,000 fee and moves it into credits with
#   `withdrawToCredits` (origin "earned"); a forged "earned" tag from anyone else reverts;
# - `examples/credits-rehearsal.rs` runs the API: an observer holding NO ETH deposits through the
#   relayer, posts its commit and the receipt for it through the relayer, each paid by a voucher
#   adding the action's fixed fee; a forged commit, a repeated post (released, not charged) and a
#   relay over the hourly cap are refused; the account views and lines are read with the accounts'
#   own proofs; one reporter pass pays the fees to the relayer with `payRelayer`;
# - the chain is checked: the observer still has 0 ETH, its commit and receipt are logged, its
#   account and the pool, the relayer's token balance, and conservation (token held = unspent
#   balances + pool).
# Local only: chain id 31337. Every key is made fresh for the run and thrown away (anvil gas and
# mock tokens only); the deployer and the coordinator are anvil's unlocked dev accounts. cast runs
# in a temp dir outside the repo (Foundry loads a `.env` from its working directory).
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
fail() { echo "CREDITS ANVIL FAILED: $*"; exit 1; }
(cd contracts && "$FOUNDRY_BIN/forge" build >"$work/forge.log" 2>&1) || { cat "$work/forge.log"; fail "forge build"; }
cargo build --locked -q -p onyx-api --features http-client --example credits-rehearsal
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
c() { local sub=$1; shift; (cd "$work" && "$FOUNDRY_BIN/cast" "$sub" --rpc-url "$RPC" "$@"); }
[ "$(c chain-id)" = 31337 ] || fail "not a local anvil"
read -r deployer coordinator buyer < <(c rpc eth_accounts | python3 -I -c 'import json,sys; a=json.load(sys.stdin); print(a[0], a[1], a[2])')
newkey() { python3 -I -c 'import secrets; print("0x" + secrets.token_hex(32))'; }
export ONYX_CREDITS_OBSERVER_KEY ONYX_CREDITS_RELAYER_KEY ONYX_CREDITS_REPORTER_KEY ONYX_CREDITS_RECEIPT_KEY ONYX_CREDITS_MINER_KEY
ONYX_CREDITS_OBSERVER_KEY=$(newkey)
ONYX_CREDITS_RELAYER_KEY=$(newkey)
ONYX_CREDITS_REPORTER_KEY=$(newkey)
ONYX_CREDITS_RECEIPT_KEY=$(newkey)
ONYX_CREDITS_MINER_KEY=$(newkey)
addr() { (cd "$work" && "$FOUNDRY_BIN/cast" wallet address --private-key "$1"); }
observer=$(addr "$ONYX_CREDITS_OBSERVER_KEY")
relayer=$(addr "$ONYX_CREDITS_RELAYER_KEY")
reporter=$(addr "$ONYX_CREDITS_REPORTER_KEY")
receipt=$(addr "$ONYX_CREDITS_RECEIPT_KEY")
miner=$(addr "$ONYX_CREDITS_MINER_KEY")
# Gas for the relayer, the reporter and the miner. The observer gets none.
for a in "$relayer" "$reporter" "$miner"; do c rpc anvil_setBalance "$a" 0x56BC75E2D63100000 >/dev/null; done
bytecode() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["bytecode"]["object"])' "$repo/contracts/out/$1"; }
deploy() { # artifact, then the abi-encoded constructor args (hex, may be empty)
  local code; code=$(bytecode "$1")$2
  c send --unlocked --from "$deployer" --create "$code" --json | python3 -I -c 'import json,sys; r=json.load(sys.stdin); assert r["status"]=="0x1", r; print(r["contractAddress"])'
}
enc() { (cd "$work" && "$FOUNDRY_BIN/cast" abi-encode "$@") | sed 's/^0x//'; }
kk() { (cd "$work" && "$FOUNDRY_BIN/cast" keccak "$@"); }
sendu() { local from=$1; shift; c send --unlocked --from "$from" "$@" --json >/dev/null; }
sendk() { local k=$1; shift; c send --private-key "$k" "$@" --json >/dev/null; }
token=$(deploy MockUSDC3009Receive.sol/MockUSDC3009Receive.json "")
ledgerview=$(deploy OnyxRevenue.t.sol/MockLedgerView.json "")
revenue=$(deploy OnyxRevenueV2.sol/OnyxRevenueV2.json "$(enc 'f(address,address,address,address,uint16[4])' "$token" "$ledgerview" "$reporter" "$deployer" '[5000,2000,1000,2000]')")
prepaid=$(deploy OnyxPrepaidV2.sol/OnyxPrepaidV2.json "$(enc 'f(address,address,address,address,uint64,uint128)' "$token" "$revenue" "$reporter" "$relayer" 3600 1000000)")
regledger=$(deploy MockRegistryLedger.sol/MockRegistryLedger.json "$(enc 'f(address)' "$coordinator")")
registry=$(deploy OnyxRegistry.sol/OnyxRegistry.json "$(enc 'f(address,address)' "$deployer" "$regledger")")
echo "deployed: token $token, revenue $revenue, prepaid $prepaid, registry $registry"

# A level-2 draw for the observer's posts: a feed metric, a receipt signer, one announced draw.
metric=$(kk "credits.demo.feed")
template=$(kk "credits demo template")
subject=$(kk "credits demo subject")
sendu "$deployer" "$registry" 'registerMetric(bytes32,uint8,bytes32,uint8)' "$metric" 2 "$template" 0
sendu "$coordinator" "$registry" 'setReceiptSigner(address)' "$receipt"
height=$(( $(c block-number) + 2 ))
sendu "$coordinator" "$registry" 'announceDraw(bytes32,bytes32,uint64,uint32,bytes32,uint64)' "$metric" "$subject" 1000000 0 "$(kk 'credits demo draw')" "$height"
c rpc anvil_mine 0x3 >/dev/null
fact=$(c call "$registry" 'factKey(bytes32,bytes32,uint64)(bytes32)' "$metric" "$subject" 1000000)
sendu "$deployer" "$registry" 'fixBeacon(bytes32,uint32)' "$fact" 0

# Earned credit: the miner's 80% of a 1,000,000 fee on its shard, moved into credits.
root=$(kk "credits demo root")
zero=0x0000000000000000000000000000000000000000000000000000000000000000
packed=0x$(python3 -I -c 'print(b"onyx-commitment-v1".hex())')${root#0x}0000000000000000$(printf '0%.0s' $(seq 384))
binding=$(kk "$packed")
sendu "$deployer" "$ledgerview" 'set(bytes32,bytes32,bool)' "$root" "$binding" false
sendu "$deployer" "$ledgerview" 'addReveal(address,bytes32)' "$miner" "$root"
shard=0x$(printf '5%.0s' $(seq 64))
sendu "$deployer" "$revenue" 'register(uint64,bytes32,(bytes32,uint64,bytes32,bytes32,bytes32,bytes32,bytes32,bytes32))' 1 "$shard" "($root,0,$zero,$zero,$zero,$zero,$zero,$zero)"
sendu "$deployer" "$token" 'mint(address,uint256)' "$buyer" 1000000
sendu "$buyer" "$token" 'approve(address,uint256)' "$revenue" 1000000
sendu "$buyer" "$revenue" 'pay(uint64,bytes32,uint128)' 1 "$shard" 1000000
sendu "$deployer" "$revenue" 'claim(uint64,bytes32,address)' 1 "$shard" "$miner"
sendk "$ONYX_CREDITS_MINER_KEY" "$revenue" 'withdrawToCredits(address)' "$prepaid"
# A forged "earned" tag from anyone else reverts (THREATS CL6-8).
sendu "$deployer" "$token" 'mint(address,uint256)' "$buyer" 10
sendu "$buyer" "$token" 'approve(address,uint256)' "$prepaid" 10
if c send --unlocked --from "$buyer" "$prepaid" 'depositFor(address,uint128,uint8)' "$buyer" 10 2 --json >/dev/null 2>&1; then
  fail "depositFor with origin 2 from a buyer was accepted"
fi
# The observer's tokens (no ETH): 10 units of the mock asset.
sendu "$deployer" "$token" 'mint(address,uint256)' "$observer" 10000000
c rpc anvil_mine 0x3 >/dev/null
nonce_before=$(c nonce "$relayer")
bin=${CARGO_TARGET_DIR:-$PWD/target}/debug/examples/credits-rehearsal
(cd "$work" && "$bin" "$RPC" "$prepaid" "$registry" "$token" "$work/prepaid.journal" "$fact") 2>&1 | tee "$work/run.log"
first() { awk '{print $1}'; }
eth=$(c balance "$observer")
read -r dep red ex <<<"$(c call "$prepaid" 'accounts(address)(uint128,uint128,uint64)' "$observer" | first | tr '\n' ' ')"
read -r mdep mred _ <<<"$(c call "$prepaid" 'accounts(address)(uint128,uint128,uint64)' "$miner" | first | tr '\n' ' ')"
pool=$(c call "$prepaid" 'pool()(uint256)' | first)
held=$(c call "$token" 'balanceOf(address)(uint256)' "$prepaid" | first)
paid=$(c call "$token" 'balanceOf(address)(uint256)' "$relayer" | first)
round=$(kk "credits demo round")
logged=$(c call "$registry" 'commitLogOf(bytes32,uint32,address,bytes32)(bytes32,uint64,bytes)' "$fact" 0 "$observer" "$round" | head -n1)
rlog=$(c call "$registry" 'receiptLog(bytes32)(bytes32)' "$(c call "$registry" 'logKey(bytes32,uint32,address,bytes32)(bytes32)' "$fact" 0 "$observer" "$round")")
sends=$(( $(c nonce "$relayer") - nonce_before ))
echo "chain: observer ETH $eth, account ($dep, $red, $ex), miner ($mdep, $mred), pool $pool, prepaid holds $held, relayer paid $paid, relayer sent $sends transaction(s)"
python3 -I - "$work/run.log" <<'PY' || fail "the rehearsal did not report as expected"
import json, sys
s = {}
for l in open(sys.argv[1]):
    if l.startswith("{"):
        j = json.loads(l)
        s.setdefault(j["step"], []).append(j)
one = lambda k: s[k][0]
assert one("schedule")["status"] == 200 and len(one("schedule")["actions"]) == 4, one("schedule")
for k, fee in (("deposit", "2000"), ("commit", "1000"), ("receipt", "1000")):
    assert (one(k)["status"], one(k)["fee"], one(k)["charged"]) == (200, fee, True), one(k)
assert (one("forged_commit")["status"], one("forged_commit")["code"]) == (403, "relay_not_signed"), one("forged_commit")
assert (one("repeat_commit")["status"], one("repeat_commit")["code"]) == (422, "relay_would_revert"), one("repeat_commit")
assert (one("over_cap")["status"], one("over_cap")["code"]) == (429, "relay_rate_limited"), one("over_cap")
pub = one("observer_public")["view"]
assert pub["view"] == "public" and "spent" not in pub and pub["deposited"] == "5000000", pub
acc = one("observer_account")["view"]
assert (acc["view"], acc["spent"], acc["held"], acc["spendable"]) == ("account", "4000", "0", "4996000"), acc
assert acc["credited"]["deposit"] == "5000000" and acc["credited"]["earned"] == "0", acc
lines = one("observer_lines")
# The repeated commit and the over-cap relay were each held, then released (nothing charged).
assert lines["kinds"] == ["relay_hold", "gas", "relay_hold", "gas", "relay_hold", "gas", "relay_hold", "release", "relay_hold", "release"], lines
assert (one("lines_without_proof")["status"], one("lines_without_proof")["code"]) == (403, "account_proof_required")
miner = one("miner_account")["view"]
assert miner["credited"]["earned"] == "800000" and miner["spendable"] == "800000", miner
p1, p2 = s["pass"]
assert (p1["redeemed"], p1["relayerPaid"], p1["allocated"]) == (1, "4000", 0) and p1["relayerTx"], p1
assert (p2["redeemed"], p2["relayerPaid"], p2["relayerTx"]) == (0, "0", None), p2
for k in ("deposit", "commit", "receipt"):
    print(f"gas used by the relayed {k}: {one(k)['gasUsed']}")
PY
[ "$eth" = 0 ] || fail "the observer holds $eth wei; it must have needed none"
[ "$dep $red $ex" = "5000000 4000 0" ] || fail "observer account ($dep, $red, $ex), want (5000000, 4000, 0)"
[ "$mdep $mred" = "800000 0" ] || fail "miner account ($mdep, $mred), want (800000, 0)"
[ "$pool" = 0 ] || fail "pool $pool, want 0"
[ "$paid" = 4000 ] || fail "relayer paid $paid, want 4000"
[ "$held" = $(( dep - red + mdep - mred + pool )) ] || fail "conservation: held $held != unspent balances + pool"
[ "$logged" = "$(kk 'credits demo commit')" ] || fail "commit not logged: $logged"
[ "$rlog" = "$(kk 'credits demo commit')" ] || fail "receipt not logged: $rlog"
# deposit, commit, receipt: 3 relayed transactions; the forged, repeated and over-cap ones sent nothing.
[ "$sends" = 3 ] || fail "the relayer sent $sends transactions, want 3"
echo "CREDITS ANVIL PASSED"
