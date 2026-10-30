#!/usr/bin/env bash
# CL8 (ADR-0058): an aligned multi-venue feed end to end on a LOCAL anvil (chain id 31337), with
# synthetic venues on loopback only. No real venue is contacted: no venue's terms are cleared
# (docs/ops/market-venues.md). Never sends a Base Sepolia transaction.
#
# Usage: scripts/level2-aligned-anvil.sh [composite]
#   composite  default fixtures/aligned-demo/composite.json (its repo: fixtures/aligned-demo)
#
# What it does (about 2 minutes): deploys OnyxRegistry next to a stub ledger on a fresh anvil;
# for each of the composite's six venue manifests, starts a loopback fake venue answering that
# venue's URL with its recording, five observers fetch it through onyx_fetch, and the round runs
# with its draw announced and fixed on chain, the metric registered from the manifest
# (`registerMetric`) and the settled round recorded (`recordObservation` / `recordAttempt`).
# Then the aligned answer is computed from the venue facts (value, the agreeing reveals' times,
# the venue's own bucket time, the block and time of ObservationRecorded), the registry is
# checked to hold each venue manifest's template id, and the client check recomputes the answer
# and refuses five forged ones. Scenario: venues A, B, C honest; D a thin venue 3% high (a wash
# print at the bucket edge); E stamps its candle 5 s off the grid (a skewed clock); F halted
# (zero volume, last price repeated).
#
# Local only: anvil's public dev accounts; no key is read from .env or .onyx-testnet.
# Env: FOUNDRY_BIN (~/.foundry/bin), ONYX_L2_RPC_PORT (8552), ONYX_L2_LOG_DIR (keep the outputs
# there; default a temp dir under ~/.cache that is removed).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
composite=${1:-fixtures/aligned-demo/composite.json}
crepo=$(dirname "$composite")
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
PORT=${ONYX_L2_RPC_PORT:-8552}
RPC=http://127.0.0.1:$PORT
mkdir -p "$HOME/.cache"
work=$(mktemp -d "$HOME/.cache/onyx-cl8-anvil.XXXXXX")
out=${ONYX_L2_LOG_DIR:-$work/out}
mkdir -p "$out"
anvil_pid=""
cleanup() {
  [ -n "$anvil_pid" ] && kill "$anvil_pid" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
# cast and forge run outside the repo: Foundry loads a .env from its working directory (M8-L55-F4).
castw() { (cd "$work" && ETH_RPC_URL=$RPC "$FOUNDRY_BIN/cast" "$@"); }

(cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/build.log" 2>&1) \
  || { tail -20 "$work/build.log"; exit 1; }
artifact=$REPO/contracts/out/OnyxRegistry.sol/OnyxRegistry.json
cargo build -q -p onyx-level2 --example level2-aligned
bin=$work/bin
mkdir "$bin"
cp "${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/level2-aligned" "$bin/"
la() { "$bin/level2-aligned" "$1" --composite "$composite" --repo "$crepo" "${@:2}"; }
la pin >"$work/pins" || { echo "the composite does not load" >&2; exit 1; }
echo "composite $composite: $(wc -l <"$work/pins") venue feeds"

"$FOUNDRY_BIN/anvil" --port "$PORT" --silent --accounts 10 &
anvil_pid=$!
for _ in $(seq 50); do castw chain-id >/dev/null 2>&1 && break; sleep 0.2; done
[ "$(castw chain-id)" = 31337 ] || { echo "not a local anvil" >&2; exit 1; }

COORD=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266
OWNER=0x70997970C51812dc3A010C7d01b50e0d17dc79C8
LEDGER=0x00000000000000000000000000000000000c0de1
castw rpc anvil_setCode "$LEDGER" \
  "0x60003560e01c63d7d85a5714602c5773${COORD#0x}60005260206000f35b60206000f3" >/dev/null
code=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["bytecode"]["object"])' "$artifact")
ctor=$(castw abi-encode "c(address,address)" "$OWNER" "$LEDGER")
REG=$(castw send --unlocked --from "$OWNER" --create "${code}${ctor#0x}" --json \
  | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["contractAddress"])')
echo "OnyxRegistry at $REG on the local anvil (chain 31337)"
chain=(--rpc "$RPC" --registry "$REG" --cast "$FOUNDRY_BIN/cast")

la demo --work "$out" "${chain[@]}" --coordinator "$COORD" --owner "$OWNER" \
  >"$out/aligned.stdout" 2>"$out/demo.log" || { tail -30 "$out/demo.log"; exit 1; }
grep -E "observers fetched|registered|recorded|wrote" "$out/demo.log" | head -40 || true

# THREATS CL3-8 per venue: the registry holds exactly each venue manifest's template id.
while IFS== read -r metric template; do
  row=$(castw call "$REG" "metrics(bytes32)(uint8,bytes32,uint64)" "$(castw keccak "$metric")" | sed -n 2p)
  [ "$row" = "$template" ] || { echo "registry holds $row for $metric, not $template" >&2; exit 1; }
done <"$work/pins"
echo "registry: every venue metric holds its manifest's template id"

la check --work "$out" "${chain[@]}" --coordinator "$COORD" 2>&1 | tee "$out/check.log"
grep -q "refused (the store of a recorded venue hidden)" "$out/check.log" || { echo "the hidden-venue attack was not refused" >&2; exit 1; }

python3 -I - "$out/aligned.json" <<'PY'
import json, sys
a = json.load(open(sys.argv[1]))
v = {x["venue"]: x for x in a["venues"]}
print(f"aligned {a['metric']} bucket [{a['bucket']['start']}, {a['bucket']['end']}) asOf {a['asOf']}: "
      f"value {a['value']} {a['unit']}, band {a['band']['min']}..{a['band']['max']}, "
      f"spread {a['spreadPpm']} ppm, disagreement {a['disagreement']}, rights {a['rights']}")
for x in a["venues"]:
    if x["included"]:
        print(f"  {x['venue']}: {x['value']} venueTime {x['venueTime']} observed {x['observedFrom']}..{x['observedTo']} "
              f"recorded block {x['recordedAt']['block']} at {x['recordedAt']['time']}")
    else:
        print(f"  {x['venue']}: not counted ({x['reason']})")
assert a["status"] == "aligned" and a["value"] == "67012.50", a["value"]
assert a["disagreement"] is True and a["band"] == {"min": "67011.50", "max": "69022.88"}, a["band"]
assert [x["venue"] for x in a["venues"] if x["included"]] == ["demo.venue_a", "demo.venue_b", "demo.venue_c", "demo.venue_d"]
assert v["demo.venue_e"]["reason"] == "unavailable" and v["demo.venue_f"]["reason"] == "unavailable"
for x in a["venues"]:
    if x["included"]:
        assert x["venueTime"] == a["bucket"]["start"]
        assert a["bucket"]["end"] <= x["window"]["start"] <= x["observedFrom"] <= x["observedTo"] <= x["window"]["end"] <= x["recordedAt"]["time"]
        assert x["recordedAt"]["block"] > 0
PY
echo "CL8 LOCAL ALIGNED DEMO PASSED (anvil, loopback venues only)"
