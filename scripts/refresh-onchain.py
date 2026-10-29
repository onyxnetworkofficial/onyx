#!/usr/bin/env python3
"""Refresh the website's on-chain window (ADR-0043, ROADMAP P3).

Two sources of blocks:
  --rest URL   a Bitcoin Core REST endpoint (`-rest=1`; unauthenticated, read only). The script
               starts from the newest block already in the first --dir (or --start HASH), asks
               `/rest/headers/<hash>.bin?count=N` for the chain after it (older nodes:
               `/rest/headers/N/<hash>.bin`), walks back from `/rest/chaininfo.json`'s best block
               only to bound the count, and writes each new block as `<height>.raw.hex`
               (`/rest/block/<hash>.bin`) and `<height>.v3.json.gz` (`/rest/block/<hash>.json`:
               the transactions with their prevouts, the only REST call that gives input values;
               the same layout scripts/fetch-btc-node.sh writes). Loopback only unless
               --allow-remote: REST has no authentication, so it should never face a network.
  --dir DIR    block folders that already hold the files (repeat or colon-separate).

Then it runs the exporter over the last --last blocks (default 2016, about two weeks) of the
folders, writes apps/web/static/onchain/, and with --write-pins rewrites the CHAIN_* pins in
apps/web/verify/src/lib.rs from the exporter's output. Every block check (proof of work, links,
Merkle root, prevouts) is the exporter's (adapters/source-btc/src/chain.rs); this script checks
only what it fetched: each raw header hashes to the hash it asked for and links to the one
before, and each JSON names that hash and height.

Usage:
  scripts/refresh-onchain.py --dir fixtures-live/week:fixtures-live/tail [--last N] [--write-pins]
  scripts/refresh-onchain.py --rest http://127.0.0.1:8332 --dir <blocks dir> [--max-new N] ...
  scripts/refresh-onchain.py --fetch-only ...   (fetch blocks, do not export)
"""
import argparse
import gzip
import hashlib
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MAX_BIN = 4_000_000 + 1_000  # a block's serialized size is at most 4,000,000 bytes
MAX_JSON = 256 << 20  # same cap as adapters/source-btc/src/node.rs MAX_V3_JSON
HEADERS_PER_CALL = 2000  # Bitcoin Core's MAX_REST_HEADERS_RESULTS
PIN_NAMES = ("CHAIN_RECIPE_ID", "CHAIN_FROM", "CHAIN_TO", "CHAIN_TIP", "CHAIN_COMMITMENT",
             "CHAIN_RERUN_HEIGHT", "CHAIN_RERUN_INPUT")


def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def display(h):
    return h[::-1].hex()


class Rest:
    def __init__(self, url, allow_remote=False):
        u = urllib.parse.urlparse(url)
        if u.scheme != "http" or not u.hostname:
            sys.exit(f"--rest must be an http:// URL, not {url!r}")
        if not allow_remote and u.hostname not in ("127.0.0.1", "localhost", "::1"):
            sys.exit("--rest must be a loopback address (REST is unauthenticated); --allow-remote to override")
        if u.username or u.password or u.query:
            sys.exit("--rest takes no credentials or query: REST is unauthenticated")
        self.base = f"http://{u.netloc}{u.path.rstrip('/')}"

    def get(self, path, cap):
        req = urllib.request.Request(self.base + path, headers={"User-Agent": "onyx-refresh-onchain"})
        with urllib.request.urlopen(req, timeout=120) as r:
            body = r.read(cap + 1)
        if len(body) > cap:
            raise ValueError(f"{path}: answer larger than {cap} bytes")
        return body

    def chaininfo(self):
        return json.loads(self.get("/rest/chaininfo.json", 1 << 20))

    def headers_after(self, start_hash, count):
        """Raw 80-byte headers from start_hash (included) forward, as Core returns them."""
        try:
            raw = self.get(f"/rest/headers/{start_hash}.bin?count={count}", 80 * count)
        except urllib.error.HTTPError as e:
            if e.code not in (400, 404):
                raise
            raw = self.get(f"/rest/headers/{count}/{start_hash}.bin", 80 * count)
        if len(raw) % 80:
            raise ValueError("headers answer is not a whole number of headers")
        return [raw[i:i + 80] for i in range(0, len(raw), 80)]

    def block(self, h):
        raw = self.get(f"/rest/block/{h}.bin", MAX_BIN)
        js = self.get(f"/rest/block/{h}.json", MAX_JSON)
        return raw, js


def heights_in(d):
    out = set()
    for f in os.listdir(d):
        m = re.fullmatch(r"(\d+)\.v3\.json\.gz", f)
        if m and os.path.isfile(os.path.join(d, f"{m.group(1)}.raw.hex")):
            out.add(int(m.group(1)))
    return out


def local_tip(d):
    hs = heights_in(d)
    if not hs:
        return None
    h = max(hs)
    with open(os.path.join(d, f"{h}.raw.hex")) as f:
        head = bytes.fromhex(f.read(160))
    return h, display(sha256d(head))


def fetch(rest, out_dir, start, max_new, confirmations):
    """Fetch every block after `start` = (height, display hash) up to `confirmations` blocks below
    the node's best (P3A-R1-4: a pinned tip should not be reorged away), at most max_new.
    Returns the heights written."""
    os.makedirs(out_dir, exist_ok=True)
    best = rest.chaininfo()
    if best.get("chain") != "main":
        sys.exit(f"the node is on chain {best.get('chain')!r}, not main")
    height, prev = start
    want = min(max_new, int(best["blocks"]) - confirmations - height)
    written = []
    while want > 0:
        heads = rest.headers_after(prev, min(HEADERS_PER_CALL, want + 1))
        if not heads or display(sha256d(heads[0])) != prev:
            raise ValueError(f"the node's headers do not start at {prev}")
        if len(heads) == 1:
            break
        for head in heads[1:]:
            if display(head[4:36]) != prev:
                raise ValueError(f"header after {prev} does not link to it")
            h = display(sha256d(head))
            raw, js = rest.block(h)
            if raw[:80] != head:
                raise ValueError(f"{h}: block bytes do not start with its header")
            doc = json.loads(js)
            if doc.get("hash") != h or doc.get("height") != height + 1:
                raise ValueError(f"{h}: JSON names another block")
            height += 1
            tmp = os.path.join(out_dir, f".{height}.tmp")
            with open(tmp + ".hex", "w") as f:
                f.write(raw.hex())
            with gzip.open(tmp + ".gz", "wb") as f:
                f.write(js)
            os.replace(tmp + ".hex", os.path.join(out_dir, f"{height}.raw.hex"))
            os.replace(tmp + ".gz", os.path.join(out_dir, f"{height}.v3.json.gz"))
            written.append(height)
            prev = h
            want -= 1
            if want == 0:
                break
    return written


def export(dirs, last, out, write_pins):
    have = set()
    for d in dirs:
        have |= heights_in(d)
    if not have:
        sys.exit("no blocks in the folders")
    top = max(have)
    lo = top
    while lo - 1 in have:
        lo -= 1
    frm = max(lo + 1, top - last + 1)
    cmd = ["cargo", "run", "--release", "-q", "-p", "onyx-source-btc", "--example", "export-chain-stats",
           "--", out, ":".join(dirs), str(frm), str(top)]
    print("running:", " ".join(cmd), file=sys.stderr)
    r = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True)
    sys.stderr.write(r.stderr[-4000:])
    if r.returncode != 0:
        sys.exit(f"export failed ({r.returncode})")
    pins = dict(re.findall(r"^(CHAIN_[A-Z_]+)=(\S+)$", r.stdout, re.M))
    if set(pins) != set(PIN_NAMES):
        sys.exit(f"exporter printed pins {sorted(pins)}")
    print(json.dumps(pins, indent=2))
    if write_pins:
        write_pin_file(os.path.join(ROOT, "apps/web/verify/src/lib.rs"), pins)


def write_pin_file(path, pins):
    src = open(path).read()
    for k, v in pins.items():
        num = k in ("CHAIN_FROM", "CHAIN_TO", "CHAIN_RERUN_HEIGHT")
        pat = re.compile(rf"(pub const {k}: (?:u64|&str) =\s*)(\"[^\"]*\"|[0-9_]+);")
        if len(pat.findall(src)) != 1:
            sys.exit(f"{k}: not exactly one pin in {path}")
        val = f"{int(v):_}" if num else f'"{v}"'
        src = pat.sub(lambda m: m.group(1) + val + ";", src)
    open(path, "w").write(src)
    print(f"pins written to {path}; rebuild the verifier and rerun the gates", file=sys.stderr)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dir", action="append", default=[], help="block folder(s); the first receives fetched blocks")
    ap.add_argument("--rest", help="Bitcoin Core REST base URL, e.g. http://127.0.0.1:8332")
    ap.add_argument("--allow-remote", action="store_true")
    ap.add_argument("--start", help="height:hash to fetch after (default: newest block in the first --dir)")
    ap.add_argument("--max-new", type=int, default=2016)
    ap.add_argument("--confirmations", type=int, default=6, help="leave this many newest blocks unfetched")
    ap.add_argument("--last", type=int, default=2016, help="blocks in the exported window")
    ap.add_argument("--out", default=os.path.join(ROOT, "apps/web/static/onchain"))
    ap.add_argument("--write-pins", action="store_true")
    ap.add_argument("--fetch-only", action="store_true")
    a = ap.parse_args()
    dirs = [x for d in a.dir for x in d.split(":") if x]
    if not dirs:
        sys.exit("give at least one --dir")
    if a.rest:
        rest = Rest(a.rest, a.allow_remote)
        if a.start:
            h, x = a.start.split(":")
            start = (int(h), x)
        else:
            start = local_tip(dirs[0])
            if start is None:
                sys.exit(f"{dirs[0]} has no block to continue from: give --start height:hash")
        got = fetch(rest, dirs[0], start, a.max_new, a.confirmations)
        print(f"fetched {len(got)} block(s)" + (f": {got[0]}-{got[-1]}" if got else ""), file=sys.stderr)
    if not a.fetch_only:
        export(dirs, a.last, a.out, a.write_pins)


if __name__ == "__main__":
    main()
