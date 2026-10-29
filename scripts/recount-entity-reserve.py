#!/usr/bin/env python3
"""Independent recount for the reserve recipe (ADR-0034): Python, not the Rust recipe or adapter.

Subcommands (all amounts integer satoshis, printed as decimal strings):

  snapshot <scantxoutset.json> <labels.json> <out.json> <method>
      Turn a `bitcoin-cli scantxoutset start [addr(...)...]` result into the committed per-address
      snapshot (fixtures/reserves/scan-<height>.json): only label-set addresses, only addresses
      holding something. Extra scanned addresses (e.g. a PoR list) are left out.
  derive <snapshot.json> <labels.json> <flows.jsonl> <base_height> <out.json>
      Base at <base_height> = snapshot totals minus each entity's net flow over the blocks after
      <base_height> up to the snapshot height (flows from scripts/stream-reserve-flows.py).
      Checked address by address first (R-RES-NEG-2): walking back from the scan block by block,
      each address's balance after every block (its scan minus its own flows after that block)
      must not be negative, also when its entity's total would not be (RES-9: at every height,
      not only at the base). A flow at an address outside the label set is refused, as in Rust
      (`reserve::derive_base_by_address`).
  shard <block dir | fixture.json> <labels.json> <base.json> <from> <to>
      Recount one btc-entity-reserve shard from getblock-3 JSON (<h>.v3.json.gz) or a committed
      fixture file (fixtures/btc/blocks-*.json): <base.json> is
      {"base_height", "base": [{"entity", "balance_sats"}]}. Prints the rows as JSON. Spends are
      applied before outputs per transaction and a negative balance is an error, as in the recipe.
  reconcile <earlier.json> <later.json> <flows.jsonl>
      Per address: earlier + received - sent over (earlier, later] must equal later.

Address decoding is the stream script's own decoder, not a port of the recipe's.
"""
import gzip
import importlib.util
import json
import os
import sys
from decimal import Decimal

sys.dont_write_bytecode = True
_spec = importlib.util.spec_from_file_location(
    "stream", os.path.join(os.path.dirname(os.path.abspath(__file__)), "stream-reserve-flows.py")
)
stream = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(stream)


def sats(v):
    return int(Decimal(str(v)) * 100000000)


def labels(path):
    d = json.load(open(path))
    return d, {l["address"]: l["entity"] for l in d["labels"]}


def load_blocks(path, lo, hi):
    """Per block (height, {address: (received, sent)}) over heights lo..hi inclusive, in height
    order; every height must be present."""
    seen = {}
    for line in open(path):
        r = json.loads(line)
        h = r["height"]
        if lo <= h <= hi and h not in seen:
            seen[h] = r["flows"]
    missing = [h for h in range(lo, hi + 1) if h not in seen]
    if missing:
        raise SystemExit(f"flows file misses {len(missing)} blocks, first {missing[0]}")
    return [(h, seen[h]) for h in range(lo, hi + 1)]


def load_flows(path, lo, hi):
    """Per address (received, sent) over heights lo..hi inclusive; every height must be present."""
    out = {}
    for _, flows in load_blocks(path, lo, hi):
        for a, (rx, tx) in flows.items():
            o = out.setdefault(a, [0, 0])
            o[0] += rx
            o[1] += tx
    return out


def cmd_snapshot(scan_path, labels_path, out, method):
    d, owner = labels(labels_path)
    scan = json.load(open(scan_path))
    if not scan.get("success"):
        raise SystemExit("scan did not succeed")
    rows = {}
    for u in scan["unspents"]:
        desc = u["desc"]
        if not desc.startswith("addr("):
            raise SystemExit(f"unexpected descriptor {desc}")
        a = "btc:" + desc[5 : desc.index(")")]
        if a not in owner:
            continue
        r = rows.setdefault(a, {"address": a, "entity": owner[a], "utxos": 0, "sats": 0})
        r["utxos"] += 1
        r["sats"] += sats(u["amount"])
    content = os.environ.get("ONYX_LABEL_SET_HASH")
    if not content:
        raise SystemExit("set ONYX_LABEL_SET_HASH (cargo run -p onyx-source-btc --example export-reserves -- label-hash)")
    doc = {
        "version": 1,
        "method": method,
        "height": scan["height"],
        "block_hash": scan["bestblock"],
        "label_set_name": d["name"],
        "label_set_version": d["version"],
        "label_set_hash": content,
        "addresses_scanned": len(d["labels"]),
        "rows": [dict(r, sats=str(r["sats"])) for _, r in sorted(rows.items())],
    }
    json.dump(doc, open(out, "w"), indent=1)
    open(out, "a").write("\n")
    tot = {}
    for r in rows.values():
        tot[r["entity"]] = tot.get(r["entity"], 0) + r["sats"]
    print(json.dumps({"height": scan["height"], "rows": len(rows), "totals": {k: str(v) for k, v in sorted(tot.items())}}))


def entity_totals(snap, entities):
    t = {e: 0 for e in entities}
    for r in snap["rows"]:
        t[r["entity"]] += int(r["sats"])
    return t


def cmd_derive(snap_path, labels_path, flows_path, base_height, out):
    d, owner = labels(labels_path)
    snap = json.load(open(snap_path))
    base_height = int(base_height)
    per_block = load_blocks(flows_path, base_height + 1, snap["height"])
    t = entity_totals(snap, sorted(set(owner.values())))
    # Walk back from the scan, block by block: an address's balance after every block must not
    # be negative (RES-9), and every flow must be at a label-set address (as in Rust).
    bal = {r["address"]: int(r["sats"]) for r in snap["rows"]}
    net = {e: 0 for e in t}
    for h, flows in reversed(per_block):
        for a, (rx, tx) in flows.items():
            e = owner.get("btc:" + a)
            if not e:
                raise SystemExit(f"btc:{a}: flow at an address outside the label set")
            net[e] += rx - tx
            b = bal.get("btc:" + a, 0) - rx + tx
            if b < 0:
                raise SystemExit(f"btc:{a}: scan minus its own flows is negative after block {h - 1}")
            bal["btc:" + a] = b
    base = []
    for e in sorted(t):
        b = t[e] - net[e]
        if b < 0:
            raise SystemExit(f"{e}: scan minus flows is negative")
        base.append({"entity": e, "balance_sats": str(b)})
    doc = {
        "version": 1,
        "base_height": base_height,
        "snapshot": os.path.basename(snap_path),
        "snapshot_height": snap["height"],
        "derivation": f"snapshot totals minus net flows over {base_height + 1}-{snap['height']} "
        "(scripts/stream-reserve-flows.py, scripts/recount-entity-reserve.py derive); the recipe "
        "chain from this base must land on the snapshot exactly (examples/export-reserves.rs)",
        "net_sats": {e: str(net[e]) for e in sorted(net)},
        "base": base,
    }
    json.dump(doc, open(out, "w"), indent=1)
    open(out, "a").write("\n")
    print(json.dumps(doc["base"]))


def blocks(source, lo, hi):
    """(height, [(spent [(script hex, sats)], created [...])]) per block: from getblock-3 files
    (<dir>/<h>.v3.json.gz) or from a committed fixture file (fixtures/btc/blocks-*.json)."""
    if os.path.isdir(source):
        for h in range(lo, hi + 1):
            blk = json.load(gzip.open(os.path.join(source, f"{h}.v3.json.gz")))
            if blk["height"] != h:
                raise SystemExit(f"block file {h} holds height {blk['height']}")
            yield h, [
                (
                    [(i["prevout"]["scriptPubKey"]["hex"], sats(i["prevout"]["value"])) for i in t["vin"] if "prevout" in i],
                    [(o["scriptPubKey"]["hex"], sats(o["value"])) for o in t["vout"]],
                )
                for t in blk["tx"]
            ]
    else:
        got = [b for b in json.load(open(source))["blocks"] if lo <= b["height"] <= hi]
        if [b["height"] for b in got] != list(range(lo, hi + 1)):
            raise SystemExit("fixture does not hold the range")
        for b in got:
            yield b["height"], [
                ([(v[2], v[3]) for v in t.get("vin", []) if len(v) >= 4], [(v[0], v[1]) for v in t["vout"]])
                for t in b["txs"]
            ]


def cmd_shard(block_dir, labels_path, base_path, lo, hi):
    d, owner = labels(labels_path)
    script_owner = {stream.script_hex(a[4:]): e for a, e in owner.items()}
    b = json.load(open(base_path)) if os.path.exists(base_path) else json.loads(base_path)
    base_height = int(b["base_height"])
    lo, hi = int(lo), int(hi)
    if base_height + 1 != lo:
        raise SystemExit("stale base")
    st = {x["entity"]: {"bal": int(x["balance_sats"]), "rx": 0, "tx": 0, "n": 0, "base": int(x["balance_sats"])} for x in b["base"]}
    if sorted(st) != sorted(set(owner.values())):
        raise SystemExit("base does not name exactly the labelled entities")
    for h, txs in blocks(block_dir, lo, hi):
        for vin, vout in txs:
            touched = set()
            for script, v in vin:
                e = script_owner.get(script)
                if e:
                    st[e]["tx"] += v
                    st[e]["bal"] -= v
                    if st[e]["bal"] < 0:
                        raise SystemExit(f"{e}: negative balance at {h}")
                    touched.add(e)
            for script, v in vout:
                e = script_owner.get(script)
                if e:
                    st[e]["rx"] += v
                    st[e]["bal"] += v
                    touched.add(e)
            for e in touched:
                st[e]["n"] += 1
    rows = [
        {
            "entity": e,
            "base_height": base_height,
            "height": hi,
            "base_sats": str(s["base"]),
            "received_sats": str(s["rx"]),
            "sent_sats": str(s["tx"]),
            "tx_count": s["n"],
            "balance_sats": str(s["bal"]),
        }
        for e, s in sorted(st.items())
    ]
    print(json.dumps(rows))


def cmd_reconcile(a_path, b_path, flows_path):
    a, b = json.load(open(a_path)), json.load(open(b_path))
    if a["label_set_hash"] != b["label_set_hash"]:
        raise SystemExit("scans of different label sets")
    flows = load_flows(flows_path, a["height"] + 1, b["height"])
    av = {r["address"]: int(r["sats"]) for r in a["rows"]}
    bv = {r["address"]: int(r["sats"]) for r in b["rows"]}
    addrs = set(av) | set(bv) | {"btc:" + x for x in flows}
    bad = []
    for x in sorted(addrs):
        rx, tx = flows.get(x[4:], (0, 0))
        if av.get(x, 0) + rx - tx != bv.get(x, 0):
            bad.append(x)
    print(json.dumps({"from": a["height"], "to": b["height"], "addresses": len(addrs), "disagree": len(bad), "first": bad[:5]}))
    if bad:
        raise SystemExit(1)


if __name__ == "__main__":
    cmds = {"snapshot": cmd_snapshot, "derive": cmd_derive, "shard": cmd_shard, "reconcile": cmd_reconcile}
    if len(sys.argv) < 2 or sys.argv[1] not in cmds:
        raise SystemExit(__doc__)
    cmds[sys.argv[1]](*sys.argv[2:])
