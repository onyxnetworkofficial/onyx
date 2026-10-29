#!/usr/bin/env python3
"""Independent recount of `eth-chain-stats` (ADR-0044, docs/spec/recipes.md R-ETH-2).

Reads the JSON-RPC answers of every block of the window (`<n>.block.json.gz` and
`<n>.receipts.json.gz`, as scripts/refresh-eth-onchain.py writes them; each checked against the
folder's MANIFEST.json SHA-256) and recomputes each block's row from the RPC's own fields: the
receipts' `effectiveGasPrice`, `gasUsed`, `status`, `contractAddress` and `blobGasPrice`, the
transactions' `type`, `to` and `value`, the header's quantities. The recipe computes the same row
from the hashed RLP bytes (the priority fee from each transaction's fee caps and the gas between
cumulative totals, the blob base fee from the excess blob gas), so a decoding or definition bug in
either shows up here. Integers only.

Usage: recount-eth-chain-stats.py <eth-chain-stats.json> <window dir>
Prints one line per differing field and, last, a summary line; exit 1 on any difference.
"""
import gzip
import hashlib
import json
import os
import struct
import sys

FIELDS = [
    ("number", "Q"), ("hash", "32s"), ("time", "Q"), ("interval_secs", "Q"), ("txs", "Q"),
    ("failed_txs", "Q"), ("contract_creations", "Q"), ("gas_used", "Q"), ("gas_limit", "Q"),
    ("fullness_ppm", "I"), ("base_fee_wei", "u128"), ("burned_wei", "u128"),
    ("priority_fees_wei", "u128"), ("blob_txs", "Q"), ("blob_gas_used", "Q"),
    ("excess_blob_gas", "Q"), ("blob_base_fee_wei", "u128"), ("blob_fee_wei", "u128"),
    ("value_wei", "u128"),
]
BPO2_FRACTION = 11_684_671  # EIP-7892 BPO2 blob base fee update fraction


def decode_leaf(hexstr):
    """Borsh OutputRow {key: Vec<u8>, value: Vec<u8>} holding an EthChainStatsRow."""
    b = bytes.fromhex(hexstr[2:] if hexstr.startswith("0x") else hexstr)
    (klen,) = struct.unpack_from("<I", b, 0)
    key = b[4:4 + klen]
    off = 4 + klen
    (vlen,) = struct.unpack_from("<I", b, off)
    v = b[off + 4:off + 4 + vlen]
    if off + 4 + vlen != len(b):
        raise ValueError("trailing bytes in leaf")
    row, o = {}, 0
    for name, fmt in FIELDS:
        if fmt == "u128":
            row[name] = int.from_bytes(v[o:o + 16], "little")
            o += 16
        else:
            (row[name],) = struct.unpack_from("<" + fmt, v, o)
            o += struct.calcsize(fmt)
    if o != len(v):
        raise ValueError("row length")
    row["hash"] = "0x" + row["hash"].hex()
    if key != struct.pack(">Q", row["number"]):
        raise ValueError("key is not the number")
    return row


def fake_exponential(factor, numerator, denominator):
    """EIP-4844's reference loop, written again here (Python integers)."""
    i, output, acc = 1, 0, factor * denominator
    while acc > 0:
        output += acc
        acc = acc * numerator // (denominator * i)
        i += 1
    return output // denominator


class Folder:
    def __init__(self, d):
        self.d = d
        with open(os.path.join(d, "MANIFEST.json")) as f:
            self.sha = json.load(f)["sha256"]

    def result(self, name):
        with gzip.open(os.path.join(self.d, name), "rb") as f:
            raw = f.read()
        if hashlib.sha256(raw).hexdigest() != self.sha.get(name):
            raise ValueError(f"{name}: not the manifest's answer")
        return json.loads(raw)["result"]


def q(x):
    return int(x, 16)


def recount(folder, n):
    b = folder.result(f"{n}.block.json.gz")
    rs = folder.result(f"{n}.receipts.json.gz")
    parent = folder.result(f"{n - 1}.block.json.gz")
    base = q(b["baseFeePerGas"])
    used, limit = q(b["gasUsed"]), q(b["gasLimit"])
    txs = b["transactions"]
    priority = 0
    blob_prices = set()
    for t, r in zip(txs, rs, strict=True):
        priority += (q(r["effectiveGasPrice"]) - base) * q(r["gasUsed"])
        if r.get("blobGasPrice"):
            blob_prices.add(q(r["blobGasPrice"]))
    excess = q(b["excessBlobGas"])
    if len(blob_prices) > 1:
        raise ValueError(f"{n}: receipts disagree on the blob gas price")
    blob_fee = blob_prices.pop() if blob_prices else fake_exponential(1, excess, BPO2_FRACTION)
    blob_used = q(b["blobGasUsed"])
    return {
        "number": q(b["number"]),
        "hash": b["hash"],
        "time": q(b["timestamp"]),
        "interval_secs": q(b["timestamp"]) - q(parent["timestamp"]),
        "txs": len(txs),
        "failed_txs": sum(1 for r in rs if r["status"] == "0x0"),
        # A reverted transaction creates no contract and sends no ether (P4A-R2-1): read from the
        # receipts' own `status` and `contractAddress`, not from the recipe's decoding.
        "contract_creations": sum(1 for r in rs if r["status"] == "0x1" and r.get("contractAddress")),
        "gas_used": used,
        "gas_limit": limit,
        "fullness_ppm": used * 1_000_000 // limit,
        "base_fee_wei": base,
        "burned_wei": base * used,
        "priority_fees_wei": priority,
        "blob_txs": sum(1 for t in txs if t["type"] == "0x3"),
        "blob_gas_used": blob_used,
        "excess_blob_gas": excess,
        "blob_base_fee_wei": blob_fee,
        "blob_fee_wei": blob_fee * blob_used,
        "value_wei": sum(q(t["value"]) for t, r in zip(txs, rs, strict=True) if r["status"] == "0x1"),
    }


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    with open(sys.argv[1]) as f:
        doc = json.load(f)
    folder = Folder(sys.argv[2])
    rows = [decode_leaf(l) for d in doc["days"] for l in d["leaves"]]
    if [r["number"] for r in rows] != list(range(doc["from"], doc["to"] + 1)):
        print("the leaves are not the window's blocks in order")
        sys.exit(1)
    diffs = 0
    for r in rows:
        want = recount(folder, r["number"])
        for name, _ in FIELDS:
            if r[name] != want[name]:
                diffs += 1
                print(f"{r['number']} {name}: recipe {r[name]}, recount {want[name]}")
    if diffs:
        print(f"recount: {diffs} differences over {len(rows)} blocks")
        sys.exit(1)
    print(f"recount: all {len(FIELDS)} fields of all {len(rows)} blocks match")


if __name__ == "__main__":
    main()
