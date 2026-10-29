#!/usr/bin/env bash
# North-star loop NL16 rehearsal: an operator on a clean machine joins a level-2 round from
# docs/join.md alone. The "clean machine" is a fresh container from the official ubuntu:26.04
# image with curl, ca-certificates and libudev1 (cast needs it) installed and the join kit copied
# in; inside it only the docs' commands run (`join.sh key`, `join.sh observe`). The round runs
# against a local anvil (chain 31337, never a live chain): four observers on this host and the
# container as the fifth, a pool of exactly five, so the draw takes all five. Sealed reveals: the
# outside observer's commit and reveal both go on chain, no file reaches the coordinator.
# Takes about 10 minutes (the Treasury feed's 300 s window and its phases).
#
# Builds (once, on this host): the join kit with scripts/join-build.sh fiscaldata into
# ONYX_JOIN_KIT (default ~/.cache/onyx-nl16/kit) and the coordinator in the same target folder.
# The image (onyx-join-rehearsal:nl16) and the container (onyx-join-operator) are removed by name
# at the end; ONYX_JOIN_KEEP_IMAGE=1 keeps the image for a rerun.
# Env: FOUNDRY_BIN (~/.foundry/bin), ONYX_JOIN_RPC_PORT (8556), ONYX_L2_LEAD (seconds until the
# window opens, 90), ONYX_L2_RECORD_DATE (default: the newest Treasury record, read once),
# ONYX_JOIN_LOG_DIR (logs; default a folder that is removed), CARGO_TARGET_DIR.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
PORT=${ONYX_JOIN_RPC_PORT:-8556}
RPC=http://127.0.0.1:$PORT
LEAD=${ONYX_L2_LEAD:-90}
KIT=${ONYX_JOIN_KIT:-$HOME/.cache/onyx-nl16/kit}
IMAGE=onyx-join-rehearsal:nl16
NAME=onyx-join-operator
mkdir -p "$HOME/.cache"
work=$(mktemp -d "$HOME/.cache/onyx-join-rehearsal.XXXXXX")
logs=${ONYX_JOIN_LOG_DIR:-$work/logs}
mkdir -p "$logs"
anvil_pid=""
pids=()
cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  [ -n "$anvil_pid" ] && kill "$anvil_pid" 2>/dev/null || true
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  [ "${ONYX_JOIN_KEEP_IMAGE:-0}" = 1 ] || docker rmi "$IMAGE" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT
castw() { (cd "$work" && ETH_RPC_URL=$RPC "$FOUNDRY_BIN/cast" "$@"); }

# 1. The kit (docs/join.md step 1) and the coordinator, built on this host.
scripts/join-build.sh "$KIT" fiscaldata
RUSTFLAGS="--remap-path-prefix=$HOME=/build" CARGO_INCREMENTAL=0 \
  cargo build -q --release -p onyx-source-fiscaldata --example level2-coordinator
coordinator=$work/level2-coordinator
cp "${CARGO_TARGET_DIR:-$REPO/target}/release/examples/level2-coordinator" "$coordinator"
observer=$KIT/bin/level2-observer-fiscaldata

# 2. The operator's clean machine: an official base image, curl, and the kit, nothing else.
mkdir -p "$work/ctx"
cp -r "$KIT" "$work/ctx/kit"
cat >"$work/ctx/Dockerfile" <<'DOCKER'
FROM ubuntu:26.04
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates libudev1 \
 && rm -rf /var/lib/apt/lists/* && useradd -m onyxop
COPY --chown=onyxop:onyxop kit/ /home/onyxop/onyx-join/
USER onyxop
WORKDIR /home/onyxop
DOCKER
docker build -q -t "$IMAGE" "$work/ctx" >/dev/null

# 3. A local anvil with OnyxRegistry (as scripts/level2-sealed.sh, with 2 s blocks: five observers
# polling one anvil can slow the coordinator past the draw lead of 3 blocks, BeaconNotFuture; and a
# base fee of 0, so anvil's fixed 1 gwei tip stays inside the outside observer's 1 gwei fee cap).
(cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/build.log" 2>&1) \
  || { tail -20 "$work/build.log"; exit 1; }
artifact=$REPO/contracts/out/OnyxRegistry.sol/OnyxRegistry.json
"$FOUNDRY_BIN/anvil" --port "$PORT" --silent --block-time 2 --accounts 10 \
  --base-fee 0 \
  --config-out "$work/accounts.json" &
anvil_pid=$!
for _ in $(seq 50); do castw chain-id >/dev/null 2>&1 && break; sleep 0.2; done
[ "$(castw chain-id)" = 31337 ] || { echo "not a local anvil"; exit 1; }
COORD=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266
OWNER=0x70997970C51812dc3A010C7d01b50e0d17dc79C8
REPORTER=0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC
FUNDER=0xa0Ee7A142d267C1f36714E4a8F75612F20a79720
LEDGER=0x00000000000000000000000000000000000c0de1
castw rpc anvil_setCode "$LEDGER" \
  "0x60003560e01c63d7d85a5714602c5773${COORD#0x}60005260206000f35b60206000f3" >/dev/null
code=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["bytecode"]["object"])' "$artifact")
ctor=$(castw abi-encode "c(address,address)" "$OWNER" "$LEDGER")
REG=$(castw send --unlocked --from "$OWNER" --create "${code}${ctor#0x}" --json \
  | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["contractAddress"])')
echo "OnyxRegistry at $REG on the local anvil (chain 31337)"

# 4. The operator makes a key in the container (docs step 2) and sends only the address.
docker run -d --name "$NAME" --network host "$IMAGE" sleep infinity >/dev/null
outside=$(docker exec "$NAME" onyx-join/join.sh key onyx-join/observer.key | tail -1)
echo "$outside" | grep -Eq '^0x[0-9a-f]{40}$' || { echo "join.sh key printed no address: $outside"; exit 1; }
[ "$(docker exec "$NAME" stat -c %a onyx-join/observer.key)" = 600 ] || { echo "the key is not 0600"; exit 1; }
echo "outside observer $outside (key made and kept in the container)"
castw send --unlocked --from "$FUNDER" "$outside" --value 0.01ether >/dev/null

# Four observers on this host (anvil accounts 3..6), the receipt and reveal keys, the pool of five.
mkdir -m 700 "$work/keys"
python3 -I - "$work/accounts.json" "$work/keys" "$outside" <<'PY'
import json, os, secrets, sys
acc = json.load(open(sys.argv[1]))
keys, addrs = acc["private_keys"], acc["available_accounts"]
out, outside = sys.argv[2], sys.argv[3]
def write(name, hexkey):
    fd = os.open(os.path.join(out, name), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    os.write(fd, hexkey.removeprefix("0x").encode()); os.close(fd)
pool = []
for i in range(3, 7):
    write(f"observer-{i}.key", keys[i])
    pool.append({"address": addrs[i], "bond": 1000})
pool.append({"address": outside, "bond": 1000})
json.dump(pool, open(os.path.join(out, "pool.json"), "w"))
write("receipt.key", secrets.token_hex(32))
write("reveal.key", secrets.token_hex(32))
PY

date_of=${ONYX_L2_RECORD_DATE:-}
if [ -z "$date_of" ]; then
  date_of=$(curl -sS --fail --proto =https -m 20 -A onyx-dev \
    "https://api.fiscaldata.treasury.gov/services/api/fiscal_service/v2/accounting/od/debt_to_penny?sort=-record_date&page%5Bsize%5D=1&fields=record_date" \
    | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["data"][0]["record_date"])')
fi
ws=$(( $(date +%s) + LEAD ))
feed=(--record-date "$date_of" --window-start "$ws")
chain=(--rpc "$RPC" --registry "$REG" --cast "$FOUNDRY_BIN/cast")
mkdir -m 700 "$work/spool"

# 5. The coordinator announces the round (round.json) and runs it.
"$coordinator" "${chain[@]}" --coordinator "$COORD" --owner "$OWNER" --reporter "$REPORTER" \
  --book "$work/book.json" --store "$work/store.json" --new-store --pool "$work/keys/pool.json" \
  --spool "$work/spool" --receipt-key "$work/keys/receipt.key" --reveal-key "$work/keys/reveal.key" \
  --announce "$work/round.json" "${feed[@]}" >"$logs/round.json" 2>"$logs/coordinator.log" &
coord_pid=$!
pids+=("$coord_pid")
for _ in $(seq 120); do [ -s "$work/round.json" ] && break; sleep 0.5; done
[ -s "$work/round.json" ] || { echo "no announcement:"; tail -20 "$logs/coordinator.log"; exit 1; }
docker cp "$work/round.json" "$NAME:/home/onyxop/round.json"

# 6. The operator observes (docs step 3); the host's four observers run as in the other rehearsals.
docker exec "$NAME" onyx-join/join.sh observe round.json onyx-join/observer.key \
  >"$logs/outside.log" 2>&1 &
out_pid=$!
pids+=("$out_pid")
host_pids=()
for i in 3 4 5 6; do
  "$observer" --key-file "$work/keys/observer-$i.key" --pool "$work/keys/pool.json" \
    --spool "$work/spool" --state "$work/state-$i" "${feed[@]}" "${chain[@]}" \
    >"$logs/observer-$i.log" 2>&1 &
  host_pids+=($!)
  pids+=($!)
done
echo "round of $date_of: 4 host observers and the outside one started; the window opens in $(( ws - $(date +%s) )) s"

wait "$coord_pid" || { echo "the coordinator failed:"; tail -20 "$logs/coordinator.log"; exit 1; }
cat "$logs/round.json"
wait "$out_pid" || { echo "the outside observer failed:"; tail -20 "$logs/outside.log"; exit 1; }
grep -q "sealed post on chain" "$logs/outside.log" || { echo "the outside observer posted nothing:"; tail -20 "$logs/outside.log"; exit 1; }
for idx in "${!host_pids[@]}"; do
  wait "${host_pids[$idx]}" || { echo "host observer $((idx + 3)) failed"; tail -5 "$logs/observer-$((idx + 3)).log"; exit 1; }
done
pids=()
python3 -I - "$logs/round.json" "$outside" <<'PY'
import json, sys
r, outside = json.load(open(sys.argv[1])), sys.argv[2].lower()
assert r["outcome"].startswith("Value(Number("), r["outcome"]
assert r["commits_from_chain"] == 5 and r["reveals"] == 5 and r["sealed_posts"] == 5, r
assert r["level2_claim"] is True, r
obs = {o["address"].lower(): o["status"] for o in r["observers"]}
assert obs.get(outside) == "Agreed", obs
assert all(s == "Agreed" for s in obs.values()), obs
print(f"outside observer {outside}: drawn, committed and revealed on chain, Agreed")
PY
echo "JOIN REHEARSAL PASSED"
