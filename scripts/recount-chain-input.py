#!/usr/bin/env python3
"""Recount the website's shipped block input in Python (ADR-0043, THREATS P3A-R1-5).

The website ships one block's full `btc-chain-stats` recipe input
(`apps/web/static/onchain/rerun-input.b64.txt`, block 970,835). This script decodes it
(Borsh `RecipeInput` holding a `ChainSource`) without any Rust code and recomputes all 22
fields of that block's `ChainStatsRow` by the definitions in docs/spec/recipes.md R-CHAIN-2,
then requires them to equal the committed leaf of that block in
`apps/web/static/onchain/btc-chain-stats.json`. It runs in check-all, where no raw blocks
exist: a recipe change that moves a definition and a re-blessed golden file still fail here.
(`scripts/recount-chain-stats.py` recounts every block from the node's JSON, where the blocks are.)

Usage: recount-chain-input.py [<window.json> <rerun-input.b64.txt>] [--self-test]
Exit 1 on any difference; --self-test also requires each planted change to be caught.
"""
import base64
import hashlib
import json
import os
import struct
import sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from importlib import import_module  # noqa: E402

FIELDS = import_module("recount-chain-stats").FIELDS
decode_leaf = import_module("recount-chain-stats").decode_leaf


class Reader:
    def __init__(self, b):
        self.b, self.o = b, 0

    def take(self, n):
        if self.o + n > len(self.b):
            raise ValueError("truncated")
        v = self.b[self.o:self.o + n]
        self.o += n
        return v

    def u32(self):
        return struct.unpack("<I", self.take(4))[0]

    def u64(self):
        return struct.unpack("<Q", self.take(8))[0]

    def vec(self):
        return self.take(self.u32())

    def end(self):
        if self.o != len(self.b):
            raise ValueError("trailing bytes")


def decode_input(raw):
    """RecipeInput {abi_version, source, labels, params}; source = ChainSource."""
    r = Reader(raw)
    r.u32()
    src = Reader(r.vec())
    labels, params = r.vec(), r.vec()
    r.end()
    if labels or params:
        raise ValueError("labels or params are not empty")
    s = {"schema_version": src.u32(), "chain": src.vec().decode(), "parent_header": src.take(80), "blocks": []}
    for _ in range(src.u32()):
        b = {"height": src.u64(), "hash": src.take(32), "header": src.take(80), "txs": []}
        for _ in range(src.u32()):
            base, total = src.u32(), src.u32()
            ins = [{"value": src.u64(), "script": src.vec(), "created_height": src.u64()} for _ in range(src.u32())]
            outs = [{"script": src.vec(), "value": src.u64()} for _ in range(src.u32())]
            b["txs"].append({"base": base, "total": total, "ins": ins, "outs": outs})
        s["blocks"].append(b)
    src.end()
    return s


def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def subsidy(h):
    n = h // 210_000
    return 0 if n >= 64 else (50 * 100_000_000) >> n


def is_p2tr(s):
    return len(s) == 34 and s[0] == 0x51 and s[1] == 0x20


def unspendable(s):
    return s[:1] == b"\x6a" or len(s) > 10_000


def compact_size_len(n):
    return 1 if n <= 0xFC else 3 if n <= 0xFFFF else 5 if n <= 0xFFFF_FFFF else 9


def recount(src):
    """One row per block, by R-CHAIN-2; header fields from the 80 header bytes."""
    rows = []
    parent = src["parent_header"]
    for b in src["blocks"]:
        h, hdr, txs = b["height"], b["header"], b["txs"]
        if sha256d(hdr)[::-1] != b["hash"] or hdr[4:36] != sha256d(parent):
            raise ValueError(f"{h}: header hash or link")
        time = struct.unpack_from("<I", hdr, 68)[0]
        prefix = 80 + compact_size_len(len(txs))
        r = {"height": h, "hash": b["hash"].hex(), "time": time,
             "interval_secs": time - struct.unpack_from("<I", parent, 68)[0],
             "bits": struct.unpack_from("<I", hdr, 72)[0], "txs": len(txs),
             "size_bytes": prefix + sum(t["total"] for t in txs),
             "weight_wu": 4 * prefix + sum(3 * t["base"] + t["total"] for t in txs),
             "subsidy_sats": subsidy(h)}
        cb = sum(o["value"] for o in txs[0]["outs"])
        fees = transfer = vsizes = segwit = cbd = 0
        rates, scripts = [], set()
        for t in txs:
            scripts |= {o["script"] for o in t["outs"] if not unspendable(o["script"])}
        for t in txs[1:]:
            out_sum = sum(o["value"] for o in t["outs"])
            fee = sum(i["value"] for i in t["ins"]) - out_sum
            vsize = -(-(3 * t["base"] + t["total"]) // 4)
            fees += fee
            transfer += out_sum
            vsizes += vsize
            rates.append(fee * 1000 // vsize)
            segwit += t["total"] > t["base"]
            for i in t["ins"]:
                cbd += i["value"] * (h - i["created_height"])
                if not unspendable(i["script"]):
                    scripts.add(i["script"])
        rates.sort()
        r.update(inputs=sum(len(t["ins"]) for t in txs[1:]), outputs=sum(len(t["outs"]) for t in txs),
                 fees_sats=fees, coinbase_sats=cb, issued_sats=cb - fees, transfer_sats=transfer,
                 fee_rate_median_msat_vb=rates[(len(rates) - 1) // 2] if rates else 0,
                 fee_rate_avg_msat_vb=fees * 1000 // vsizes if vsizes else 0, segwit_txs=segwit,
                 taproot_inputs=sum(is_p2tr(i["script"]) for t in txs[1:] for i in t["ins"]),
                 taproot_outputs=sum(is_p2tr(o["script"]) for t in txs for o in t["outs"]),
                 active_scripts=len(scripts), coin_blocks_destroyed=cbd)
        rows.append(r)
        parent = hdr
    return rows


def differences(src, leaves):
    out = []
    for r in recount(src):
        want = leaves.get(r["height"])
        if want is None:
            out.append(f"{r['height']}: no committed leaf")
            continue
        out += [f"{r['height']} {n}: leaf {want[n]}, recount {r[n]}" for n, _ in FIELDS if want[n] != r[n]]
    return out


def self_test(src, leaves):
    """Each planted change to the source moves the recount off the leaf in the named field."""
    import copy

    def first_spend(s):
        return next(t for t in s["blocks"][0]["txs"][1:] if t["ins"])

    plants = {
        "coin_blocks_destroyed": lambda s: first_spend(s)["ins"][0].__setitem__("created_height", first_spend(s)["ins"][0]["created_height"] - 1),
        "fees_sats": lambda s: first_spend(s)["ins"][0].__setitem__("value", first_spend(s)["ins"][0]["value"] + 1),
        "weight_wu": lambda s: first_spend(s).__setitem__("base", first_spend(s)["base"] + 1),
        "taproot_outputs": lambda s: s["blocks"][0]["txs"][0]["outs"].append({"script": b"\x51\x20" + bytes(32), "value": 0}),
    }
    missed = []
    for field, plant in plants.items():
        s = copy.deepcopy(src)
        plant(s)
        if not any(f" {field}:" in d for d in differences(s, leaves)):
            missed.append(field)
    return missed


def main():
    args = [a for a in sys.argv[1:] if a != "--self-test"]
    window = args[0] if args else os.path.join(ROOT, "apps/web/static/onchain/btc-chain-stats.json")
    rerun = args[1] if len(args) > 1 else os.path.join(ROOT, "apps/web/static/onchain/rerun-input.b64.txt")
    doc = json.load(open(window))
    leaves = {}
    for d in doc["days"]:
        for leaf in d["leaves"]:
            row = decode_leaf(leaf)
            leaves[row["height"]] = row
    src = decode_input(base64.b64decode(open(rerun).read().strip(), validate=True))
    if src["schema_version"] != 1 or src["chain"] != "btc.mainnet" or len(src["blocks"]) != 1:
        sys.exit("not a one-block btc.mainnet ChainSource v1")
    h = src["blocks"][0]["height"]
    if h != doc["rerun"]["height"]:
        sys.exit(f"the input holds block {h}, the window names {doc['rerun']['height']}")
    diffs = differences(src, leaves)
    for d in diffs:
        print(d)
    if diffs:
        sys.exit(1)
    if "--self-test" in sys.argv:
        missed = self_test(src, leaves)
        if missed:
            sys.exit(f"self-test: planted changes not caught in {missed}")
    print(f"scripts/recount-chain-input.py: all {len(FIELDS)} fields of block {h} "
          f"({len(src['blocks'][0]['txs'])} transactions) match its committed leaf")


if __name__ == "__main__":
    main()
