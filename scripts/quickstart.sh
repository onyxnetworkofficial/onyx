#!/usr/bin/env bash
# Buyer quickstart (north-star loop NL19, docs/agents.md "Buyer quickstart"): from nothing to a fact you checked
# yourself on the test network. Asks the hosted query API for one level-2 value (an earthquake
# count of a free public source), then checks it with your own copy of the source's template and
# your own read of the Base Sepolia registry: the value is rebuilt from the observers' signed
# commits and reveals, the round's result is matched on chain, and a changed value is refused.
#
# Usage: scripts/quickstart.sh [subject]
#   subject  default usgs.earthquakes.m5_world:2026-07-19 (any day the hosted node serves)
# Env: ONYX_API_URL (https://onyx.cryptagraph.com), ONYX_RPC (https://sepolia.base.org),
#      ONYX_REGISTRY (the hosted deployment's OnyxRegistry; docs/ops/deployment.md)
# Needs: curl, python3, a Rust toolchain (the first run builds the checker, a few minutes) and
# Foundry's `cast` on PATH or in ~/.foundry/bin (the on-chain read). Testnet only; no key, no
# payment: values of free public sources are free to read (ADR-0048 amendment 1).
set -euo pipefail
cd "$(dirname "$0")/.."
API=${ONYX_API_URL:-https://onyx.cryptagraph.com}
RPC=${ONYX_RPC:-https://sepolia.base.org}
REGISTRY=${ONYX_REGISTRY:-0x497389e04f7d58e07a79ac8d395c3927aecbdf2b}
SUBJECT=${1:-usgs.earthquakes.m5_world:2026-07-19}
METRIC=usgs.earthquakes.m5_count
CAST=$(command -v cast || echo "$HOME/.foundry/bin/cast")
[ -x "$CAST" ] || { echo "Foundry's cast is needed for the on-chain read (https://getfoundry.sh)" >&2; exit 2; }
t0=$(date +%s)

echo "1. The node: $API/v1/health"
curl -fsS -m 20 -A onyx-quickstart "$API/v1/health" | python3 -I -c '
import json, sys
h = json.load(sys.stdin)
print("   chain %s, settlement %s, rights %s" % (h["chainId"], h["contract"], h["rights"]))
if h["chainId"] != 84532:
    sys.exit("   not Base Sepolia: stop")'

echo "2. One value: $METRIC for $SUBJECT"
cache=${XDG_CACHE_HOME:-$HOME/.cache}
mkdir -p "$cache"
work=$(mktemp -d "$cache/onyx-quickstart.XXXXXX")
trap 'rm -rf "$work"' EXIT
curl -fsS -m 20 -A onyx-quickstart -o "$work/answer.json" "$API/v1/observations/$METRIC?subject=$SUBJECT"
python3 -I -c '
import json, sys
a = json.load(open(sys.argv[1]))
print("   the node says: %s (proof level %s); not trusted yet" % (a["value"], a.get("proofLevel")))' "$work/answer.json"

echo "3. Build the checker (once; later runs reuse it)"
cargo build --locked -q --release -p onyx-source-usgs --example level2-check

echo "4. Check it yourself: your template, your read of the registry on Base Sepolia"
"${CARGO_TARGET_DIR:-target}/release/examples/level2-check" --url "$API" --subject "$SUBJECT" \
  --rpc "$RPC" --registry "$REGISTRY" --cast "$CAST" | tee "$work/check.json"
python3 -I -c '
import json, sys
c = json.load(open(sys.argv[1]))
assert c["client_checks"].startswith("passed"), c
assert c["forged_value_refused"] is True, c
print("VERIFIED: %s %s = %s (round %s..., checked off chain and on chain in %d s)" % (c["metric"], c["subject"], c["value"], c["round"][:10], int(sys.argv[2])))' \
  "$work/check.json" "$(( $(date +%s) - t0 ))"
