"""Fetch Bitcoin blocks + prevouts from a public Esplora API and verify them.

Verified locally against the raw block bytes:
  - block hash == dsha256(header), and header hash <= target from nBits (proof of work)
  - each header's prev-hash == previous block's hash (the range is one chain)
  - tip hash matches a second, independent API
  - header merkle root == merkle(txids), with no duplicate txids (CVE-2012-2459)
  - txid order, input count, input outpoints, outputs match the raw block
  - coinbase derived from the raw tx (index 0, null outpoint), never from the API
  - for non-coinbase txs, sum(prevout values) >= sum(outputs) (fee >= 0)
NOT verified: prevout script/value beyond the fee check (asserted by the API).
The fixture stores headers and outpoints so all of the above except the prevouts can be
rechecked offline (adapters/source-btc does this on load).
"""
import hashlib, json, sys, time, urllib.request

API = "https://mempool.space/api"
SECOND_API = "https://blockstream.info/api"

def get(path, raw=False, base=API):
    for attempt in range(5):
        try:
            with urllib.request.urlopen(base + path, timeout=60) as r:
                b = r.read()
                return b if raw else json.loads(b)
        except Exception as e:
            time.sleep(2 + attempt * 3)
    raise SystemExit("fetch failed: " + path)

def dsha(b): return hashlib.sha256(hashlib.sha256(b).digest()).digest()

def varint(b, i):
    n = b[i]
    if n < 0xfd: return n, i + 1
    if n == 0xfd: return int.from_bytes(b[i+1:i+3], 'little'), i + 3
    if n == 0xfe: return int.from_bytes(b[i+1:i+5], 'little'), i + 5
    return int.from_bytes(b[i+1:i+9], 'little'), i + 9

def parse_tx(b, i):
    start = i
    ver = b[i:i+4]; i += 4
    segwit = b[i] == 0 and b[i+1] == 1
    if segwit: i += 2
    body_start = i
    nin, i = varint(b, i); ins = []
    for _ in range(nin):
        prev = b[i:i+32][::-1].hex(); vout = int.from_bytes(b[i+32:i+36], 'little'); i += 36
        sl, i = varint(b, i); i += sl + 4
        ins.append((prev, vout))
    nout, i = varint(b, i); outs = []
    for _ in range(nout):
        val = int.from_bytes(b[i:i+8], 'little'); i += 8
        sl, i = varint(b, i); outs.append((b[i:i+sl].hex(), val)); i += sl
    body_end = i
    if segwit:
        for _ in range(nin):
            n, i = varint(b, i)
            for _ in range(n):
                l, i = varint(b, i); i += l
    lock = b[i:i+4]; i += 4
    txid = dsha(ver + b[body_start:body_end] + lock)[::-1].hex()
    return txid, ins, outs, i

def merkle(ids):
    h = [bytes.fromhex(t)[::-1] for t in ids]
    while len(h) > 1:
        if len(h) % 2: h.append(h[-1])
        h = [dsha(h[k] + h[k+1]) for k in range(0, len(h), 2)]
    return h[0]

def target(bits):
    exp, mant = bits >> 24, bits & 0xFFFFFF
    return mant << (8 * (exp - 3))

def fetch_block(height, prev_hash):
    bh = get(f"/block-height/{height}", raw=True).decode().strip()
    raw = get(f"/block/{bh}/raw", raw=True)
    header = raw[:80]
    assert dsha(header)[::-1].hex() == bh, "header hash mismatch"
    bits = int.from_bytes(header[72:76], "little")
    assert int.from_bytes(dsha(header), "little") <= target(bits), "insufficient proof of work"
    if prev_hash is not None:
        assert header[4:36][::-1].hex() == prev_hash, "prev-hash link broken"
    n, i = varint(raw, 80); txs = []
    for _ in range(n):
        txid, ins, outs, i = parse_tx(raw, i); txs.append((txid, ins, outs))
    assert i == len(raw), "trailing bytes"
    assert len({t[0] for t in txs}) == len(txs), "duplicate txid"
    assert merkle([t[0] for t in txs]) == raw[36:68], "merkle root mismatch"
    api_txs = []
    for start in range(0, n, 25):
        api_txs += get(f"/block/{bh}/txs/{start}")
        time.sleep(0.25)
    assert [t["txid"] for t in api_txs] == [t[0] for t in txs], "txid order mismatch"
    out = []
    for k, ((txid, ins, outs), at) in enumerate(zip(txs, api_txs)):
        cb = len(ins) == 1 and ins[0] == ("00" * 32, 0xFFFFFFFF)
        assert cb == (k == 0), "coinbase must be exactly the first tx"
        assert len(ins) == len(at["vin"]), "input count mismatch"
        vin = []
        if not cb:
            for (prev, vo), v in zip(ins, at["vin"]):
                assert v["txid"] == prev and v["vout"] == vo, "outpoint mismatch"
                vin.append([prev, vo, v["prevout"]["scriptpubkey"], v["prevout"]["value"]])
            assert sum(v[3] for v in vin) >= sum(val for _, val in outs), "negative fee"
        vout = [[s, val] for s, val in outs]
        assert vout == [[o["scriptpubkey"], o["value"]] for o in at["vout"]], "output mismatch"
        out.append({"txid": txid, "vin": vin, "vout": vout})
    print(f"block {height} {bh}: {n} txs verified", file=sys.stderr)
    return {"height": height, "hash": bh, "header": header.hex(), "txs": out}

if __name__ == "__main__":
    lo, hi, dest = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
    blocks, prev = [], None
    for h in range(lo, hi + 1):
        b = fetch_block(h, prev)
        blocks.append(b)
        prev = b["hash"]
    second = get(f"/block-height/{hi}", raw=True, base=SECOND_API).decode().strip()
    assert second == prev, "tip hash differs between independent APIs"
    doc = {"chain": "btc.mainnet", "source": API, "second_source": SECOND_API,
           "verified": "pow, prev-hash chain, tip vs second API, merkle root, no duplicate txids, "
                       "txids, input counts, outpoints, coinbase from raw, outputs, fee >= 0; "
                       "prevout script/value asserted by source",
           "blocks": blocks}
    with open(dest, "w") as f:
        json.dump(doc, f, separators=(",", ":"), sort_keys=True)
