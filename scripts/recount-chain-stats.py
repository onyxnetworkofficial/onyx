#!/usr/bin/env python3
"""Independent recount of `btc-chain-stats` (ADR-0043, docs/spec/recipes.md R-CHAIN-2).

Reads the node's own verbosity-3 JSON of every block (`<height>.v3.json.gz`, as
scripts/fetch-btc-node.sh and scripts/refresh-onchain.py write it) and recomputes each
block's row from the node's fields: block `size` and `weight`, each transaction's `fee`,
`vsize`, `weight` and `size`, prevout `value`, `height` and `scriptPubKey` (its `type` for
Taproot). The recipe computes the same row from raw bytes (sizes) and the prevouts; this
script never reads the raw bytes, so a size, fee or vsize bug in either shows up here.
Amounts are parsed as decimals, never floats.

Usage: recount-chain-stats.py <btc-chain-stats.json> <dir>[:<dir>...] [-j N]
Prints one line per differing field and, last, a summary line; exit 1 on any difference.
"""
import decimal
import gzip
import json
import os
import struct
import sys
from multiprocessing import Pool

SAT = decimal.Decimal(100_000_000)
FIELDS = [
    ("height", "Q"), ("hash", "32s"), ("time", "I"), ("interval_secs", "q"), ("bits", "I"),
    ("txs", "Q"), ("inputs", "Q"), ("outputs", "Q"), ("size_bytes", "Q"), ("weight_wu", "Q"),
    ("subsidy_sats", "Q"), ("fees_sats", "Q"), ("coinbase_sats", "Q"), ("issued_sats", "q"),
    ("transfer_sats", "u128"), ("fee_rate_median_msat_vb", "Q"), ("fee_rate_avg_msat_vb", "Q"),
    ("segwit_txs", "Q"), ("taproot_inputs", "Q"), ("taproot_outputs", "Q"),
    ("active_scripts", "Q"), ("coin_blocks_destroyed", "u128"),
]


def decode_leaf(hexstr):
    """Borsh OutputRow {key: Vec<u8>, value: Vec<u8>} holding a ChainStatsRow."""
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
    row["hash"] = row["hash"].hex()
    if key != struct.pack(">Q", row["height"]):
        raise ValueError("key is not the height")
    return row


def sats(x):
    d = decimal.Decimal(x) * SAT
    if d != d.to_integral_value():
        raise ValueError(f"amount {x} has more than 8 decimals")
    return int(d)


def subsidy(h):
    n = h // 210_000
    return 0 if n >= 64 else (50 * 100_000_000) >> n


def unspendable(script_hex):
    return script_hex.startswith("6a") or len(script_hex) // 2 > 10_000


def find(dirs, name):
    for d in dirs:
        p = os.path.join(d, name)
        if os.path.isfile(p):
            return p
    return None


def load(dirs, h):
    p = find(dirs, f"{h}.v3.json.gz")
    if p is None:
        return None
    with gzip.open(p, "rt") as f:
        return json.load(f, parse_float=decimal.Decimal)


def parent_time(dirs, h):
    """Header time of block h - 1: from its JSON when present, else its raw header."""
    b = load(dirs, h - 1)
    if b is not None:
        return b["time"]
    p = find(dirs, f"{h - 1}.raw.hex")
    with open(p) as f:
        head = bytes.fromhex(f.read(160))
    return struct.unpack_from("<I", head, 68)[0]


def recount(job):
    dirs, h = job
    b = load(dirs, h)
    txs = b["tx"]
    r = {"height": b["height"], "hash": b["hash"], "time": b["time"], "bits": int(b["bits"], 16),
         "txs": len(txs), "size_bytes": b["size"], "weight_wu": b["weight"],
         "subsidy_sats": subsidy(h)}
    r["interval_secs"] = b["time"] - parent_time(dirs, h)
    inputs = outputs = fees = transfer = vsizes = segwit = tr_in = tr_out = cbd = 0
    rates, scripts = [], set()
    coinbase = 0
    for k, t in enumerate(txs):
        outputs += len(t["vout"])
        out_sum = 0
        for o in t["vout"]:
            out_sum += sats(o["value"])
            spk = o["scriptPubKey"]
            tr_out += spk.get("type") == "witness_v1_taproot"
            if not unspendable(spk["hex"]):
                scripts.add(spk["hex"])
        if k == 0:
            coinbase = out_sum
            continue
        inputs += len(t["vin"])
        for v in t["vin"]:
            p = v["prevout"]
            cbd += sats(p["value"]) * (h - p["height"])
            tr_in += p["scriptPubKey"].get("type") == "witness_v1_taproot"
            if not unspendable(p["scriptPubKey"]["hex"]):
                scripts.add(p["scriptPubKey"]["hex"])
        fee = sats(t["fee"])
        fees += fee
        transfer += out_sum
        vsizes += t["vsize"]
        rates.append(fee * 1000 // t["vsize"])
        segwit += t["weight"] != 4 * t["size"]
    rates.sort()
    r.update(inputs=inputs, outputs=outputs, fees_sats=fees, coinbase_sats=coinbase,
             issued_sats=coinbase - fees, transfer_sats=transfer,
             fee_rate_median_msat_vb=rates[(len(rates) - 1) // 2] if rates else 0,
             fee_rate_avg_msat_vb=fees * 1000 // vsizes if vsizes else 0,
             segwit_txs=segwit, taproot_inputs=tr_in, taproot_outputs=tr_out,
             active_scripts=len(scripts), coin_blocks_destroyed=cbd)
    return h, r


def main():
    args = sys.argv[1:]
    jobs_n = 1
    if "-j" in args:
        i = args.index("-j")
        jobs_n = int(args[i + 1])
        del args[i:i + 2]
    if len(args) != 2:
        sys.exit(__doc__)
    doc = json.load(open(args[0]))
    dirs = args[1].split(":")
    rows = {}
    for d in doc["days"]:
        for leaf in d["leaves"]:
            row = decode_leaf(leaf)
            rows[row["height"]] = row
    heights = sorted(rows)
    if heights != list(range(doc["from"], doc["to"] + 1)):
        print("the file's leaves are not exactly its range")
        sys.exit(1)
    diffs = 0
    with Pool(jobs_n) as pool:
        for h, want in pool.imap(recount, [(dirs, h) for h in heights], chunksize=4):
            got = rows[h]
            for name, _ in FIELDS:
                if got[name] != want[name]:
                    diffs += 1
                    print(f"{h} {name}: recipe {got[name]}, recount {want[name]}")
    if diffs:
        print(f"{diffs} difference(s) over {len(heights)} blocks")
        sys.exit(1)
    print(f"scripts/recount-chain-stats.py: all {len(FIELDS)} fields of all {len(heights)} blocks "
          f"({heights[0]}-{heights[-1]}) match the node's own JSON")


if __name__ == "__main__":
    main()
