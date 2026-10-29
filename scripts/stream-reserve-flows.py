#!/usr/bin/env python3
"""Per-address BTC flows over a block range, streamed from a node (ADR-0034, live checks).

Usage: python3 -I scripts/stream-reserve-flows.py <ssh-host> <addresses.txt> <from> <to> <out.jsonl> [workers]

Reads each block with `bitcoin-cli getblock <hash> 3` over ssh (read-only; nothing is written on
the node) and keeps only what touches the listed addresses: one JSON line per block,
{"height", "hash", "prev", "flows": {address: [received_sats, sent_sats]}}, in height order.
Blocks are not stored, so long ranges (weeks) fit in memory and on disk. Independent of the Rust
adapter and recipes: addresses are matched by the scriptPubKey this script derives itself
(its own Bech32/Base58 decoder), amounts are Decimal BTC to integer satoshis.

The output is used by scripts/recount-entity-reserve.py to roll a UTXO scan back to an earlier
height (base snapshot derivation and the OKX proof-of-reserves comparison).
"""
import gzip
import json
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
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
    data = [CH.index(c) for c in a[3:]]
    chk = polymod([ord(x) >> 5 for x in "bc"] + [0] + [ord(x) & 31 for x in "bc"] + data)
    ver = data[0]
    if chk != (1 if ver == 0 else 0x2BC830A3):
        raise ValueError(f"bad checksum: {a}")
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
    import hashlib

    n = 0
    for c in a:
        n = n * 58 + B58.index(c)
    raw = n.to_bytes(25, "big")
    if hashlib.sha256(hashlib.sha256(raw[:21]).digest()).digest()[:4] != raw[21:]:
        raise ValueError(f"bad checksum: {a}")
    ver, h = raw[0], raw[1:21]
    if ver == 0:
        return b"\x76\xa9\x14" + h + b"\x88\xac"
    if ver == 5:
        return b"\xa9\x14" + h + b"\x87"
    raise ValueError(a)


def script_hex(addr):
    return (bech32_script(addr) if addr.startswith("bc1") else b58_script(addr)).hex()


def sats(v):
    return int(Decimal(str(v)) * 100000000)


def block(host, h, scripts):
    cmd = f"bitcoin-cli getblock $(bitcoin-cli getblockhash {h}) 3 | gzip -1 -c"
    raw = subprocess.run(["ssh", "-o", "BatchMode=yes", host, cmd], capture_output=True, check=True).stdout
    d = json.loads(gzip.decompress(raw))
    if d["height"] != h:
        raise SystemExit(f"node returned height {d['height']} for {h}")
    flows = {}
    for t in d["tx"]:
        for i in t["vin"]:
            p = i.get("prevout")
            if p:
                a = scripts.get(p["scriptPubKey"]["hex"])
                if a:
                    flows.setdefault(a, [0, 0])[1] += sats(p["value"])
        for o in t["vout"]:
            a = scripts.get(o["scriptPubKey"]["hex"])
            if a:
                flows.setdefault(a, [0, 0])[0] += sats(o["value"])
    return {"height": h, "hash": d["hash"], "prev": d.get("previousblockhash"), "flows": flows}


def main():
    host, addr_file, lo, hi, out = sys.argv[1:6]
    workers = int(sys.argv[6]) if len(sys.argv) > 6 else 6
    lo, hi = int(lo), int(hi)
    addrs = [a.strip() for a in open(addr_file) if a.strip()]
    scripts = {script_hex(a): a for a in addrs}
    if len(scripts) != len(addrs):
        raise SystemExit("two addresses give the same script")
    done = {}
    try:
        for line in open(out):
            r = json.loads(line)
            done[r["height"]] = r
    except FileNotFoundError:
        pass
    todo = [h for h in range(lo, hi + 1) if h not in done]
    with ThreadPoolExecutor(workers) as ex, open(out, "a") as f:
        for n, r in enumerate(ex.map(lambda h: block(host, h, scripts), todo)):
            f.write(json.dumps(r, sort_keys=True) + "\n")
            done[r["height"]] = r
            if n % 100 == 0:
                print(f"{r['height']} ({n + 1}/{len(todo)})", file=sys.stderr, flush=True)
    # Chain check: every block links to the one before it.
    for h in range(lo + 1, hi + 1):
        if done[h]["prev"] != done[h - 1]["hash"]:
            raise SystemExit(f"block {h} does not extend block {h - 1}")
    print(f"{hi - lo + 1} blocks {lo}-{hi}, chain links checked", file=sys.stderr)


if __name__ == "__main__":
    main()
