#!/usr/bin/env python3
"""Tests of scripts/refresh-onchain.py against a fake Bitcoin Core REST server on 127.0.0.1
(ADR-0043, THREATS CHAIN-6): the fetch writes exactly the blocks the node serves after the
local tip, in the fetch-btc-node.sh layout; a header that does not link, a block whose bytes
are not its header's, a JSON naming another block, a non-main chain, a non-loopback URL and a
URL with credentials are each refused with nothing written; the old headers path is used
when the new one is missing; and the pin writer replaces each CHAIN_* pin exactly once.
No outside network: the server binds 127.0.0.1 and every block is synthetic.
"""
import gzip
import hashlib
import http.server
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "refresh-onchain.py")
spec = importlib.util.spec_from_file_location("refresh", SCRIPT)
refresh = importlib.util.module_from_spec(spec)
spec.loader.exec_module(refresh)

BASE = 900_000
fails = []


def check(ok, what):
    print(("ok   " if ok else "FAIL ") + what)
    if not ok:
        fails.append(what)


def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def chain(n):
    """n + 1 synthetic headers (the first is the local tip), linked by prev hash."""
    heads, prev = [], b"\x11" * 32
    for k in range(n + 1):
        h = (2).to_bytes(4, "little") + prev + bytes([k]) * 32 + (1_791_000_000 + 600 * k).to_bytes(4, "little") + b"\xf0\x1e\x02\x17" + k.to_bytes(4, "little")
        heads.append(h)
        prev = sha256d(h)
    return heads


class Node:
    def __init__(self, heads, chain_name="main", old_headers=False, edit=None):
        self.heads = heads
        self.by_hash = {sha256d(h)[::-1].hex(): (BASE + k, h) for k, h in enumerate(heads)}
        self.chain_name = chain_name
        self.old_headers = old_headers
        self.edit = edit or (lambda kind, height, body: body)

    def answer(self, path):
        if path == "/rest/chaininfo.json":
            best = sha256d(self.heads[-1])[::-1].hex()
            return json.dumps({"chain": self.chain_name, "blocks": BASE + len(self.heads) - 1, "bestblockhash": best}).encode()
        if path.startswith("/rest/headers/"):
            rest = path[len("/rest/headers/"):]
            if "?count=" in rest:
                if self.old_headers:
                    return None
                h, count = rest.split(".bin?count=")
            else:
                count, h = rest.split("/")
                h = h[: -len(".bin")]
            if h not in self.by_hash:
                return None
            k = self.by_hash[h][0] - BASE
            return self.edit("headers", BASE + k, b"".join(self.heads[k:k + int(count)]))
        if path.startswith("/rest/block/"):
            name = path[len("/rest/block/"):]
            h, ext = name.rsplit(".", 1)
            if h not in self.by_hash:
                return None
            height, head = self.by_hash[h]
            if ext == "bin":
                return self.edit("bin", height, head + b"txs-of-%d" % height)
            return self.edit("json", height, json.dumps({"hash": h, "height": height, "tx": []}).encode())
        return None


def serve(node):
    class H(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            body = node.answer(self.path)
            if body is None:
                self.send_response(404)
                self.end_headers()
                return
            self.send_response(200)
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *a):
            pass

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


def local(heads):
    d = tempfile.mkdtemp(prefix="onyx-refresh-")
    with open(os.path.join(d, f"{BASE}.raw.hex"), "w") as f:
        f.write((heads[0] + b"local").hex())
    with gzip.open(os.path.join(d, f"{BASE}.v3.json.gz"), "wb") as f:
        f.write(b"{}")
    return d


def run(url, d, *extra, conf="0"):
    return subprocess.run([sys.executable, "-I", SCRIPT, "--rest", url, "--dir", d, "--fetch-only", "--confirmations", conf, *extra],
                          capture_output=True, text=True, timeout=60)


def new_files(d):
    return sorted(f for f in os.listdir(d) if not f.startswith(str(BASE) + "."))


def main():
    heads = chain(5)
    # 1. Honest node: blocks BASE+1..BASE+5 written, bytes as served.
    for old in (False, True):
        node = Node(heads, old_headers=old)
        srv = serve(node)
        d = local(heads)
        r = run(f"http://127.0.0.1:{srv.server_address[1]}", d)
        ok = r.returncode == 0 and len(new_files(d)) == 10
        for k in range(1, 6):
            h = BASE + k
            raw = bytes.fromhex(open(os.path.join(d, f"{h}.raw.hex")).read()) if ok else b""
            js = json.loads(gzip.open(os.path.join(d, f"{h}.v3.json.gz")).read()) if ok else {}
            ok = ok and raw == heads[k] + b"txs-of-%d" % h and js.get("height") == h
        check(ok, f"an honest node's 5 new blocks are written as served ({'old' if old else 'new'} headers path)")
        # Run again: nothing new, nothing rewritten.
        r = run(f"http://127.0.0.1:{srv.server_address[1]}", d)
        check(r.returncode == 0 and "fetched 0 block" in r.stderr, "a second run at the node's tip fetches nothing")
        srv.shutdown()
        shutil.rmtree(d)
    # P3A-R1-4: by default the newest 6 blocks stay unfetched (3 of 8 new blocks are written).
    heads8 = chain(9)
    srv = serve(Node(heads8))
    d = local(heads8)
    r = run(f"http://127.0.0.1:{srv.server_address[1]}", d, conf="6")
    check(r.returncode == 0 and len(new_files(d)) == 6, "six confirmations: the newest 6 of 9 new blocks stay unfetched")
    srv.shutdown()
    shutil.rmtree(d)
    # --max-new bounds the fetch.
    srv = serve(Node(heads))
    d = local(heads)
    r = run(f"http://127.0.0.1:{srv.server_address[1]}", d, "--max-new", "2")
    check(r.returncode == 0 and new_files(d) == sorted([f"{BASE + 1}.raw.hex", f"{BASE + 1}.v3.json.gz", f"{BASE + 2}.raw.hex", f"{BASE + 2}.v3.json.gz"]), "--max-new 2 fetches two blocks")
    srv.shutdown()
    shutil.rmtree(d)
    # 2. Lies, each refused before its block is written.
    def unlinked(kind, height, body):
        if kind == "headers" and len(body) > 160:
            return body[:80 + 4] + b"\x00" * 32 + body[80 + 36:]
        return body
    lies = {
        "a header that does not link": unlinked,
        "block bytes that are not its header's": lambda k, h, b: b"\x00" + b[1:] if k == "bin" and h == BASE + 3 else b,
        "a JSON naming another height": lambda k, h, b: json.dumps({"hash": json.loads(b)["hash"], "height": h + 1}).encode() if k == "json" and h == BASE + 2 else b,
    }
    for what, edit in lies.items():
        srv = serve(Node(heads, edit=edit))
        d = local(heads)
        r = run(f"http://127.0.0.1:{srv.server_address[1]}", d)
        bad_h = {"a header that does not link": BASE + 1, "block bytes that are not its header's": BASE + 3, "a JSON naming another height": BASE + 2}[what]
        written = new_files(d)
        check(r.returncode != 0 and all(int(f.split(".")[0]) < bad_h for f in written), f"refused: {what} (nothing at or after it written)")
        srv.shutdown()
        shutil.rmtree(d)
    srv = serve(Node(heads, chain_name="test"))
    d = local(heads)
    r = run(f"http://127.0.0.1:{srv.server_address[1]}", d)
    check(r.returncode != 0 and new_files(d) == [], "refused: a node on another chain")
    srv.shutdown()
    for url, what in [("http://192.0.2.1:8332", "a non-loopback URL"), ("http://user:pw@127.0.0.1:8332", "a URL with credentials"),
                      ("https://127.0.0.1:8332", "a non-http URL")]:
        r = run(url, d)
        check(r.returncode != 0 and new_files(d) == [], f"refused before any request: {what}")
    shutil.rmtree(d)
    # 3. The pin writer replaces every CHAIN_* pin exactly once.
    lib = os.path.join(os.path.dirname(HERE), "apps/web/verify/src/lib.rs")
    tmp = tempfile.mkdtemp(prefix="onyx-pins-")
    p = os.path.join(tmp, "lib.rs")
    shutil.copy(lib, p)
    pins = {"CHAIN_RECIPE_ID": "0x" + "ab" * 32, "CHAIN_FROM": "100", "CHAIN_TO": "123456", "CHAIN_TIP": "cd" * 32,
            "CHAIN_COMMITMENT": "0x" + "ef" * 32, "CHAIN_RERUN_HEIGHT": "120000", "CHAIN_RERUN_INPUT": "0x" + "01" * 32}
    refresh.write_pin_file(p, pins)
    src = open(p).read()
    check(all(src.count(f'"{v}"') == 1 for k, v in pins.items() if not k.endswith(("FROM", "TO", "HEIGHT")))
          and "CHAIN_TO: u64 = 123_456;" in src and "CHAIN_FROM: u64 = 100;" in src,
          "the pin writer sets each CHAIN_* pin once")
    shutil.rmtree(tmp)
    if fails:
        sys.exit(f"{len(fails)} refresh test(s) failed")
    print("refresh-onchain tests: all passed")


if __name__ == "__main__":
    main()
