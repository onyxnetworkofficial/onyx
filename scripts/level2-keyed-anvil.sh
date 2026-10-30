#!/usr/bin/env bash
# CL4 (ADR-0054): keyed contributions with agreement, end to end on a LOCAL anvil (chain id
# 31337), with a loopback fake keyed provider and FAKE keys. Never sends a Base Sepolia
# transaction and never calls a third party.
#
# Usage: scripts/level2-keyed-anvil.sh   (about 6 minutes: one real-time round of 2 minutes)
#
# What it shows:
#  1. Ten key holders, each with its OWN fake key in its OWN onyx-contrib vault (encrypted file
#     vault, a passphrase each), take seats with `onyx-contrib keyed join` (the terms shown and
#     accepted; the seat holds nothing about the key). Holders 1 and 10 declare one operator.
#  2. One holder alone: the coordinator runs no round and states level 4, "stated by one key
#     holder".
#  3. The round: the keyed snapshot keeps 9 seats (one per operator), 7 are drawn; each drawn
#     holder's `onyx-contrib keyed observe` hands its key from the vault to the observer on
#     stdin; the observer fetches the fake provider with it at a private slot, posts its own
#     commit on chain and spools its reveal. The fake provider answers holders 3, 6 and 8 with
#     other values (it targets their keys): whichever of them are drawn are out-voted
#     (Outlier), and the value settles at level 2 with the keyed evidence.
#  4. Every file the run wrote (logs, stores, the book, seats, pools, vaults, states, the
#     spool), every block, transaction and receipt of the chain, anvil's state dump and every
#     process command line seen during the round are searched for every fake key (as typed,
#     percent-encoded, hex, base64 and a 12-character piece): none may hold any.
#
# Local only: anvil's public dev accounts as observers; no key is read from .env or
# .onyx-testnet. What this does NOT show: independent machines or networks (one host).
#
# Env: FOUNDRY_BIN (~/.foundry/bin), ONYX_L2_RPC_PORT (8552), ONYX_L2_LEAD (seconds until the
# window opens, 40), ONYX_L2_LOG_DIR (logs and outputs; default a temp dir under ~/.cache that
# is removed).
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
PORT=${ONYX_L2_RPC_PORT:-8552}
RPC=http://127.0.0.1:$PORT
LEAD=${ONYX_L2_LEAD:-40}
DEMO=$REPO/fixtures/keyed-demo
MANIFEST=$DEMO/sources/demo.keyed.widget_index/manifest.json
mkdir -p "$HOME/.cache"
work=$(mktemp -d "$HOME/.cache/onyx-cl4-anvil.XXXXXX")
logs=${ONYX_L2_LOG_DIR:-$work/logs}
mkdir -p "$logs" "$work/tmp"
# Every helper's temporary files (cast's work folders included) land here, so they are scanned
# and removed with the rest.
export TMPDIR=$work/tmp
anvil_pid=""
pids=()
cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  [ -n "$anvil_pid" ] && kill "$anvil_pid" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
castw() { (cd "$work" && ETH_RPC_URL=$RPC "$FOUNDRY_BIN/cast" "$@"); }

(cd "$work" && "$FOUNDRY_BIN/forge" build --root "$REPO/contracts" >"$work/build.log" 2>&1) \
  || { tail -20 "$work/build.log"; exit 1; }
artifact=$REPO/contracts/out/OnyxRegistry.sol/OnyxRegistry.json
cargo build -q -p onyx-level2 --example level2-keyed
bin=$work/bin
mkdir "$bin"
cp "${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/level2-keyed" "$bin/"
lk() { local mode=$1; shift; "$bin/level2-keyed" "$mode" --manifest "$MANIFEST" --repo "$DEMO" "$@"; }
pin=$(lk pin)
echo "keyed manifest: ${pin%%=*}, template ${pin#*=}"

# 1. FAKE keys, in this shell's memory only (never echoed, never written by this script).
keys=()
for _ in $(seq 10); do keys+=("FAKE-cl4-$(python3 -I -c 'import secrets; print(secrets.token_hex(8))')+k/$(python3 -I -c 'import secrets; print(secrets.token_hex(3))')"); done
# The fake provider knows each key's Keccak-256 only. Holders 3, 6 and 8 get other values.
mkdir -m 700 "$work/provider"
printf '%s\n' "${keys[@]}" | node --input-type=module -e "
import { keccak256 } from '$REPO/apps/contrib/src/keccak.mjs';
let s = ''; process.stdin.on('data', (c) => (s += c)).on('end', () => {
  const odd = { 3: '999.9', 6: '1000.0', 8: '1001.0' };
  const out = s.trim().split('\n').map((k, i) => ({ key_keccak: keccak256(k), value: odd[i + 1] ?? '1234.5' }));
  process.stdout.write(JSON.stringify(out));
});" >"$work/provider/answers.json"
lk provider --answers "$work/provider/answers.json" --port-file "$work/provider/port" \
  2>"$logs/provider.log" &
pids+=($!)
for _ in $(seq 100); do [ -s "$work/provider/port" ] && break; sleep 0.1; done
pport=$(cat "$work/provider/port")
echo "fake keyed provider on 127.0.0.1:$pport (keys by hash only)"

# 2. A local anvil and the registry.
"$FOUNDRY_BIN/anvil" --port "$PORT" --silent --accounts 14 --block-time 1 \
  --config-out "$work/accounts.json" &
anvil_pid=$!
for _ in $(seq 50); do castw chain-id >/dev/null 2>&1 && break; sleep 0.2; done
[ "$(castw chain-id)" = 31337 ] || { echo "not a local anvil" >&2; exit 1; }
COORD=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266
OWNER=0x70997970C51812dc3A010C7d01b50e0d17dc79C8
REPORTER=0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC
LEDGER=0x00000000000000000000000000000000000c0de1
castw rpc anvil_setCode "$LEDGER" \
  "0x60003560e01c63d7d85a5714602c5773${COORD#0x}60005260206000f35b60206000f3" >/dev/null
code=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["bytecode"]["object"])' "$artifact")
ctor=$(castw abi-encode "c(address,address)" "$OWNER" "$LEDGER")
REG=$(castw send --unlocked --from "$OWNER" --create "${code}${ctor#0x}" --json \
  | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["contractAddress"])')
echo "OnyxRegistry at $REG on the local anvil (chain 31337)"
chain=(--rpc "$RPC" --registry "$REG" --cast "$FOUNDRY_BIN/cast")

# 3. Ten holders (anvil accounts 3..12): a vault each, the key set from stdin, a seat each.
python3 -I - "$work/accounts.json" "$work/holders" <<'PY'
import json, os, secrets, sys
acc = json.load(open(sys.argv[1]))
os.makedirs(sys.argv[2], mode=0o700)
def write(path, text):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    os.write(fd, text.encode()); os.close(fd)
for i in range(1, 11):
    d = os.path.join(sys.argv[2], f"h{i}")
    os.makedirs(d, mode=0o700)
    write(os.path.join(d, "observer.key"), acc["private_keys"][i + 2].removeprefix("0x"))
    write(os.path.join(d, "address"), acc["available_accounts"][i + 2])
write(os.path.join(sys.argv[2], "receipt.key"), secrets.token_hex(32))
PY
pass=()
for i in $(seq 10); do pass+=("vault-pass-$(python3 -I -c 'import secrets; print(secrets.token_hex(12))')"); done
as_holder() {
  local i=$1; shift
  ONYX_CONTRIB_HOME="$work/holders/h$i/config" ONYX_CONTRIB_VAULT=file \
    ONYX_CONTRIB_VAULT_PASSPHRASE="${pass[$((i - 1))]}" ONYX_CONTRIB_KEYED_BIN="$bin/level2-keyed" \
    node "$REPO/apps/contrib/src/cli.mjs" "$@"
}
seat_args=()
for i in $(seq 10); do
  as_holder "$i" provider add widgets --base-url https://keyed.example.org/v1 --query api_key \
    --docs https://keyed.example.org/terms >>"$logs/holders.log" 2>&1
  printf '%s' "${keys[$((i - 1))]}" | as_holder "$i" key set widgets >>"$logs/holders.log" 2>&1
  op=()
  if [ "$i" = 1 ] || [ "$i" = 10 ]; then op=(--operator acme.data); fi
  as_holder "$i" keyed join widgets --manifest "$MANIFEST" --repo "$DEMO" \
    --observer-key "$work/holders/h$i/observer.key" "${op[@]}" --yes \
    >"$logs/join-h$i.log" 2>&1 || { cat "$logs/join-h$i.log"; exit 1; }
  seat_args+=(--seat "$work/holders/h$i/config/keyed/demo.keyed.widget_index.seat.json")
done
echo "10 holders took seats (each with its own key in its own vault; holders 1 and 10 declare operator acme.data)"
now=$(date +%s)
attest=(--bond 1000 --bonded-at $((now - 30 * 86400)) --agreed 5)
lk seats "${seat_args[@]}" "${attest[@]}" >"$work/seats.json"
lk seats --seat "$work/holders/h1/config/keyed/demo.keyed.widget_index.seat.json" "${attest[@]}" \
  >"$work/one-seat.json"

window_start=$((now + LEAD))
record_date=$(date -u -d "@$((window_start - 86400))" +%F)
feed=(--record-date "$record_date" --window-start "$window_start")
mkdir -m 700 "$work/spool"
round=(--coordinator "$COORD" --owner "$OWNER" --reporter "$REPORTER" --receipt-key "$work/holders/receipt.key" --spool "$work/spool")

# 4. One holder alone: no round, level 4, said so.
lk coordinator "${chain[@]}" "${round[@]}" --book "$work/book-one.json" --seats "$work/one-seat.json" \
  "${feed[@]}" >"$logs/one-holder.json" 2>"$logs/one-holder.log"
python3 -I - "$logs/one-holder.json" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
assert s["proof_level"] == 4 and s["standing"] == "stated by one key holder" and s["level2_claim"] is False, s
print(f"one holder: level {s['proof_level']}, {s['standing']} ({s['drawable_seats']} drawable seat, {s['needed_seats']} needed)")
PY

# 5. The keyed round.
lk pool --seats "$work/seats.json" "${feed[@]}" >"$work/pool.json" 2>"$logs/pool.log"
cat "$logs/pool.log"
members=$(python3 -I -c 'import json,sys; print(" ".join(m["address"].lower() for m in json.load(open(sys.argv[1]))))' "$work/pool.json")
started=0
for i in $(seq 10); do
  addr=$(tr 'A-F' 'a-f' <"$work/holders/h$i/address")
  case " $members " in *" $addr "*) ;; *) echo "holder $i: not in this round's snapshot (one seat per operator)"; continue ;; esac
  as_holder "$i" keyed observe widgets --manifest "$MANIFEST" --repo "$DEMO" -- \
    --key-file "$work/holders/h$i/observer.key" --pool "$work/pool.json" --seats "$work/seats.json" \
    --spool "$work/spool" --state "$work/holders/h$i/state" "${feed[@]}" "${chain[@]}" \
    --fixture "127.0.0.1:$pport" >"$logs/observer-h$i.log" 2>&1 &
  pids+=($!)
  started=$((started + 1))
done
echo "$started keyed observers started; the window opens at $window_start (in ${LEAD}s), about 5 minutes in all"
sleep 3
ps -eo args >"$logs/ps-during-round.txt"
lk coordinator "${chain[@]}" "${round[@]}" --book "$work/book.json" --store "$logs/store.json" \
  --new-store --seats "$work/seats.json" --pool "$work/pool.json" "${feed[@]}" \
  >"$logs/round.json" 2>"$logs/coordinator.log" \
  || { echo "the coordinator failed:"; tail -20 "$logs/coordinator.log"; exit 1; }
ps -eo args >>"$logs/ps-during-round.txt"
receipted=0 undrawn=0 failed=0
for p in "${pids[@]:1}"; do
  if wait "$p"; then :; else failed=$((failed + 1)); fi
done
pids=("${pids[0]}")
for f in "$logs"/observer-h*.log; do
  if grep -q "receipt on chain" "$f"; then receipted=$((receipted + 1));
  elif grep -q "not drawn" "$f"; then undrawn=$((undrawn + 1)); fi
done
echo "observers: $receipted receipted on chain, $undrawn not drawn, $failed failed"
python3 -I - "$logs/round.json" "$receipted" "$undrawn" "$failed" "$work/holders" <<'PY'
import json, os, sys
r = json.load(open(sys.argv[1]))
receipted, undrawn, failed = map(int, sys.argv[2:5])
odd = set()
for i in (3, 6, 8):
    odd.add(open(os.path.join(sys.argv[5], f"h{i}", "address")).read().strip().lower())
assert r["outcome"] == "Value(Number(12345))", r["outcome"]
assert r["commits_from_chain"] == 7 and r["reveals"] == 7, r
st = {o["address"].lower(): o["status"] for o in r["observers"]}
drawn_odd = [a for a in st if a in odd]
assert drawn_odd, "at least one targeted holder is drawn (7 of 9)"
assert all(st[a] == "Outlier" for a in drawn_odd), st
agreed = sum(1 for s in st.values() if s == "Agreed")
assert agreed == 7 - len(drawn_odd) and agreed >= 4, st
assert r["level2_claim"] is True and r["proof_level"] == 2
assert r["keyed"]["evidence"].startswith("Level 2"), r["keyed"]
assert (receipted, undrawn, failed) == (7, 2, 0), (receipted, undrawn, failed)
print(f"round: 1234.5 index points at level 2 ({r['keyed']['evidence']}); {agreed}/7 agreed, {len(drawn_odd)} targeted holder(s) out-voted as Outlier; odds that {r['keyed']['colluders']} colluding seats (a third of the pool) win one draw: {r['keyed']['collusion_odds_one_draw_ppm']} ppm")
PY

# Review CL4-R1-3: every seat of the snapshot fetched once (the 2 undrawn ones as cover), so
# the keys that fetch in the window are not the drawn set.
answered=$(grep -o '[0-9]* answered' "$logs/provider.log" | tail -1 | cut -d' ' -f1)
covered=$(grep -l "cover fetch done" "$logs"/observer-h*.log | wc -l)
[ "$answered" = 9 ] && [ "$covered" = 2 ] \
  || { echo "cover fetches: the provider answered $answered requests, $covered undrawn seats fetched (want 9 and 2)"; exit 1; }
echo "cover fetches: the provider answered 9 keys (7 drawn, 2 undrawn as cover); each request with its own key"
# 6. The leak scan: chain data, files, command lines.
python3 -I - "$RPC" "$logs/chain.json" <<'PY'
import json, sys, urllib.request
rpc, out = sys.argv[1], sys.argv[2]
def call(m, p):
    req = urllib.request.Request(rpc, json.dumps({"jsonrpc": "2.0", "id": 1, "method": m, "params": p}).encode(), {"content-type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=30))["result"]
head = int(call("eth_blockNumber", []), 16)
blocks, receipts = [], []
for n in range(head + 1):
    b = call("eth_getBlockByNumber", [hex(n), True])
    blocks.append(b)
    for t in b["transactions"]:
        receipts.append(call("eth_getTransactionReceipt", [t["hash"]]))
state = call("anvil_dumpState", [])
json.dump({"blocks": blocks, "receipts": receipts, "state": state}, open(out, "w"))
print(f"chain data: {head + 1} blocks, {len(receipts)} transactions, state dump of {len(state)} hex chars")
PY
cat >"$work/scan.py" <<'PY'
import base64, gzip, json, os, sys, urllib.parse
skip = sys.argv[1]
roots = sys.argv[2:]
keys = sys.stdin.read().split()
assert len(keys) == 10, len(keys)
forms = []
for k in keys:
    b = k.encode()
    forms += [b, urllib.parse.quote(k, safe="").encode(), b.hex().encode(), base64.b64encode(b).rstrip(b"="), b[:12]]
files, seen = 0, set()
for top in roots:
    for root, _, names in os.walk(top):
        if root.startswith(skip):
            continue
        for n in names:
            p = os.path.realpath(os.path.join(root, n))
            if p in seen or not os.path.isfile(p) or p.endswith("scan.py"):
                continue
            seen.add(p)
            data = open(p, "rb").read()
            texts = [data]
            if n == "chain.json":
                raw = bytes.fromhex(json.loads(data)["state"].removeprefix("0x"))
                try:
                    raw = gzip.decompress(raw)
                except OSError:
                    pass
                texts.append(raw)
            files += 1
            for t in texts:
                for f in forms:
                    assert f not in t, f"{p} holds a form of a fake key"
print(f"leak scan: {files} files (logs, stores, book, seats, pools, encrypted vaults, states, spool, chain blocks, receipts and state, process lists) hold no form of any of the 10 fake keys")
PY
# The scan's own check: a planted percent-encoded copy of one key must be found.
mkdir "$work/plant"
python3 -I -c 'import sys, urllib.parse; open(sys.argv[1], "w").write("x=" + urllib.parse.quote(sys.stdin.readline().strip(), safe=""))' \
  "$work/plant/log.txt" < <(printf '%s\n' "${keys[4]}")
if python3 -I "$work/scan.py" "$bin" "$work/plant" < <(printf '%s\n' "${keys[@]}") >/dev/null 2>&1; then
  echo "the leak scan missed a planted key" >&2; exit 1
fi
rm -r "$work/plant"
python3 -I "$work/scan.py" "$bin" "$work" "$logs" < <(printf '%s\n' "${keys[@]}")
echo "CL4 KEYED LOCAL DEMO PASSED (anvil, fake keyed provider)"
