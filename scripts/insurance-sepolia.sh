#!/usr/bin/env bash
# North-star loop NL14 and NL18: the second markets deployment on Base Sepolia (testnet only, tONYX
# test token with no value): a guarded OnyxFutures and OnyxParametric, both settled from level-2
# values the deployment's OnyxRegistry records through a new OnyxObservedFacts (ADR-0042
# amendment 3, docs/spec/parametric-insurance.md).
#
# Usage:
#   scripts/insurance-sepolia.sh deploy <export dir>
#       Deploys OnyxObservedFacts, OnyxFutures and OnyxParametric next to the registry and the
#       tONYX, OnyxReobserve and OnyxPanelOracle of <export dir>/markets.json, and writes
#       <export dir>/insurance.json (refused when it exists).
#   scripts/insurance-sepolia.sh offer <export dir> --metric M --template 0x.. --subject S
#       --cmp atleast|atmost --threshold N --payout A --premium A --buy-by T --close-by T
#       --observable T --resolve-by T --max N
#       Lists the fact (earliest recording block estimated at --observable, THREATS NL6-11;
#       latest close --close-by, the source's earliest publication less a margin, NL6-10) and
#       offers a product. Amounts in whole tONYX; times in unix seconds.
#   scripts/insurance-sepolia.sh status <export dir>
#   scripts/insurance-sepolia.sh demo-fork <export dir>
#       Meeting demo on a throwaway anvil fork of Base Sepolia (nothing is sent to the real chain,
#       no real key is used): the products of <export dir>/insurance.json are offered again on
#       fork-deployed copies of the contracts (same facts and terms; only the earliest recording
#       block is near, since a fork cannot skip millions of blocks), an anvil test account
#       underwrites, another buys one policy of each, the registry's coordinator (impersonated on
#       the fork) records a demo value for each fact, and one policy pays while the other expires.
#
# Keys: the deployer key from .env (BASE_SEPOLIA_PRIVATE_KEY) deploys, lists and offers; it reaches
# forge only through its environment, never a command line (THREATS B6-1). It is an
# insider (owner and the registry's coordinator), so it can neither underwrite nor buy (R-PI-4).
# Env: ONYX_MARKETS_RPC (https://sepolia.base.org; an anvil fork of Base Sepolia for a
# rehearsal, which skips the chain check), FOUNDRY_BIN (~/.foundry/bin).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
[ $# -ge 2 ] || { sed -n '7,20p' "$0" >&2; exit 2; }
cmd=$1
export_dir=$(cd "$2" && pwd)
shift 2
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
RPC=${ONYX_MARKETS_RPC:-https://sepolia.base.org}
work=$(mktemp -d "$HOME/.cache/onyx-insurance.XXXXXX")
trap 'rm -rf "$work"' EXIT
# cast and forge run outside the repo: Foundry loads a .env from its working directory (M8-L55-F4).
castw() { (cd "$work" && ETH_RPC_URL=$RPC "$FOUNDRY_BIN/cast" "$@"); }
if [ "$cmd" = demo-fork ]; then
  [ -e "$export_dir/insurance.json" ] || { echo "no $export_dir/insurance.json: run deploy first" >&2; exit 1; }
  RPC=http://127.0.0.1:${ONYX_DEMO_PORT:-8549}
  "$FOUNDRY_BIN/anvil" --fork-url "${ONYX_MARKETS_RPC:-https://sepolia.base.org}" \
    --port "${ONYX_DEMO_PORT:-8549}" --silent >"$work/anvil.log" 2>&1 &
  anvil_pid=$!
  trap 'kill "$anvil_pid" 2>/dev/null; rm -rf "$work"' EXIT
  for _ in $(seq 60); do castw block-number >/dev/null 2>&1 && break; sleep 1; done
fi
chain=$(castw chain-id)
case "$chain" in
84532) ;;
31337) echo "anvil (chain 31337): rehearsal" >&2 ;;
*) echo "not Base Sepolia (chain 84532) or anvil: refused" >&2; exit 1 ;;
esac
markets=$export_dir/markets.json
out=$export_dir/insurance.json
[ -e "$markets" ] || { echo "no $markets (scripts/markets-sepolia.sh deploy first)" >&2; exit 1; }
field() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$1" "$2"; }

key=$(python3 -I - "$REPO/.env" <<'PY'
import sys
try:
    lines = open(sys.argv[1]).read().splitlines()
except FileNotFoundError:
    sys.exit(0)
for line in lines:
    k, sep, v = line.partition("=")
    if sep and k.strip() == "BASE_SEPOLIA_PRIVATE_KEY":
        print(v.strip().strip('"').strip("'"))
        break
PY
)
[ -n "$key" ] || [ "$cmd" = demo-fork ] || { echo "BASE_SEPOLIA_PRIVATE_KEY is missing from .env" >&2; exit 1; }

case "$cmd" in
deploy)
  [ ! -e "$out" ] || { echo "$out exists: one second deployment per export" >&2; exit 1; }
  REG=$(field "$markets" registry)
  [ "$(castw code "$REG")" != 0x ] || { echo "no registry code at $REG" >&2; exit 1; }
  cp -a "$REPO/contracts" "$work/contracts"
  rm -rf "$work/contracts/broadcast" "$work/contracts/cache"
  # forge reads the node URL from an endpoint alias, so it never appears on a command line.
  printf '\n[rpc_endpoints]\nonyx_markets = "${ONYX_MARKETS_RPC}"\n' >>"$work/contracts/foundry.toml"
  (cd "$work/contracts" && ONYX_MARKETS_RPC=$RPC BASE_SEPOLIA_PRIVATE_KEY=$key ONYX_REGISTRY=$REG \
    ONYX_MARKETS_TOKEN=$(field "$markets" token) ONYX_MARKETS_REOBSERVE=$(field "$markets" reobserve) \
    ONYX_MARKETS_PANEL=$(field "$markets" panel) \
    "$FOUNDRY_BIN/forge" script script/DeployInsurance.s.sol:DeployInsurance --rpc-url onyx_markets \
    --broadcast --slow >"$work/deploy.log" 2>&1) || { cat "$work/deploy.log" >&2; exit 1; }
  python3 -I - "$work/contracts/broadcast/DeployInsurance.s.sol/$chain/run-latest.json" "$markets" "$out" <<'PY'
import json, sys
b = json.load(open(sys.argv[1]))
first = json.load(open(sys.argv[2]))
names = {"OnyxObservedFacts": "facts", "OnyxFutures": "futures", "OnyxParametric": "insurance"}
out = {k: first[k] for k in ("chainId", "registry", "token", "reobserve", "panel")}
out["products"] = []
blocks, gas, fee = [], 0, 0
for t in b["transactions"]:
    if t["transactionType"] == "CREATE" and t["contractName"] in names:
        out[names[t["contractName"]]] = t["contractAddress"].lower()
for r in b["receipts"]:
    blocks.append(int(r["blockNumber"], 16))
    g = int(r["gasUsed"], 16)
    gas += g
    fee += g * int(r["effectiveGasPrice"], 16) + int(r.get("l1Fee", "0x0"), 16)
missing = [n for n in names.values() if n not in out]
if missing:
    sys.exit(f"broadcast without {missing}")
out["deployBlock"] = min(blocks)
open(sys.argv[3], "w").write(json.dumps(out, indent=2) + "\n")
print(f"insurance deployed: {len(b['receipts'])} txs, {gas} gas, {fee / 1e18:.9f} test ETH")
for n in names.values():
    print(f"  {n:9} {out[n]}")
PY
  ;;
offer)
  [ -e "$out" ] || { echo "no $out: run deploy first" >&2; exit 1; }
  declare -A a=()
  while [ $# -gt 0 ]; do
    [ $# -ge 2 ] || { echo "flag $1 needs a value" >&2; exit 2; }
    a[${1#--}]=$2
    shift 2
  done
  for f in metric template subject cmp threshold payout premium buy-by close-by observable resolve-by max; do
    [ -n "${a[$f]:-}" ] || { echo "--$f is required" >&2; exit 2; }
  done
  case "${a[cmp]}" in atleast) cmpn=0 ;; atmost) cmpn=1 ;; *) echo "--cmp atleast|atmost" >&2; exit 2 ;; esac
  [ "${a[buy-by]}" -le "${a[close-by]}" ] || { echo "--buy-by after --close-by: refused (NL6-10)" >&2; exit 1; }
  facts=$(field "$out" facts)
  ins=$(field "$out" insurance)
  mk=$(castw keccak "$(printf '%s' "${a[metric]}" | od -An -tx1 | tr -d ' \n' | sed 's/^/0x/')")
  # subject_key: keccak256(borsh(Subject::Aggregate(subject))): variant 3, u32 LE length, bytes.
  sk=$(castw keccak "$(python3 -I -c 'import sys; s=sys.argv[1].encode(); print("0x"+(bytes([3])+len(s).to_bytes(4,"little")+s).hex())' "${a[subject]}")")
  # The block at --observable, from the chain's own recent block rate (late is safe).
  head=$(castw block-number)
  now=$(castw block latest --field timestamp)
  then=$(castw block $((head - 10000)) --field timestamp)
  ms=$(( (now - then) * 1000 / 10000 )); [ "$ms" -ge 1 ] || ms=1
  nb=$(( head + (${a[observable]} - now) * 1000 / ms ))
  fact=$(castw call "$(field "$out" registry)" 'factKeyOf(bytes32,bytes32,bytes32,uint64,bool)(bytes32)' \
    "$mk" "${a[template]}" "$sk" 0 true)
  cp -a "$REPO/contracts" "$work/contracts"
  rm -rf "$work/contracts/broadcast" "$work/contracts/cache"
  printf '\n[rpc_endpoints]\nonyx_markets = "${ONYX_MARKETS_RPC}"\n' >>"$work/contracts/foundry.toml"
  (cd "$work/contracts" && ONYX_MARKETS_RPC=$RPC BASE_SEPOLIA_PRIVATE_KEY=$key ONYX_FACTS=$facts \
    ONYX_INSURANCE=$ins P_METRIC=$mk P_TEMPLATE=${a[template]} P_SUBJECT_KEY=$sk P_NOT_BEFORE=$nb \
    P_CLOSE_BY=${a[close-by]} P_CMP=$cmpn P_THRESHOLD=${a[threshold]} \
    P_PAYOUT=${a[payout]}000000000000000000 P_PREMIUM=${a[premium]}000000000000000000 \
    P_BUY_BY=${a[buy-by]} P_OBSERVABLE=${a[observable]} P_RESOLVE_BY=${a[resolve-by]} P_MAX=${a[max]} \
    "$FOUNDRY_BIN/forge" script script/DeployInsurance.s.sol:OfferPolicy --rpc-url onyx_markets \
    --broadcast --slow >"$work/offer.log" 2>&1) || { cat "$work/offer.log" >&2; exit 1; }
  [ "$(castw call "$facts" 'closeBy(bytes32)(uint64)' "$fact" | cut -d' ' -f1)" = "${a[close-by]}" ] \
    || { echo "the listing did not take" >&2; exit 1; }
  id=$(castw call "$ins" 'productCount()(uint256)' | cut -d' ' -f1)
  python3 -I - "$out" "$id" "$fact" "$nb" <<PY
import json, sys
p = json.load(open(sys.argv[1]))
p["products"].append({"id": int(sys.argv[2]), "fact": sys.argv[3], "notBeforeBlock": int(sys.argv[4]),
    "metric": "${a[metric]}", "subject": "${a[subject]}", "cmp": "${a[cmp]}",
    "threshold": "${a[threshold]}", "payout": "${a[payout]}", "premium": "${a[premium]}",
    "buyBy": ${a[buy-by]}, "closeBy": ${a[close-by]}, "observableAt": ${a[observable]},
    "resolveBy": ${a[resolve-by]}, "maxPolicies": ${a[max]}})
open(sys.argv[1], "w").write(json.dumps(p, indent=2) + "\n")
print(f"product {sys.argv[2]} offered on fact {sys.argv[3]}")
PY
  ;;
status)
  [ -e "$out" ] || { echo "no $out: run deploy first" >&2; exit 1; }
  ins=$(field "$out" insurance)
  echo "pool $(castw call "$ins" 'pool()(uint256)') reserved $(castw call "$ins" 'reserved()(uint256)')"
  n=$(castw call "$ins" 'productCount()(uint256)' | cut -d' ' -f1)
  for i in $(seq 1 "$n"); do
    echo "product $i: $(castw call "$ins" 'product(uint256)((bytes32,uint8,uint8,uint32,uint32,uint64,uint64,uint64,int256,uint128,uint128,int256))' "$i")"
  done
  ;;
demo-fork)
  ins_live=$(field "$out" insurance)
  facts_live=$(field "$out" facts)
  REG=$(field "$out" registry)
  token=$(field "$out" token)
  coord=$(castw call "$(castw call "$REG" 'ledger()(address)')" 'coordinator()(address)')
  # anvil's own unlocked test accounts (fork only): the copies' owner, an underwriter, a buyer.
  owner=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266
  uw=0x70997970C51812dc3A010C7d01b50e0d17dc79C8
  buyer=0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC
  castw rpc anvil_impersonateAccount "$coord" >/dev/null
  castw rpc anvil_setBalance "$coord" 0x8AC7230489E80000 >/dev/null
  as() { local from=$1; shift; castw send --unlocked --from "$from" "$@" >/dev/null; }
  (cd "$REPO/contracts" && "$FOUNDRY_BIN/forge" build -q)
  create() {
    local art=$REPO/contracts/out/$1.sol/$1.json
    local code
    code=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["bytecode"]["object"])' "$art")
    castw send --unlocked --from "$owner" --create "$code$(castw abi-encode "constructor($2)" "${@:3}" | cut -c3-)" \
      --json | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["contractAddress"])'
  }
  facts=$(create OnyxObservedFacts address,address "$REG" "$owner")
  ins=$(create OnyxParametric address,address,address,address "$token" "$facts" "$facts" "$(field "$out" reobserve)")
  echo "fork copies: facts $facts, insurance $ins (owner: anvil test account $owner)"
  n=$(castw call "$ins_live" 'productCount()(uint256)' | cut -d' ' -f1)
  P='product(uint256)((bytes32,uint8,uint8,uint32,uint32,uint64,uint64,uint64,int256,uint128,uint128,int256))'
  for i in $(seq 1 "$n"); do
    read -r fact cmp _ _ max buyby obs res thr payout premium _ < <(castw call "$ins_live" "$P" "$i" \
      | tr -d '()' | sed 's/ \[[^]]*\]//g' | tr ',' ' ')
    read -r metric sk _ _ < <(castw call "$facts_live" 'listed(bytes32)(bytes32,bytes32,uint64,uint64)' "$fact" | tr '\n' ' '; echo)
    tpl=$(castw call "$REG" 'metrics(bytes32)(uint8,bytes32,uint64)' "$metric" | sed -n 2p)
    closeby=$(castw call "$facts_live" 'closeBy(bytes32)(uint64)' "$fact" | cut -d' ' -f1)
    as "$owner" "$facts" 'list(bytes32,bytes32,bytes32,uint64,bool,uint64,uint64)' "$metric" "$tpl" "$sk" 0 true \
      $(( $(castw block-number) + 5 )) "$closeby"
    as "$owner" "$ins" 'offer(bytes32,uint8,int256,uint128,uint128,uint64,uint64,uint64,uint32)' \
      "$fact" "$cmp" "$thr" "$payout" "$premium" "$buyby" "$obs" "$res" "$max"
  done
  as "$coord" "$token" 'mint(address,uint256)' "$uw" 1000000000000000000000
  as "$coord" "$token" 'mint(address,uint256)' "$buyer" 100000000000000000000
  as "$uw" "$token" 'approve(address,uint256)' "$ins" 1000000000000000000000
  as "$buyer" "$token" 'approve(address,uint256)' "$ins" 100000000000000000000
  as "$uw" "$ins" 'underwrite(uint256)' 500000000000000000000
  for i in $(seq 1 "$n"); do as "$buyer" "$ins" 'buy(uint256,uint32)' "$i" 1; done
  bal() { castw call "$token" 'balanceOf(address)(uint256)' "$1" | cut -d' ' -f1; }
  b0=$(bal "$buyer")
  echo "underwriter put 500 tONYX in the pool; buyer bought 1 policy of each product (balance now $(python3 -c "print($b0/1e18)") tONYX)"
  # A demo value per product: the first triggers its policy, every later one does not.
  for i in $(seq 1 "$n"); do
    read -r fact cmp _ _ _ _ obs _ thr _ _ _ < <(castw call "$ins" "$P" "$i" | tr -d '()' | sed 's/ \[[^]]*\]//g' | tr ',' ' ')
    read -r metric sk _ _ < <(castw call "$facts" 'listed(bytes32)(bytes32,bytes32,uint64,uint64)' "$fact" | tr '\n' ' '; echo)
    if [ "$i" = 1 ]; then
      value=$(( cmp == 0 ? thr + 1 : thr - 25 ))
    else
      value=$(( cmp == 0 ? thr - 2 : thr + 25 ))
    fi
    now=$(castw block latest --field timestamp)
    [ "$now" -ge "$obs" ] || castw rpc evm_increaseTime $(( obs - now + 60 )) >/dev/null
    castw rpc evm_mine >/dev/null
    spec=$(castw keccak "onyx-demo-fork-spec-$i")
    round=$(castw keccak "onyx-demo-fork-round-$i")
    pool=$(castw keccak "onyx-demo-fork-pool")
    cfg=$(castw keccak "onyx-demo-fork-cfg")
    attempt=$(castw call "$REG" 'drawCount(bytes32)(uint32)' "$fact")
    commit=$(castw call "$REG" 'drawCommit(bytes32,bytes32,bytes32)(bytes32)' "$pool" "$spec" "$cfg")
    as "$coord" "$REG" 'announceDraw(bytes32,bytes32,uint64,uint32,bytes32,uint64)' "$metric" "$sk" "$obs" \
      "$attempt" "$commit" $(( $(castw block-number) + 2 ))
    castw rpc anvil_mine 0x3 >/dev/null
    as "$buyer" "$REG" 'fixBeacon(bytes32,uint32)' "$fact" "$attempt"
    result=$(castw call "$facts" 'resultHash(bytes32,bytes32,int128)(bytes32)' "$spec" "$round" "$value")
    as "$coord" "$REG" 'recordObservation(bytes32,bytes32,uint64,bytes32,bytes32,bytes32,bytes32)' \
      "$metric" "$sk" "$obs" "$spec" "$result" "$pool" "$cfg"
    as "$buyer" "$facts" 'prove(bytes32,bytes32,int128)' "$fact" "$round" "$value"
    as "$buyer" "$ins" 'settle(uint256)' "$i"
    state=$(castw call "$ins" "$P" "$i" | tr -d '()' | sed 's/ \[[^]]*\]//g' | tr ',' ' ' | awk '{print $3}')
    case "$state" in 2) word="PAID: the buyer claims the payout" ;; 3) word="EXPIRED: the premium stays with the pool" ;; *) word="state $state" ;; esac
    echo "product $i: the coordinator recorded the demo value $value (threshold $thr, $( [ "$cmp" = 0 ] && echo 'at least' || echo 'at most' )); $word"
    [ "$state" = 2 ] && as "$buyer" "$ins" 'claim(uint256)' "$i"
  done
  b1=$(bal "$buyer")
  pool=$(castw call "$ins" 'pool()(uint256)' | cut -d' ' -f1)
  echo "buyer: $(python3 -c "print($b0/1e18)") -> $(python3 -c "print($b1/1e18)") tONYX; pool now $(python3 -c "print($pool/1e18)") tONYX"
  echo "DEMO PASSED (fork only: demo values, test token, nothing sent to Base Sepolia)"
  ;;
*)
  echo "unknown command $cmd (deploy|offer|status|demo-fork)" >&2
  exit 2
  ;;
esac
