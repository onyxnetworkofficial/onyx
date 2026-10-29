#!/usr/bin/env python3
"""Build the reserve header chain fixture (ADR-0037, THREATS RES-4).

Usage: python3 -I scripts/build-reserve-headers.py <from> <to> <out.json> <live-dir>... \
           [--fetch] [--own-hashes <flows.jsonl>]...

One 80-byte header per height from..to, in order. A height whose raw block is in one of the
live dirs (`<h>.raw.hex`, our own node's `getblock <hash> 0`, scripts/fetch-btc-node.sh) takes
its header from those bytes. With --fetch, a height missing there is read from mempool.space's
Esplora API and its hash is cross-checked on blockstream.info (the S2 precedent in
docs/ops/data-sources.md: consensus data, any node returns the same bytes). Every header must
hash to its block hash and link to the previous one; the Rust loader
(`reserve::HeaderChain::from_json`) checks proof of work against the pinned floor again.
--own-hashes files (our own node's flows streams, one block per line with "height" and "hash")
must agree with the chain at every height they name.

Output: {"version": 1, "from": <height>, "headers": [<160 hex chars>, ...], "sources": [...]}.
Reproducible: same inputs, same bytes.
"""
import hashlib
import json
import sys
import urllib.request
from pathlib import Path

MEMPOOL = "https://mempool.space/api"
BLOCKSTREAM = "https://blockstream.info/api"


def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": "onyx-reserve-headers"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read().decode().strip()


def block_hash(header_hex):
    """Display-order hash of an 80-byte header."""
    raw = bytes.fromhex(header_hex)
    if len(raw) != 80:
        raise SystemExit("header is not 80 bytes")
    return hashlib.sha256(hashlib.sha256(raw).digest()).digest()[::-1].hex()


def prev_of(header_hex):
    return bytes.fromhex(header_hex)[4:36][::-1].hex()


def main():
    args = sys.argv[1:]
    fetch = "--fetch" in args
    args = [a for a in args if a != "--fetch"]
    own = []
    while "--own-hashes" in args:
        i = args.index("--own-hashes")
        own.append(Path(args[i + 1]))
        del args[i : i + 2]
    if len(args) < 4:
        raise SystemExit(__doc__)
    lo, hi, out = int(args[0]), int(args[1]), Path(args[2])
    live = [Path(d) for d in args[3:]]
    if lo > hi:
        raise SystemExit("empty range")
    headers, fetched = [], []
    for h in range(lo, hi + 1):
        hexh = None
        for d in live:
            f = d / f"{h}.raw.hex"
            if f.exists():
                hexh = f.read_text().strip()[:160]
                break
        if hexh is None:
            if not fetch:
                raise SystemExit(f"{h}: no raw block in the live dirs (add --fetch)")
            want = get(f"{MEMPOOL}/block-height/{h}")
            if get(f"{BLOCKSTREAM}/block-height/{h}") != want:
                raise SystemExit(f"{h}: mempool.space and blockstream.info disagree")
            hexh = get(f"{MEMPOOL}/block/{want}/header")
            if block_hash(hexh) != want:
                raise SystemExit(f"{h}: fetched header does not hash to {want}")
            fetched.append(h)
        if headers and prev_of(hexh) != block_hash(headers[-1]):
            raise SystemExit(f"{h}: does not link to {h - 1}")
        headers.append(hexh)
    by_height = {lo + i: block_hash(x) for i, x in enumerate(headers)}
    checked = 0
    for f in own:
        for line in f.read_text().splitlines():
            if not line.strip():
                continue
            b = json.loads(line)
            if b["height"] in by_height:
                if by_height[b["height"]] != b["hash"]:
                    raise SystemExit(f"{f}: {b['height']} differs from the chain")
                checked += 1
    sources = [f"own node raw blocks: {lo}..{hi} except the fetched heights"]
    if fetched:
        sources.append(
            f"fetched {fetched[0]}..{fetched[-1]} from mempool.space Esplora, hashes "
            f"cross-checked on blockstream.info; {checked} of them also match our own "
            "node's flows streams"
        )
    doc = {"from": lo, "headers": headers, "sources": sources, "version": 1}
    out.write_text(json.dumps(doc, indent=0, sort_keys=True) + "\n")
    print(f"wrote {len(headers)} headers {lo}..{hi} to {out} ({len(fetched)} fetched, "
          f"{checked} matched own-node hashes)")


if __name__ == "__main__":
    main()
