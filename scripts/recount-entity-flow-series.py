#!/usr/bin/env python3
# Independent recount of per-block entity flows (Python, not the Rust recipe), for the
# btc-entity-flow-series golden test and the website export.
# Usage: python3 -I scripts/recount-entity-flow-series.py fixtures/btc/blocks-921080-921082.json <labels.json>
#    or: python3 -I scripts/recount-entity-flow-series.py fixtures-live/node <labels.json> 970543 970566
# Prints one JSON list: [{"entity", "height", "received_sats", "sent_sats", "net_sats", "tx_count"}, ...]
# sorted by (entity, height), only (entity, block) pairs the entity was touched in. Amounts are
# decimal strings. Address decoding is its own code, not a port of the recipe's.
import gzip, json, os, sys
from decimal import Decimal

CH = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"
B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def polymod(v):
    g = [0x3B6A57B2, 0x26508E6D, 0x1EA119FA, 0x3D4233DD, 0x2A1462B3]
    c = 1
    for x in v:
        b = c >> 25
        c = (c & 0x1FFFFFF) << 5 ^ x
        for i in range(5):
            c ^= g[i] if (b >> i) & 1 else 0
    return c


def bech32_script(a):
    hrp, data = a[:2], [CH.index(c) for c in a[3:]]
    assert hrp == "bc"
    chk = polymod([ord(x) >> 5 for x in hrp] + [0] + [ord(x) & 31 for x in hrp] + data)
    ver = data[0]
    assert chk == (1 if ver == 0 else 0x2BC830A3)
    acc = bits = 0
    prog = []
    for v in data[1:-6]:
        acc = (acc << 5) | v
        bits += 5
        while bits >= 8:
            bits -= 8
            prog.append((acc >> bits) & 255)
    return bytes([0 if ver == 0 else 0x50 + ver, len(prog)]) + bytes(prog)


def b58_script(a):
    n = 0
    for c in a:
        n = n * 58 + B58.index(c)
    raw = n.to_bytes(25, "big")
    ver, h = raw[0], raw[1:21]
    if ver == 0:
        return b"\x76\xa9\x14" + h + b"\x88\xac"
    if ver == 5:
        return b"\xa9\x14" + h + b"\x87"
    raise ValueError(a)


def script(addr):
    a = addr.split(":", 1)[1]
    return (bech32_script(a) if a.startswith("bc1") else b58_script(a)).hex()


def load_blocks(src, lo=None, hi=None):
    """[(height, [(vin [(script, sats)], vout [(script, sats)])])]"""
    if os.path.isdir(src):
        # Node mode: getblock-3 JSON, independent of the Rust raw-block parser.
        out = []
        sats = lambda v: int(Decimal(str(v)) * 100000000)
        for h in range(lo, hi + 1):
            # Exact decimals: never route BTC amounts through binary floats (SER-R1-F5).
            d = json.load(gzip.open(os.path.join(src, f"{h}.v3.json.gz")), parse_float=Decimal)
            txs = []
            for t in d["tx"]:
                vin = [(i["prevout"]["scriptPubKey"]["hex"], sats(i["prevout"]["value"])) for i in t["vin"] if "prevout" in i]
                vout = [(o["scriptPubKey"]["hex"], sats(o["value"])) for o in t["vout"]]
                txs.append((vin, vout))
            out.append((h, txs))
        return out
    doc = json.load(open(src))
    out = []
    for b in doc["blocks"]:
        txs = []
        for t in b["txs"]:
            vin = [(v[2], v[3]) for v in t.get("vin", []) if len(v) >= 4]
            vout = [(v[0], v[1]) for v in t["vout"]]
            txs.append((vin, vout))
        out.append((b["height"], txs))
    return out


def series(blocks, labels):
    # Refuse exactly what the recipe refuses (SER-R2-F3): an entity id with a
    # zero byte (BAD_LABELS) and one script labelled with two different
    # entities (CONFLICTING_LABELS). An empty entity id is accepted, as the
    # recipe accepts it (the host's label checks refuse it earlier).
    owner = {}
    for l in labels:
        e = l["entity"]
        if "\x00" in e:
            raise SystemExit(f"entity id with a zero byte for {l['address']}")
        s = script(l["address"])
        if owner.setdefault(s, e) != e:
            raise SystemExit(f"conflicting labels for {l['address']}: {owner[s]} vs {e}")
    acc = {}
    for h, txs in blocks:
        for vin, vout in txs:
            touched = set()
            for s, v in vin:
                e = owner.get(s)
                if e is not None:
                    acc.setdefault((e, h), [0, 0, 0])[1] += v
                    touched.add(e)
            for s, v in vout:
                e = owner.get(s)
                if e is not None:
                    acc.setdefault((e, h), [0, 0, 0])[0] += v
                    touched.add(e)
            for e in touched:
                acc[(e, h)][2] += 1
    return [
        {"entity": e, "height": h, "received_sats": str(r), "sent_sats": str(s), "net_sats": str(r - s), "tx_count": n}
        for (e, h), (r, s, n) in sorted(acc.items())
    ]


if __name__ == "__main__":
    src, labels_path = sys.argv[1], sys.argv[2]
    lo = int(sys.argv[3]) if len(sys.argv) > 3 else None
    hi = int(sys.argv[4]) if len(sys.argv) > 4 else None
    labels = json.load(open(labels_path))["labels"]
    print(json.dumps(series(load_blocks(src, lo, hi), labels)))
