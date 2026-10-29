#!/usr/bin/env bash
# NORTH-STAR NS11: futures markets on Base Sepolia (testnet only, tONYX test token with no value),
# settled from level-2 values the deployment's OnyxRegistry records (ADR-0042 amendment 2).
#
# Usage:
#   scripts/markets-sepolia.sh deploy <export dir>
#       Deploys tONYX, OnyxReobserve, OnyxPanelOracle, OnyxStakes, OnyxObservedFacts and OnyxFutures
#       next to the registry named in <export dir>/deployment.json and writes
#       <export dir>/markets.json (refused when it exists).
#   scripts/markets-sepolia.sh market <export dir> open|bet|settle|status|underwrite [flags]
#       Runs the Treasury market tool (adapters/source-fiscaldata/examples/market.rs) on that file:
#       open --record-date D --edges E1[,E2] --close T --observable T --resolve-by T
#       bet --market N --key-file .onyx-testnet/keys/miner-0.key --bucket B --amount A
#       settle --market N --store <export dir>/observations/fiscaldata.json [--key-file F]...
#       status [--out FILE]
#       underwrite --key-file F --amount A (with ONYX_MARKETS_FILE=<export dir>/insurance.json):
#       funds the parametric pool so its products have cover for sale; insiders refused (R-PI-4)
#
# Keys: the deployer key from .env (BASE_SEPOLIA_PRIVATE_KEY) deploys, mints tONYX, owns the futures
# and lists facts; it reaches forge and the tool only in their environment, never a command line
# (THREATS B6-1). Bettors sign with their own key files; observers of level-2 rounds
# (validator-9..15) should not bet (THREATS NL6-4).
# Env: ONYX_MARKETS_RPC (https://sepolia.base.org; an anvil fork of Base Sepolia for a
# rehearsal), FOUNDRY_BIN (~/.foundry/bin), ONYX_MARKETS_FILE (the deployment file; `open` runs
# only on guarded futures, so new markets need ONYX_MARKETS_FILE=<export>/insurance.json).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
[ $# -ge 2 ] || { sed -n '4,18p' "$0" >&2; exit 2; }
cmd=$1
export_dir=$(cd "$2" && pwd)
shift 2
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
RPC=${ONYX_MARKETS_RPC:-https://sepolia.base.org}
work=$(mktemp -d "$HOME/.cache/onyx-markets.XXXXXX")
trap 'rm -rf "$work"' EXIT
# cast and forge run outside the repo: Foundry loads a .env from its working directory (M8-L55-F4).
castw() { (cd "$work" && ETH_RPC_URL=$RPC "$FOUNDRY_BIN/cast" "$@"); }
[ "$(castw chain-id)" = 84532 ] || { echo "not Base Sepolia (chain 84532): refused" >&2; exit 1; }
# ONYX_MARKETS_FILE picks the deployment file: markets.json (the first futures, market 1) by
# default; <export>/insurance.json names the guarded futures, where new markets open (NL14).
markets=${ONYX_MARKETS_FILE:-$export_dir/markets.json}

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
[ -n "$key" ] || { echo "BASE_SEPOLIA_PRIVATE_KEY is missing from .env" >&2; exit 1; }

case "$cmd" in
deploy)
  [ ! -e "$markets" ] || { echo "$markets exists: one markets deployment per export" >&2; exit 1; }
  REG=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["registry"])' "$export_dir/deployment.json")
  [ "$(castw code "$REG")" != 0x ] || { echo "no registry code at $REG" >&2; exit 1; }
  cp -a "$REPO/contracts" "$work/contracts"
  rm -rf "$work/contracts/broadcast" "$work/contracts/cache"
  # forge reads the node URL from an endpoint alias, so it never appears on a command line.
  printf '\n[rpc_endpoints]\nonyx_markets = "${ONYX_MARKETS_RPC}"\n' >>"$work/contracts/foundry.toml"
  (cd "$work/contracts" && ONYX_MARKETS_RPC=$RPC BASE_SEPOLIA_PRIVATE_KEY=$key ONYX_REGISTRY=$REG \
    "$FOUNDRY_BIN/forge" script script/DeployMarkets.s.sol:DeployMarkets --rpc-url onyx_markets \
    --broadcast --slow >"$work/deploy.log" 2>&1) || { cat "$work/deploy.log" >&2; exit 1; }
  python3 -I - "$work/contracts/broadcast/DeployMarkets.s.sol/84532/run-latest.json" "$REG" "$markets" <<'PY'
import json, sys
b = json.load(open(sys.argv[1]))
names = {"OnyxTestToken": "token", "OnyxReobserve": "reobserve", "OnyxPanelOracle": "panel",
         "OnyxStakes": "stakes", "OnyxObservedFacts": "facts", "OnyxFutures": "futures"}
out = {"chainId": 84532, "registry": sys.argv[2].lower(), "markets": []}
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
print(f"markets deployed: {len(b['receipts'])} txs, {gas} gas, {fee / 1e18:.9f} test ETH")
for n in names.values():
    print(f"  {n:9} {out[n]}")
PY
  ;;
market)
  [ -e "$markets" ] || { echo "no $markets: run deploy first" >&2; exit 1; }
  CARGO_INCREMENTAL=0 cargo build -q -p onyx-source-fiscaldata --example market
  # A private copy: a build during a slow command never swaps the binary under it (THREATS B6-8).
  cp "${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/market" "$work/market"
  ONYX_MARKETS_ADMIN_KEY=$key "$work/market" "$@" --rpc "$RPC" --markets "$markets" \
    --cast "$FOUNDRY_BIN/cast"
  ;;
*)
  echo "unknown command $cmd (deploy|market)" >&2
  exit 2
  ;;
esac
