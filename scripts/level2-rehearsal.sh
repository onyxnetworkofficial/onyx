#!/usr/bin/env bash
# Level-2 rehearsals with real on-chain draws (L2RND-R1): start a local anvil,
# deploy OnyxRegistry next to a stub ledger whose coordinator is anvil account
# 0, and run the recorded Treasury, SEC, USGS and NCEI rounds. Each draw is announced on
# the registry before its beacon height, the round book is saved, the beacon
# is fixed from the anvil's block hash (`fixBeacon`), and the round opens with
# it. Writes the site summaries (apps/web/static/level2/) and the draw records
# the recorded replays use (adapters/source-*/fixtures/*/draw.json).
# Local only: chain id 31337 and anvil's unlocked dev accounts (no key is read
# or passed). cast and forge run in a temp dir outside the repo: Foundry loads
# a `.env` from its working directory and project root (M8-L55-F4).
#
# Then an in-phase round on a second anvil whose clock starts inside the
# Treasury round's window (review REVLOG-R3-4): a griefing observer posts its
# own commit in the commit phase, and the coordinator must absorb it from one
# pinned log block and pass OnyxRegistry.transcriptComplete. It writes nothing.
#
# Env: FOUNDRY_BIN (~/.foundry/bin), ONYX_L2_RPC_PORT (8547; the in-phase
# anvil uses the next port), ONYX_L2_ONLY=daily (NS3: only the USGS and NCEI
# rounds, so the Treasury's and SEC's pinned summaries and draws stay as they are).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
PORT=${ONYX_L2_RPC_PORT:-8547}
RPC=http://127.0.0.1:$PORT
work=$(mktemp -d)
anvil_pid=""
anvil2_pid=""
cleanup() {
  [ -n "$anvil_pid" ] && kill "$anvil_pid" 2>/dev/null || true
  [ -n "$anvil2_pid" ] && kill "$anvil2_pid" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
castw() { (cd "$work" && ETH_RPC_URL=$RPC "$FOUNDRY_BIN/cast" "$@"); }

(cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/build.log" 2>&1) \
  || { tail -20 "$work/build.log"; exit 1; }
artifact=$REPO/contracts/out/OnyxRegistry.sol/OnyxRegistry.json

"$FOUNDRY_BIN/anvil" --port "$PORT" --silent &
anvil_pid=$!

# anvil's public dev accounts 0 (coordinator) and 1 (registry owner).
COORD=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266
OWNER=0x70997970C51812dc3A010C7d01b50e0d17dc79C8
# deploy_registry: on the anvil at $RPC, a stub ledger and OnyxRegistry; sets REG.
# The stub ledger answers `coordinatorEpoch()` (selector 0xd7d85a57) with 0
# and every other call with the coordinator (the registry only asks those two
# on these paths): selector == epoch ? RETURN 32 zero bytes : PUSH20 coord,
# MSTORE at 0, RETURN 32.
deploy_registry() {
  for _ in $(seq 50); do castw chain-id >/dev/null 2>&1 && break; sleep 0.1; done
  [ "$(castw chain-id)" = 31337 ] || { echo "not a local anvil"; exit 1; }
  LEDGER=0x00000000000000000000000000000000000c0de1
  castw rpc anvil_setCode "$LEDGER" \
    "0x60003560e01c63d7d85a5714602c5773${COORD#0x}60005260206000f35b60206000f3" >/dev/null
  code=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["bytecode"]["object"])' "$artifact")
  ctor=$(castw abi-encode "c(address,address)" "$OWNER" "$LEDGER")
  REG=$(castw send --unlocked --from "$OWNER" --create "${code}${ctor#0x}" --json \
    | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["contractAddress"])')
}
deploy_registry
echo "OnyxRegistry at $REG on the local anvil (chain 31337)"

chain=(--rpc "$RPC" --registry "$REG" --coordinator "$COORD" --owner "$OWNER" --cast "$FOUNDRY_BIN/cast")
ONLY=${ONYX_L2_ONLY:-all}
case "$ONLY" in all | daily) ;; *) echo "ONYX_L2_ONLY must be all or daily"; exit 1 ;; esac
if [ "$ONLY" = all ]; then
cargo run -q -p onyx-source-fiscaldata --example level2-round -- "${chain[@]}" \
  --book "$work/treasury-book.json" \
  --draw-out adapters/source-fiscaldata/fixtures/debt-to-penny-2026-10-08/draw.json \
  --out apps/web/static/level2/debt-to-penny.json
mkdir -p "$work/draws"
cargo run -q -p onyx-source-edgar --example level2-round -- "${chain[@]}" \
  --book "$work/edgar-book.json" --draw-out-dir "$work/draws" \
  --out apps/web/static/level2/sec-holdings.json
cp "$work/draws/mstr.json" adapters/source-edgar/fixtures/mstr-2026-03-31/draw.json
cp "$work/draws/riot.json" adapters/source-edgar/fixtures/riot-2026-03-31/draw.json
fi
# NS3: the daily sources on the shared runner (ADR-0027 amendment 17).
cargo run -q -p onyx-source-usgs --example level2-round -- "${chain[@]}" \
  --book "$work/usgs-book.json" \
  --draw-out adapters/source-usgs/fixtures/quakes-m5-2026-07-08/draw.json \
  --out apps/web/static/level2/usgs-quakes.json
cargo run -q -p onyx-source-ncei --example level2-round -- "${chain[@]}" \
  --book "$work/ncei-book.json" \
  --draw-out adapters/source-ncei/fixtures/tmax-usw00003017-2026-09-01/draw.json \
  --out apps/web/static/level2/ncei-tmax.json

[ "$ONLY" = daily ] && { echo "LEVEL-2 REHEARSAL PASSED (daily sources only)"; exit 0; }

# REVLOG-R3-4: the in-phase round, on a second anvil whose clock starts one
# second into the recorded Treasury window (RECORDED_WINDOW_START).
# Built first: the anvil's clock runs from its start, and the commit phase
# ends 360 s after the window opens.
cargo build -q -p onyx-source-fiscaldata --example level2-inphase
PORT2=$((PORT + 1))
RPC=http://127.0.0.1:$PORT2
"$FOUNDRY_BIN/anvil" --port "$PORT2" --silent --timestamp $((1791696360 + 1)) &
anvil2_pid=$!
deploy_registry
echo "OnyxRegistry at $REG on the in-phase anvil (clock in the round's window)"
cargo run -q -p onyx-source-fiscaldata --example level2-inphase -- \
  --rpc "$RPC" --registry "$REG" --coordinator "$COORD" --owner "$OWNER" --cast "$FOUNDRY_BIN/cast"
echo "LEVEL-2 REHEARSAL PASSED"
