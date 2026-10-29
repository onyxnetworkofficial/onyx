#!/usr/bin/env python3
# The website's record of what the chain indexer (ADR-0033) finds on Base Sepolia
# (apps/web/static/run/indexer-base-sepolia-2026-10-12.json). Read-only: it sends no
# transaction and needs no key; it only calls eth_chainId, eth_getCode, eth_getLogs,
# eth_getBlockByNumber and eth_call on a public node. It reads with the indexer's rules:
#   1. the deploy block is where the settlement's code appeared (no code one block before,
#      code at it; IDX-R1-4),
#   2. Finalized(uint64,bytes32,bytes32,bytes32) logs of that one contract, from the deploy
#      block to a pinned block, in ranges of at most 200 blocks (sepolia.base.org's cap),
#   3. for each event, the contract's own getFinalizedRoot and shardState, read at the pinned
#      block: the event is only a hint, the view must name the same root and binding.
# Everything is pinned to block --to, so a rerun writes the same file. The event transactions
# are matched against the recorded run (run/base-sepolia-2026-10-12.json) by hash.
# Usage: python3 -I scripts/read-indexer-sepolia.py [--rpc URL] [--to BLOCK] <run json> <out.json>
import json, sys, urllib.parse, urllib.request
from datetime import datetime, timezone

RPC = "https://sepolia.base.org"
CHAIN_ID = 84532
SETTLEMENT = "0x7d2576ee99c98ce8de353eff23708c26742a7116"
DEPLOY_BLOCK = 48037567
TO_BLOCK = 48040000
SPAN = 200
# Pinned as in crates/onyx-api/src/roots.rs and indexer.rs (cast keccak / cast sig).
FINALIZED_TOPIC = "0x642365fa0ec457443cdadf7b4792f76d3de186ce4a33f99c4f6b69e5828fba73"
GET_FINALIZED_ROOT = "8052f252"
SHARD_STATE = "6421bf40"
STATUS = ["None", "Open", "Pending", "Finalized", "Empty", "Voided", "InCase", "NoValidRoot", "Unresolved"]
SCAN = "https://sepolia.basescan.org"


def fail(msg):
    sys.exit(f"read-indexer-sepolia: {msg}")


class Node:
    def __init__(self, url):
        self.url, self.calls, self.n = url, {}, 0

    def call(self, method, params):
        self.n += 1
        self.calls[method] = self.calls.get(method, 0) + 1
        body = json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}).encode()
        req = urllib.request.Request(self.url, body, {"content-type": "application/json", "user-agent": "onyx-read-indexer"})
        with urllib.request.urlopen(req, timeout=30) as r:
            v = json.loads(r.read(16 << 20))
        if v.get("id") != self.n or v.get("error") is not None or "result" not in v:
            fail(f"{method}: bad answer {str(v)[:200]}")
        return v["result"]


def word(hexdata, i):
    return hexdata[2 + 64 * i : 2 + 64 * (i + 1)]


def public_rpc(url):
    """Scheme, host and port of an RPC URL: no user info, path, query or fragment (API keys live there)."""
    u = urllib.parse.urlsplit(url)
    host = u.hostname or ""
    return f"{u.scheme}://{host}" + (f":{u.port}" if u.port else "")


def self_test():
    """THREATS L9A-7: keys in the path, query, fragment or user info never reach the shipped file."""
    cases = {
        "https://sepolia.base.org": "https://sepolia.base.org",
        "https://base-sepolia.g.alchemy.com/v2/SECRETKEY?x=1#f": "https://base-sepolia.g.alchemy.com",
        "https://user:pw@rpc.example:8545/key/abc?apikey=def": "https://rpc.example:8545",
    }
    for url, want in cases.items():
        got = public_rpc(url)
        if got != want or "SECRET" in got or "pw" in got or "abc" in got:
            fail(f"public_rpc self-test: case {list(cases).index(url) + 1} gave the wrong origin")
    print(f"public_rpc self-test ok: {len(cases)} cases")


def main(argv):
    if list(argv) == ["--self-test"]:
        return self_test()
    rpc, to = RPC, TO_BLOCK
    args = list(argv)
    if "--rpc" in args:
        i = args.index("--rpc"); rpc = args[i + 1]; del args[i : i + 2]
    if "--to" in args:
        i = args.index("--to"); to = int(args[i + 1]); del args[i : i + 2]
    if len(args) != 2:
        fail("usage: [--rpc URL] [--to BLOCK] <run json> <out.json>")
    run_path, out_path = args
    run = json.load(open(run_path, encoding="utf-8"))
    run_txs = {t["hash"]: (s["id"], t["fn"]) for s in run["steps"] for t in s["txs"]}
    node = Node(rpc)
    if int(node.call("eth_chainId", []), 16) != CHAIN_ID:
        fail("not Base Sepolia")
    finalized = int(node.call("eth_getBlockByNumber", ["finalized", False])["number"], 16)
    if to > finalized:
        fail(f"--to {to} is above the node's finalized block {finalized}")
    pin = hex(to)

    # 1. Deploy block.
    before = node.call("eth_getCode", [SETTLEMENT, hex(DEPLOY_BLOCK - 1)])
    at = node.call("eth_getCode", [SETTLEMENT, hex(DEPLOY_BLOCK)])
    deploy_ok = before == "0x" and len(at) > 2
    if not deploy_ok:
        fail(f"deploy block check failed: {len(before)} / {len(at)} hex chars")

    # 2. Scan.
    logs, ranges, a = [], [], DEPLOY_BLOCK
    while a <= to:
        b = min(to, a + SPAN - 1)
        got = node.call("eth_getLogs", [{"address": SETTLEMENT, "fromBlock": hex(a), "toBlock": hex(b), "topics": [FINALIZED_TOPIC]}])
        for l in got:
            n = int(l["blockNumber"], 16)
            if l["address"].lower() != SETTLEMENT or l["topics"][0].lower() != FINALIZED_TOPIC or len(l["topics"]) != 3 or not a <= n <= b or l.get("removed"):
                fail(f"malformed log in {a}..{b}")
            logs.append(l)
        ranges.append([a, b, len(got)])
        a = b + 1

    # 3. The contract's view of each event, at the pinned block.
    events = []
    for l in logs:
        rnd = int(l["topics"][1], 16)
        shard = l["topics"][2].lower()
        root, binding = "0x" + l["data"][2:66].lower(), "0x" + l["data"][66:130].lower()
        args_hex = f"{rnd:064x}" + shard[2:]
        v = node.call("eth_call", [{"to": SETTLEMENT, "data": "0x" + GET_FINALIZED_ROOT + args_hex}, pin])
        st = node.call("eth_call", [{"to": SETTLEMENT, "data": "0x" + SHARD_STATE + args_hex}, pin])
        view = {"root": "0x" + word(v, 0), "binding": "0x" + word(v, 1), "finalized_at": int(word(v, 2), 16)}
        block = int(l["blockNumber"], 16)
        ts = int(node.call("eth_getBlockByNumber", [hex(block), False])["timestamp"], 16)
        tx = l["transactionHash"].lower()
        step = run_txs.get(tx)
        events.append({
            "round": rnd,
            "shard": shard,
            "root": root,
            "binding": binding,
            "block": block,
            "log_index": int(l["logIndex"], 16),
            "utc": datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%d %H:%M:%S"),
            "tx": tx,
            "tx_url": f"{SCAN}/tx/{tx}",
            "run_step": step[0] if step else None,
            "run_fn": step[1] if step else None,
            "view": view,
            "agrees": (view["root"], view["binding"]) == (root, binding) and view["finalized_at"] == block,
            "state": {"status": STATUS[int(word(st, 0), 16)], "closed": int(word(st, 1), 16) == 1},
        })
    pin_ts = int(node.call("eth_getBlockByNumber", [pin, False])["timestamp"], 16)
    out = {
        "generated_by": "scripts/read-indexer-sepolia.py",
        "command": f"python3 -I scripts/read-indexer-sepolia.py --to {to} {run_path} {out_path}",
        # THREATS L9A-7: the file ships with the site; a keyed provider URL keeps only scheme and host.
        "rpc": public_rpc(rpc),
        "network": "Base Sepolia",
        "chain_id": CHAIN_ID,
        "explorer": SCAN,
        "settlement": SETTLEMENT,
        "settlement_url": f"{SCAN}/address/{SETTLEMENT}",
        "read_at_block": to,
        "read_at_utc": datetime.fromtimestamp(pin_ts, timezone.utc).strftime("%Y-%m-%d %H:%M:%S"),
        "deploy": {"block": DEPLOY_BLOCK, "code_hex_chars_before": len(before) - 2, "code_hex_chars_at": len(at) - 2, "ok": deploy_ok},
        "scan": {"from": DEPLOY_BLOCK, "to": to, "span": SPAN, "topic": FINALIZED_TOPIC, "get_logs_calls": len(ranges), "ranges_with_events": [r for r in ranges if r[2]]},
        "rpc_calls": dict(sorted(node.calls.items())),
        "events": events,
        "all_agree": bool(events) and all(e["agrees"] for e in events),
    }
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(out, f, indent=2)
        f.write("\n")
    print(f"{len(events)} Finalized event(s) in {len(ranges)} eth_getLogs calls over {DEPLOY_BLOCK}..{to}; all agree with getFinalizedRoot: {out['all_agree']}")


if __name__ == "__main__":
    main(sys.argv[1:])
