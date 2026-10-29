#!/usr/bin/env python3
# Independent recount of blocks per mining pool (Python, not the Rust recipe or adapter), for the
# btc-pool-blocks golden test and the website export (ADR-0032).
# Usage: python3 -I scripts/recount-pool-blocks.py <pool-allowlist.json> proofs <coinbase-proofs.json>
#    or: python3 -I scripts/recount-pool-blocks.py <pool-allowlist.json> raw <dir> <from> <to>
# Prints one JSON object {pool name or "unknown": blocks}, keys sorted. Rules, written from
# docs/spec/recipes.md (R-POOL-1, R-POOL-2), not ported from the Rust code:
# - a list pool is named only with an http(s) website that is not on github.com and not one of the
#   excluded ids (POOL-R1-F4 table), and only with at least one non-empty tag or one address;
# - per block: the lowest-id named pool paid by the coinbase's largest-value output (first on ties,
#   zero-value outputs never), else the named pool whose tag occurs earliest in the coinbase
#   scriptSig (ties: longer tag, then lower id; POOL-R2-F3), else "unknown".
import hashlib, json, sys

EXCLUDED = {21, 23, 30, 40, 52, 53, 65, 66, 75, 115, 118, 131, 154}
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
    a = a.lower()
    hrp, data = a[:2], [CH.index(c) for c in a[3:]]
    assert hrp == "bc" and a[2] == "1"
    chk = polymod([ord(x) >> 5 for x in hrp] + [0] + [ord(x) & 31 for x in hrp] + data)
    ver = data[0]
    assert chk == (1 if ver == 0 else 0x2BC830A3), a
    acc = bits = 0
    prog = []
    for v in data[1:-6]:
        acc = (acc << 5) | v
        bits += 5
        while bits >= 8:
            bits -= 8
            prog.append((acc >> bits) & 0xFF)
    return bytes([0 if ver == 0 else 0x50 + ver, len(prog)] + prog)


def b58_script(a):
    n = 0
    for c in a:
        n = n * 58 + B58.index(c)
    raw = n.to_bytes(25, "big")
    assert hashlib.sha256(hashlib.sha256(raw[:21]).digest()).digest()[:4] == raw[21:], a
    ver, h = raw[0], raw[1:21]
    if ver == 0:
        return b"\x76\xa9\x14" + h + b"\x88\xac"
    assert ver == 5, a
    return b"\xa9\x14" + h + b"\x87"


def script_of(a):
    return bech32_script(a) if a.lower().startswith("bc1") else b58_script(a)


def table(path):
    out = []
    for p in sorted(json.load(open(path)), key=lambda p: p["id"]):
        link = p.get("link") or ""
        if not link.startswith(("http://", "https://")) or "github.com" in link or p["id"] in EXCLUDED:
            continue
        tags = [t.encode() for t in p["tags"] if t]
        scripts = {script_of(a) for a in p["addresses"]}
        if tags or scripts:
            out.append((p["name"], tags, scripts))
    return out


def varint(b, i):
    x = b[i]
    if x < 0xFD:
        return x, i + 1
    w = {0xFD: 2, 0xFE: 4, 0xFF: 8}[x]
    return int.from_bytes(b[i + 1 : i + 1 + w], "little"), i + 1 + w


def coinbase(tx):
    """(scriptSig, [(value, output script)]) of a serialized coinbase transaction."""
    i = 4
    if tx[i] == 0 and tx[i + 1] == 1:  # segwit marker and flag
        i += 2
    nin, i = varint(tx, i)
    assert nin == 1 and tx[i : i + 32] == bytes(32)  # a coinbase spends nothing
    i += 36
    n, i = varint(tx, i)
    sig = tx[i : i + n]
    i += n + 4
    nout, i = varint(tx, i)
    outs = []
    for _ in range(nout):
        v = int.from_bytes(tx[i : i + 8], "little")
        i += 8
        n, i = varint(tx, i)
        outs.append((v, bytes(tx[i : i + n])))
        i += n
    return bytes(sig), outs


def height_of(sig):
    n = sig[0]
    return int.from_bytes(sig[1 : 1 + n], "little")


def attribute(t, sig, outs):
    payout = None
    for v, s in outs:
        if v > 0 and (payout is None or v > payout[0]):
            payout = (v, s)
    if payout is not None:
        for name, _, scripts in t:
            if payout[1] in scripts:
                return name
    # Tag: the earliest offset in the scriptSig wins; ties: longer tag, then table order (POOL-R2-F3).
    best = None
    for i, (name, tags, _) in enumerate(t):
        for tag in tags:
            at = sig.find(tag)
            if at >= 0:
                key = (at, -len(tag), i)
                if best is None or key < best[0]:
                    best = (key, name)
    if best is not None:
        return best[1]
    return "unknown"


def main():
    t = table(sys.argv[1])
    blocks = []
    if sys.argv[2] == "proofs":
        for p in json.load(open(sys.argv[3]))["blocks"]:
            blocks.append((p["height"], coinbase(bytes.fromhex(p["coinbase"]))))
    else:
        d, lo, hi = sys.argv[3], int(sys.argv[4]), int(sys.argv[5])
        for h in range(lo, hi + 1):
            b = bytes.fromhex(open(f"{d}/{h}.raw.hex").read().strip())
            _, i = varint(b, 80)  # tx count after the header
            blocks.append((h, coinbase(b[i:])))
    counts = {}
    for h, (sig, outs) in blocks:
        assert height_of(sig) == h, h
        k = attribute(t, sig, outs)
        counts[k] = counts.get(k, 0) + 1
    print(json.dumps(dict(sorted(counts.items()))))


main()
