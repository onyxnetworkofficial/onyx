#!/usr/bin/env python3
"""Tests of scripts/refresh-eth-onchain.py against fake Ethereum JSON-RPC servers on 127.0.0.1
(ADR-0044, THREATS ETH-7): the fetch writes exactly the answers the endpoint gives, byte for
byte (their SHA-256 are the committed manifest's), only with read-only calls, the declared
User-Agent and at most the given rate; a block that does not link to its parent, receipts of
another block, an answer naming another block, a tip above the finalized block, another chain
and a second operator that disagrees on a hash are each refused with no manifest written; URLs
with credentials or a query, plain http off loopback and a rate above 5 per second are refused;
an answer file changed after the fetch fails the manifest check; and the pin writer replaces
each ETH_CHAIN_* pin exactly once. No outside network: the servers bind 127.0.0.1 and serve the
committed mainnet blocks 26,225,267-26,225,270 (fixtures/eth/blocks-26225268-26225270).
"""
import gzip
import hashlib
import http.server
import importlib.util
import json
import os
import shutil
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SCRIPT = os.path.join(HERE, "refresh-eth-onchain.py")
spec = importlib.util.spec_from_file_location("refresh_eth", SCRIPT)
refresh = importlib.util.module_from_spec(spec)
spec.loader.exec_module(refresh)

SUBSET = os.path.join(ROOT, "fixtures/eth/blocks-26225268-26225270")
FIRST, LAST = 26_225_268, 26_225_270
fails = []


def check(ok, what):
    print(("ok   " if ok else "FAIL ") + what)
    if not ok:
        fails.append(what)


def raw(name):
    with gzip.open(os.path.join(SUBSET, name), "rb") as f:
        return f.read()


ANSWERS = {}
for n in range(FIRST - 1, LAST + 1):
    ANSWERS[("block", n)] = raw(f"{n}.block.json.gz")
    if n >= FIRST:
        ANSWERS[("receipts", n)] = raw(f"{n}.receipts.json.gz")
with open(os.path.join(SUBSET, "MANIFEST.json")) as f:
    WANT_SHA = json.load(f)["sha256"]


def short_block(n):
    """The answer to eth_getBlockByNumber(n, false): the full answer with hashes for transactions."""
    doc = json.loads(ANSWERS[("block", n)])
    doc["result"]["transactions"] = [t["hash"] for t in doc["result"]["transactions"]]
    return json.dumps(doc).encode()


class Node:
    def __init__(self, chain_id=1, finalized=LAST, edit=None):
        self.chain_id = chain_id
        self.finalized = finalized
        self.edit = edit or (lambda kind, n, body: body)
        self.calls = []
        self.agents = set()
        self.times = []

    def answer(self, req):
        m, p = req["method"], req.get("params", [])
        self.calls.append(m)
        if m == "eth_chainId":
            return json.dumps({"jsonrpc": "2.0", "id": 1, "result": hex(self.chain_id)}).encode()
        if m == "eth_getBlockByNumber":
            tag, full = p
            n = self.finalized if tag == "finalized" else int(tag, 16)
            if ("block", n) not in ANSWERS:
                return json.dumps({"jsonrpc": "2.0", "id": 1, "result": None}).encode()
            body = ANSWERS[("block", n)] if full else short_block(n)
            return self.edit("block" if full else "short", n, body)
        if m == "eth_getBlockReceipts":
            n = int(p[0], 16)
            return self.edit("receipts", n, ANSWERS[("receipts", n)])
        return json.dumps({"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "no"}}).encode()


def serve(node):
    class H(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            node.agents.add(self.headers.get("User-Agent"))
            node.times.append(time.monotonic())
            body = node.answer(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *a):
            pass

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, f"http://127.0.0.1:{srv.server_address[1]}/"


def run_fetch(node, check_node=None, to=None, count=3, rate=50 / 10):
    srv, url = serve(node)
    FIRST_URL[0] = url
    srv2, url2 = serve(check_node) if check_node else (None, None)
    d = tempfile.mkdtemp(prefix="onyx-eth-fetch-")
    try:
        rpc = refresh.Rpc(url, rate, retries=1)
        chk = refresh.Rpc(url2, rate, retries=1) if url2 else None
        try:
            m = refresh.fetch(rpc, chk, d, count, to, now="2026-10-19T00:00:00Z")
            err = None
        except (SystemExit, ValueError, RuntimeError) as e:
            m, err = None, str(e)
        files = sorted(os.listdir(d))
        return m, err, files, d
    finally:
        srv.shutdown()
        if srv2:
            srv2.shutdown()


def edited(kind_want, n_want, f):
    def e(kind, n, body):
        if kind == kind_want and n == n_want:
            doc = json.loads(body)
            f(doc["result"])
            return json.dumps(doc).encode()
        return body
    return e


# Every request the client sends to the first endpoint, at the time its pacer released it (the
# caller's `self.last`, for the rate check): the pacer's own clock, so a busy machine's scheduling
# delays between the pacer and the socket do not count as sends closer together.
SENDS = []
_urlopen = refresh.urllib.request.urlopen


def timed_urlopen(req, *a, **k):
    if len(SENDS) < 1000 and getattr(req, "full_url", "").startswith(FIRST_URL[0]):
        SENDS.append(sys._getframe(1).f_locals["self"].last)
    return _urlopen(req, *a, **k)


refresh.urllib.request.urlopen = timed_urlopen
FIRST_URL = [""]

# 1. A clean fetch: the committed answers, byte for byte, read-only calls, the User-Agent, the rate.
node, other = Node(), Node()
t0 = time.monotonic()
m, err, files, d = run_fetch(node, other)
check(err is None and m is not None, f"a clean fetch succeeds ({err})")
check(m is not None and m["sha256"] == WANT_SHA, "every answer written is the committed one (SHA-256 of the raw bytes)")
check("MANIFEST.json" in files and len(files) == len(WANT_SHA) + 1, f"{len(files)} files: the 7 answers and the manifest")
check(set(node.calls) | set(other.calls) <= {"eth_chainId", "eth_getBlockByNumber", "eth_getBlockReceipts"},
      "only read-only calls are made")
check(node.agents == {"onyx-dev"} and other.agents == {"onyx-dev"}, "User-Agent onyx-dev and nothing else")
# The rate is a property of the client's pacer (server arrival times jitter under load: one gate
# run saw 9 arrivals over 1.54 s; timed at urlopen, a busy machine once showed 0.179 s between two
# sends the pacer had released 0.2 s apart), so the sends are timed on the pacer's clock.
gaps = [b - a for a, b in zip(SENDS, SENDS[1:])]
check(len(gaps) > 4 and min(gaps) >= 1 / 5 - 0.001, f"at most 5 requests a second ({len(SENDS)} sends, closest {min(gaps):.3f} s apart)")
check(m is not None and [c["number"] for c in m["cross_checked"]] == [FIRST - 1, FIRST, LAST],
      "the second operator confirmed the parent, first and last hashes")
check(m is not None and m["tip"] == json.loads(ANSWERS[("block", LAST)])["result"]["hash"], "the manifest names the tip")
check(refresh.verify_manifest(d) == m, "the manifest checks against the files")
# An answer file changed afterwards fails the check.
with gzip.open(os.path.join(d, f"{FIRST}.receipts.json.gz"), "wb") as f:
    f.write(ANSWERS[("receipts", LAST)])
try:
    refresh.verify_manifest(d)
    check(False, "a changed answer file fails the manifest check")
except SystemExit as e:
    check("not the manifest" in str(e), "a changed answer file fails the manifest check")
shutil.rmtree(d)

# 2. Lies and mismatches are refused, with no manifest written.
cases = [
    ("a block that does not link to its parent",
     Node(edit=edited("block", FIRST + 1, lambda b: b.__setitem__("parentHash", "0x" + "11" * 32))), None, "parent"),
    ("receipts of another block",
     Node(edit=edited("receipts", FIRST, lambda r: [x.__setitem__("blockHash", "0x" + "22" * 32) for x in r])), None, "receipt"),
    ("receipts missing one",
     Node(edit=edited("receipts", FIRST, lambda r: r.pop())), None, "receipts"),
    ("an answer naming another block",
     Node(edit=edited("block", FIRST, lambda b: b.__setitem__("number", hex(FIRST + 7)))), None, "the answer is block"),
    ("a second operator with another hash",
     Node(), Node(edit=edited("short", FIRST, lambda b: b.__setitem__("hash", "0x" + "33" * 32))), "cross-check endpoint says"),
    ("a second operator that has not finalized the tip", Node(), Node(finalized=LAST - 1), "finalized only"),
    ("another chain", Node(chain_id=11155111), None, "not Ethereum mainnet"),
]
for what, n1, n2, why in cases:
    m, err, files, d = run_fetch(n1, n2)
    check(m is None and err is not None and why in err and "MANIFEST.json" not in files, f"refused: {what} ({err})")
    shutil.rmtree(d)
m, err, files, d = run_fetch(Node(), None, to=LAST + 1)
check(m is None and "above the endpoint's finalized" in (err or ""), "refused: a tip above the finalized block")
shutil.rmtree(d)

# 3. Endpoint and rate rules.
for url, why in [("https://user:pw@example.org/", "credentials"), ("https://example.org/?apikey=1", "credentials"),
                 ("http://example.org/", "loopback"), ("ftp://127.0.0.1/", "http(s)")]:
    try:
        refresh.Rpc(url, 4)
        check(False, f"refused: {url}")
    except SystemExit as e:
        check(why in str(e), f"refused: {url} ({e})")
try:
    refresh.Rpc("http://127.0.0.1:1/", 6)
    check(False, "refused: a rate above 5 a second")
except SystemExit as e:
    check("--rate" in str(e), "refused: a rate above 5 a second")
try:
    refresh.Rpc("http://127.0.0.1:1/", 4).raw("eth_sendRawTransaction", ["0x00"])
    check(False, "refused: a call that is not read-only")
except ValueError as e:
    check("not a read-only call" in str(e), "refused: a call that is not read-only")

# P4A-R1-3: the cross-check endpoint must be another host (refused before any network call).
import subprocess
r = subprocess.run([sys.executable, "-I", SCRIPT, "--rpc", "https://one.example/", "--check-rpc", "https://one.example/other",
                    "--fetch-only", "--dir", tempfile.mkdtemp(prefix="onyx-eth-same-")], capture_output=True, text=True)
check(r.returncode != 0 and "another host" in r.stderr, "refused: a cross-check endpoint on the same host")

# 4. The pin writer replaces each pin exactly once.
src = os.path.join(ROOT, "apps/web/verify/src/eth_chain.rs")
d = tempfile.mkdtemp(prefix="onyx-eth-pins-")
tmp = os.path.join(d, "eth_chain.rs")
shutil.copy(src, tmp)
pins = {"ETH_CHAIN_RECIPE_ID": "0x" + "ab" * 32, "ETH_CHAIN_FROM": "1", "ETH_CHAIN_TO": "26225999",
        "ETH_CHAIN_TIP": "cd" * 32, "ETH_CHAIN_COMMITMENT": "0x" + "ef" * 32, "ETH_CHAIN_RERUN_HEIGHT": "7",
        "ETH_CHAIN_RERUN_INPUT": "0x" + "01" * 32}
refresh.write_pin_file(tmp, pins)
with open(tmp) as f:
    out = f.read()
check(f'"0x{"ab" * 32}"' in out and "26_225_999;" in out and "ETH_CHAIN_RERUN_HEIGHT: u64 = 7;" in out
      and out.count("pub const ETH_CHAIN_") == 7, "the pin writer replaces every pin")
try:
    refresh.write_pin_file(tmp, {"ETH_CHAIN_TIP": "not hex"})
    check(False, "the pin writer refuses a malformed value")
except SystemExit:
    check(True, "the pin writer refuses a malformed value")
shutil.rmtree(d)

if fails:
    print(f"{len(fails)} FAILED")
    sys.exit(1)
print("refresh-eth-onchain: all checks passed")
