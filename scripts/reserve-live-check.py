#!/usr/bin/env python3
"""Live checks of the reserve method against published figures (ADR-0034, docs/ops/indexer-integration.md).

  okx <por-btc-rows.csv> <scantxoutset.json> <flows.jsonl> <labels.json>
      OKX proof-of-reserves snapshot (the BTC rows of the file from
      https://www.okx.com/v2/asset/audit/list): each address's balance at the snapshot height is
      our scan at the tip minus the address's flows after the snapshot height up to the tip
      (scripts/stream-reserve-flows.py). Compared with OKX's declared amount per address, and
      summed over all addresses and over the ones in label set v2 (OKX's signed subset).
  bitb <wallets.json> <scantxoutset.json> <labels.json>
      Bitwise BITB wallet list (address, balance, active; as served on https://bitbetf.com/)
      against our scan at the tip, per address and in total.

Read-only, prints JSON. Nothing is tuned: differences are reported as found.
"""
import csv
import json
import sys
from decimal import Decimal


def sats(v):
    return int(Decimal(str(v)) * 100000000)


def scan_by_address(path):
    d = json.load(open(path))
    out = {}
    for u in d["unspents"]:
        a = u["desc"][5 : u["desc"].index(")")]
        out[a] = out.get(a, 0) + sats(u["amount"])
    return d["height"], d["bestblock"], out


def okx(rows_csv, scan_json, flows_jsonl, labels_json):
    rows = list(csv.reader(open(rows_csv)))
    heights = {r[3] for r in rows}
    if len(heights) != 1:
        raise SystemExit(f"rows from several heights: {heights}")
    h = int(heights.pop())
    declared = {}
    for r in rows:
        declared[r[4]] = declared.get(r[4], 0) + sats(r[5])
    tip, block, scan = scan_by_address(scan_json)
    net = {}
    seen = set()
    for line in open(flows_jsonl):
        b = json.loads(line)
        if h < b["height"] <= tip and b["height"] not in seen:
            seen.add(b["height"])
            for a, (rx, tx) in b["flows"].items():
                net[a] = net.get(a, 0) + rx - tx
    if len(seen) != tip - h:
        raise SystemExit(f"flows cover {len(seen)} of {tip - h} blocks")
    v2 = {l["address"][4:] for l in json.load(open(labels_json))["labels"] if l["entity"] == "entity:okx"}
    ours = {a: scan.get(a, 0) - net.get(a, 0) for a in declared}

    def summary(addrs):
        d = sum(declared[a] for a in addrs)
        o = sum(ours[a] for a in addrs)
        exact = sum(1 for a in addrs if declared[a] == ours[a])
        return {
            "addresses": len(addrs),
            "declared_sats": str(d),
            "ours_sats": str(o),
            "difference_sats": str(o - d),
            "difference_ppm": (o - d) * 1_000_000 // d if d else None,
            "addresses_exact": exact,
            "addresses_differing": len(addrs) - exact,
        }

    allset = sorted(declared)
    signed_v2 = [a for a in allset if a in v2]
    diffs = sorted(((ours[a] - declared[a], a) for a in allset if ours[a] != declared[a]), key=lambda x: -abs(x[0]))
    v2_now = sum(scan.get(a, 0) for a in v2)
    print(json.dumps({
        "snapshot_height": h,
        "scan_height": tip,
        "scan_block": block,
        "flows_blocks": len(seen),
        "all_por_addresses": summary(allset),
        "in_label_set_v2": summary(signed_v2),
        "largest_differences": [{"address": a, "ours_minus_declared_sats": str(d)} for d, a in diffs[:5]],
        "label_set_v2_okx_at_scan_sats": str(v2_now),
    }, indent=1))


def bitb(wallets_json, scan_json, labels_json):
    w = json.load(open(wallets_json))
    tip, block, scan = scan_by_address(scan_json)
    v2 = {l["address"][4:] for l in json.load(open(labels_json))["labels"] if l["entity"] == "entity:bitwise-bitb"}
    stated = {x["address"]: sats(x["balance_btc"]) for x in w}
    exact = [a for a in stated if stated[a] == scan.get(a, 0)]
    print(json.dumps({
        "scan_height": tip,
        "scan_block": block,
        "page_addresses": len(stated),
        "page_active": sum(1 for x in w if x["active"]),
        "page_in_label_set_v2": sum(1 for a in stated if a in v2),
        "stated_total_sats": str(sum(stated.values())),
        "ours_page_addresses_sats": str(sum(scan.get(a, 0) for a in stated)),
        "ours_all_v2_bitb_sats": str(sum(scan.get(a, 0) for a in v2)),
        "addresses_exact": len(exact),
        "v2_addresses_not_on_page_holding": sum(1 for a in v2 if a not in stated and scan.get(a, 0)),
    }, indent=1))


if __name__ == "__main__":
    cmds = {"okx": okx, "bitb": bitb}
    if len(sys.argv) < 2 or sys.argv[1] not in cmds:
        raise SystemExit(__doc__)
    cmds[sys.argv[1]](*sys.argv[2:])
