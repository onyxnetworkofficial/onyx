#!/usr/bin/env python3
"""Fetch an Ethereum mainnet block window and refresh the website's Ethereum section (ADR-0044,
ROADMAP P4a).

Source of blocks: an Ethereum JSON-RPC endpoint. First choice is your own node on loopback; a
public keyless endpoint works too (read its terms first; DATA-LICENSE.md records the ones Onyx
used). Read-only calls only: `eth_getBlockByNumber` (with full transactions) and
`eth_getBlockReceipts`, one of each per block, plus `eth_getBlockByNumber("finalized")`. The
script never sends a transaction, never signs and never takes a key or credentials (a URL with a
user, password or query string is refused).

What it checks itself (Python has no Keccak-256, so hashes are the exporter's job): each answer
is the block it asked for, each block names the one before as its parent, the receipts are the
block's (same block hash, one per transaction, in order), and the window's last block is at or
below the endpoint's finalized block. Unless --no-cross-check, a second endpoint run by another
operator must return the same hash for the window's parent, first, every 50th and last block,
and its own finalized block must be at or above the window's tip. Every block check that needs a
hash (header hash, parent links by hash, transactions root, receipts root, base fee, blob fee)
is the exporter's (adapters/source-eth/src/lib.rs `load_eth_range`), which refuses on any
mismatch.

Layout written in --dir (gitignored `fixtures-live/eth` by default): `<n>.block.json.gz` and
`<n>.receipts.json.gz`, each the endpoint's raw HTTP answer body, gzip with a zero mtime; the
window's parent has its block file only. `MANIFEST.json` records the endpoints, the fetch time,
the finalized heights, the cross-checked hashes and the SHA-256 of every raw answer
(uncompressed).

Then, unless --fetch-only, it runs the exporter over the window, writes
apps/web/static/onchain/eth-chain-stats.json and eth-rerun-input.b64.txt, and with --write-pins
rewrites the ETH_CHAIN_* pins in apps/web/verify/src/eth_chain.rs from the exporter's output.

Usage:
  scripts/refresh-eth-onchain.py --rpc URL --check-rpc URL2 [--count 300] [--to N] [--write-pins]
  scripts/refresh-eth-onchain.py --dir fixtures-live/eth [--write-pins]       (export only)
  scripts/refresh-eth-onchain.py --rpc URL --check-rpc URL2 --fetch-only ...
"""
import argparse
import gzip
import hashlib
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
USER_AGENT = "onyx-dev"
MAX_ANSWER = 64 << 20  # bytes of one JSON-RPC answer (a full block's receipts are a few MB)
MAX_RATE = 5.0  # requests per second, never more (public endpoints' fair use)
CROSS_EVERY = 50
PIN_NAMES = ("ETH_CHAIN_RECIPE_ID", "ETH_CHAIN_FROM", "ETH_CHAIN_TO", "ETH_CHAIN_TIP",
             "ETH_CHAIN_COMMITMENT", "ETH_CHAIN_RERUN_HEIGHT", "ETH_CHAIN_RERUN_INPUT")
HEX32 = re.compile(r"0x[0-9a-f]{64}")


class Rpc:
    """A rate-limited JSON-RPC client for read-only calls."""

    METHODS = ("eth_getBlockByNumber", "eth_getBlockReceipts", "eth_chainId")

    def __init__(self, url, rate, retries=5):
        u = urllib.parse.urlparse(url)
        if u.scheme not in ("http", "https") or not u.hostname:
            sys.exit(f"an endpoint must be an http(s):// URL, not {url!r}")
        if u.username or u.password or u.query or u.fragment:
            sys.exit("an endpoint takes no credentials, query or fragment (no API keys)")
        if u.scheme == "http" and u.hostname not in ("127.0.0.1", "localhost", "::1"):
            sys.exit("plain http only on loopback; use https for a remote endpoint")
        if not 0 < rate <= MAX_RATE:
            sys.exit(f"--rate must be in (0, {MAX_RATE}]")
        self.url = url
        self.gap = 1.0 / rate
        self.last = 0.0
        self.retries = retries
        self.calls = 0

    def raw(self, method, params):
        """The raw answer body of one call (bytes) and its decoded result."""
        if method not in self.METHODS:
            raise ValueError(f"{method}: not a read-only call this script makes")
        body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
        err = None
        for attempt in range(self.retries):
            wait = self.last + self.gap - time.monotonic()
            if wait > 0:
                time.sleep(wait)
            self.last = time.monotonic()
            self.calls += 1
            req = urllib.request.Request(self.url, data=body, method="POST", headers={
                "User-Agent": USER_AGENT, "Content-Type": "application/json", "Accept": "application/json"})
            try:
                with urllib.request.urlopen(req, timeout=120) as r:
                    ans = r.read(MAX_ANSWER + 1)
                if len(ans) > MAX_ANSWER:
                    raise ValueError(f"{method}: answer larger than {MAX_ANSWER} bytes")
                doc = json.loads(ans)
                if not isinstance(doc, dict) or doc.get("error") or "result" not in doc:
                    raise ValueError(f"{method}: {str(doc)[:200]}")
                return ans, doc["result"]
            except (urllib.error.URLError, ValueError, TimeoutError, ConnectionError) as e:
                if isinstance(e, ValueError) and "larger than" in str(e):
                    raise
                err = e
                time.sleep(min(30.0, 2.0 ** attempt))
        raise RuntimeError(f"{self.url}: {method} {params}: {err}")

    def block(self, n, full=True):
        return self.raw("eth_getBlockByNumber", [hex(n) if isinstance(n, int) else n, full])

    def receipts(self, n):
        return self.raw("eth_getBlockReceipts", [hex(n)])

    def finalized(self):
        _, b = self.block("finalized", False)
        if not isinstance(b, dict):
            raise ValueError("no finalized block")
        return int(b["number"], 16), b["hash"]

    def chain_id(self):
        _, c = self.raw("eth_chainId", [])
        return int(c, 16)


def gz_write(path, data):
    tmp = path + ".tmp"
    with open(tmp, "wb") as f:
        with gzip.GzipFile(fileobj=f, mode="wb", mtime=0, filename="") as g:
            g.write(data)
    os.replace(tmp, path)


def gz_read(path):
    with gzip.open(path, "rb") as f:
        return f.read(MAX_ANSWER + 1)


def check_block(n, result, parent_hash=None):
    """The answer is block n, well formed, and (when given) names parent_hash as its parent."""
    if not isinstance(result, dict):
        raise ValueError(f"block {n}: the endpoint returned no block")
    if result.get("number") != hex(n):
        raise ValueError(f"block {n}: the answer is block {result.get('number')}")
    if not HEX32.fullmatch(str(result.get("hash", ""))) or not HEX32.fullmatch(str(result.get("parentHash", ""))):
        raise ValueError(f"block {n}: malformed hash fields")
    if parent_hash is not None and result["parentHash"] != parent_hash:
        raise ValueError(f"block {n}: does not name {parent_hash} as its parent")
    txs = result.get("transactions")
    if not isinstance(txs, list) or any(not isinstance(t, dict) for t in txs):
        raise ValueError(f"block {n}: transactions are not full objects")
    return result["hash"]


def check_receipts(n, block, receipts):
    if not isinstance(receipts, list) or len(receipts) != len(block["transactions"]):
        raise ValueError(f"block {n}: {0 if not isinstance(receipts, list) else len(receipts)} receipts "
                         f"for {len(block['transactions'])} transactions")
    for i, (r, t) in enumerate(zip(receipts, block["transactions"])):
        if not isinstance(r, dict) or r.get("blockHash") != block["hash"] or r.get("transactionHash") != t.get("hash") \
                or r.get("transactionIndex") != hex(i):
            raise ValueError(f"block {n}: receipt {i} is not the block's transaction {i}")


def window_heights(d):
    out = set()
    for f in os.listdir(d):
        m = re.fullmatch(r"(\d+)\.receipts\.json\.gz", f)
        if m and os.path.isfile(os.path.join(d, f"{m.group(1)}.block.json.gz")):
            out.add(int(m.group(1)))
    return out


def fetch(rpc, check, out_dir, count, to, now=None):
    """Fetch blocks to-count+1..to (to defaults to the endpoint's finalized block) and the parent
    of the first; returns the manifest written."""
    os.makedirs(out_dir, exist_ok=True)
    if rpc.chain_id() != 1:
        sys.exit("the endpoint is not Ethereum mainnet (chain id 1)")
    fin, fin_hash = rpc.finalized()
    to = fin if to is None else to
    if to > fin:
        sys.exit(f"block {to} is above the endpoint's finalized block {fin}")
    frm = to - count + 1
    if count < 1 or frm < 1:
        sys.exit("bad --count")
    files, hashes = {}, {}
    ans, parent = rpc.block(frm - 1)
    prev = check_block(frm - 1, parent)
    hashes[frm - 1] = prev
    name = f"{frm - 1}.block.json.gz"
    gz_write(os.path.join(out_dir, name), ans)
    files[name] = hashlib.sha256(ans).hexdigest()
    t0 = time.monotonic()
    for n in range(frm, to + 1):
        ans_b, blk = rpc.block(n)
        prev = check_block(n, blk, prev)
        ans_r, rec = rpc.receipts(n)
        check_receipts(n, blk, rec)
        hashes[n] = prev
        for kind, a in (("block", ans_b), ("receipts", ans_r)):
            name = f"{n}.{kind}.json.gz"
            gz_write(os.path.join(out_dir, name), a)
            files[name] = hashlib.sha256(a).hexdigest()
        if (n - frm) % 50 == 0 or n == to:
            print(f"{n}: {len(blk['transactions'])} txs, {time.monotonic() - t0:.0f} s, {rpc.calls} calls", file=sys.stderr)
    cross = []
    if check is not None:
        if check.chain_id() != 1:
            sys.exit("the cross-check endpoint is not Ethereum mainnet")
        cfin, _ = check.finalized()
        if cfin < to:
            sys.exit(f"the cross-check endpoint has finalized only {cfin} < {to}")
        picks = sorted({frm - 1, frm, to} | {h for h in range(frm, to + 1) if (h - frm) % CROSS_EVERY == 0})
        for h in picks:
            _, b = check.block(h, False)
            got = b.get("hash") if isinstance(b, dict) else None
            if got != hashes[h]:
                sys.exit(f"block {h}: the cross-check endpoint says {got}, the source said {hashes[h]}")
            cross.append({"number": h, "hash": got})
    manifest = {
        "version": 1,
        "chain": "eth.mainnet",
        "rpc": rpc.url,
        "check_rpc": check.url if check else None,
        "fetched_at": now or time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "finalized_at_fetch": {"number": fin, "hash": fin_hash},
        "parent": frm - 1,
        "from": frm,
        "to": to,
        "tip": hashes[to],
        "cross_checked": cross,
        "requests": rpc.calls + (check.calls if check else 0),
        "sha256": dict(sorted(files.items(), key=lambda kv: (int(kv[0].split(".")[0]), kv[0]))),
    }
    tmp = os.path.join(out_dir, "MANIFEST.json.tmp")
    with open(tmp, "w") as f:
        json.dump(manifest, f, indent=1)
        f.write("\n")
    os.replace(tmp, os.path.join(out_dir, "MANIFEST.json"))
    return manifest


def verify_manifest(d):
    """Every file the manifest names holds the answer it hashed. Returns the manifest."""
    with open(os.path.join(d, "MANIFEST.json")) as f:
        m = json.load(f)
    for name, want in m["sha256"].items():
        got = hashlib.sha256(gz_read(os.path.join(d, name))).hexdigest()
        if got != want:
            sys.exit(f"{name}: SHA-256 {got} is not the manifest's {want}")
    return m


def export(d, out, write_pins):
    m = verify_manifest(d)
    cmd = ["cargo", "run", "--release", "-q", "-p", "onyx-source-eth", "--example", "export-eth-chain-stats",
           "--", out, d, str(m["from"]), str(m["to"])]
    print("running:", " ".join(cmd), file=sys.stderr)
    r = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True)
    sys.stderr.write(r.stderr[-4000:])
    if r.returncode != 0:
        sys.exit(f"export failed ({r.returncode})")
    pins = dict(re.findall(r"^(ETH_CHAIN_[A-Z_]+)=(\S+)$", r.stdout, re.M))
    if set(pins) != set(PIN_NAMES):
        sys.exit(f"exporter printed pins {sorted(pins)}")
    print(json.dumps(pins, indent=2))
    if write_pins:
        write_pin_file(os.path.join(ROOT, "apps/web/verify/src/eth_chain.rs"), pins)


def write_pin_file(path, pins):
    with open(path) as f:
        src = f.read()
    for k, v in pins.items():
        num = k in ("ETH_CHAIN_FROM", "ETH_CHAIN_TO", "ETH_CHAIN_RERUN_HEIGHT")
        pat = re.compile(rf"(pub const {k}: (?:u64|&str) =\s*)(\"[^\"]*\"|[0-9_]+);")
        if len(pat.findall(src)) != 1:
            sys.exit(f"{k}: not exactly one pin in {path}")
        if num and not re.fullmatch(r"[0-9]+", v) or not num and not re.fullmatch(r"(0x)?[0-9a-f]{64}", v):
            sys.exit(f"{k}: malformed value {v!r}")
        val = f"{int(v):_}" if num else f'"{v}"'
        src = pat.sub(lambda mm: mm.group(1) + val + ";", src)
    with open(path, "w") as f:
        f.write(src)
    print(f"pins written to {path}; rebuild the verifier and rerun the gates", file=sys.stderr)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dir", default=os.path.join(ROOT, "fixtures-live/eth"), help="window folder")
    ap.add_argument("--rpc", help="Ethereum JSON-RPC URL to fetch from (read-only calls)")
    ap.add_argument("--check-rpc", help="a second endpoint, run by another operator, for the hash cross-check")
    ap.add_argument("--no-cross-check", action="store_true", help="skip it (the manifest then says so)")
    ap.add_argument("--count", type=int, default=300, help="blocks in the window")
    ap.add_argument("--to", type=int, help="last block (default: the endpoint's finalized block)")
    ap.add_argument("--rate", type=float, default=4.0, help=f"requests per second per endpoint (at most {MAX_RATE})")
    ap.add_argument("--out", default=os.path.join(ROOT, "apps/web/static/onchain"))
    ap.add_argument("--write-pins", action="store_true")
    ap.add_argument("--fetch-only", action="store_true")
    a = ap.parse_args()
    if a.rpc:
        if not a.check_rpc and not a.no_cross_check:
            sys.exit("give --check-rpc (another operator's endpoint) or --no-cross-check")
        if a.check_rpc and not a.no_cross_check and \
                urllib.parse.urlparse(a.check_rpc).hostname == urllib.parse.urlparse(a.rpc).hostname:
            sys.exit("--check-rpc must be another operator's endpoint (another host)")
        rpc = Rpc(a.rpc, a.rate)
        check = Rpc(a.check_rpc, a.rate) if a.check_rpc and not a.no_cross_check else None
        m = fetch(rpc, check, a.dir, a.count, a.to)
        print(f"fetched {m['from']}-{m['to']} ({m['requests']} requests); cross-checked {len(m['cross_checked'])} hashes",
              file=sys.stderr)
    if not a.fetch_only:
        export(a.dir, a.out, a.write_pins)


if __name__ == "__main__":
    main()
