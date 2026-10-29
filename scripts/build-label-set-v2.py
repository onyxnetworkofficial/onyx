#!/usr/bin/env python3
"""Build fixtures/labels/test.onyx.entities.btc.v2.json from the entity indexer label files.

Usage: python3 -I scripts/build-label-set-v2.py <indexer>/fixtures/labels fixtures/labels/test.onyx.entities.btc.v2.json

       python3 -I scripts/build-label-set-v2.py --self-test

Reproducible: same inputs give the same bytes. Only company sources with first-party
evidence are labelled (fixtures/labels/PROVENANCE.md, v2). filings_gov.csv (names natural
persons) is read only for its address column, as a deny list: an address that a government
filing names (sanctions, seizures) is dropped even when a company signed or published it
(LBL-V2R-F1); none of its labels is ever used. Files never read: *_dune_thirdparty.csv
(third-party attribution,
redistribution terms unclear), mining_pool_payouts.csv (a coinbase payout shows where coins
came from, not who controls the address; many are individual miners' payouts: LBL-V2-F1),
unattributed_*, exchange_seeds_converted.csv and the rest.
"""
import csv
import json
import re
import sys
from pathlib import Path

# Confidence in ppm. A fixture convention, not a calibrated score (see PROVENANCE.md).
SIGNED = 990_000  # signature from the address key verified (OKX PoR)
PUBLISHED_CHECKED = 950_000  # entity published it; script, live file or balance re-checked
PUBLISHED = 900_000  # entity published it; nothing re-checked for this row (LBL-V2-F5)

# The grade comes from the indexer's per-row note, parsed with one strict pattern per
# source (LBL-V2R-F5): a note in any other format stops the build instead of grading the
# row silently.
# BitMEX: the indexer hashed the published redeem script and matched the address; output
# descriptors were not expanded.
BITMEX_NOTE = re.compile(
    r"PoR reserves snapshot \d{8}\.\.\d{8}; (?:"
    r"(?P<checked>published redeem script, hash160\(script\)==address verified here)"
    r"|published as an output descriptor \([A-Za-z0-9_]+\), NOT expanded or verified here)"
)
# Binance: the indexer states it re-downloaded the newest snapshot (20260801) and matched it;
# rows whose snapshot range ends there count as re-checked, older ones do not.
BINANCE_NOTE = re.compile(
    r"PoR HotCold snapshot \d{8}\.\.(?P<last>\d{8}), in \d+ of \d+ archived snapshots"
)
BINANCE_RECHECKED_SNAPSHOT = "20260801"
# BITB: on-chain balance equals the issuer-stated balance for the addresses live on the
# issuer page (65 rows); rotated-out rows were not balance-checked.
BITB_NOTE = re.compile(
    r"issuer wallet list \d{4}-\d{2}-\d{2}\.\.\d{4}-\d{2}-\d{2}; in \d+/\d+ snapshots; "
    r"(?:(?P<checked>live on issuer page)|no longer listed \(rotated out\)); onchain: \d+ txs"
)


def grade(pattern, note, address):
    """`PUBLISHED_CHECKED` or `PUBLISHED` from a note that must match `pattern` exactly."""
    m = pattern.fullmatch(note)
    if m is None:
        raise SystemExit(f"unknown note format for {address}: {note!r}")
    if pattern is BINANCE_NOTE:
        return PUBLISHED_CHECKED if m["last"] == BINANCE_RECHECKED_SNAPSHOT else PUBLISHED
    return PUBLISHED_CHECKED if m["checked"] else PUBLISHED


def rows(path):
    with open(path, newline="") as fh:
        return list(csv.DictReader(line for line in fh if not line.startswith("#")))


def filed_addresses(src):
    """Addresses a government filing names (deny list only; no label is read)."""
    return {r["address"] for r in rows(src / "filings_gov.csv")}


def build(src, report=print):
    out = {}
    deny = filed_addresses(src)
    dropped = []

    def add(address, entity, kind, ppm, origin):
        if address in deny:
            dropped.append((address, entity))
            return
        key = "btc:" + address
        if key in out:
            raise SystemExit(f"{address} labelled twice ({out[key]['entity']}, {entity}; {origin})")
        out[key] = {
            "address": key,
            "confidence_ppm": ppm,
            "entity": entity,
            "evidence": [],
            "kind": kind,
        }

    # OKX: every address whose PoR signature the indexer verified.
    for r in rows(src / "exchange_por-verified.csv"):
        if r["entity"] != "okx" or r["verdict"] != "verified":
            raise SystemExit(f"unexpected OKX verdict row {r['address']}")
        add(r["address"], "entity:okx", "Exchange", SIGNED, "okx por")

    # BitMEX and Binance: addresses the exchange published itself. Custodian rows
    # (Ceffu holding for Binance) are not exchange addresses and are left out.
    for r in rows(src / "exchange_por.csv"):
        ent, typ, ev, url = r["entity"], r["entity_type"], r["evidence"], r["source_url"]
        if ent == "okx":
            continue  # taken from the verified file above
        if ent == "bitmex":
            if ev != "published" or typ != "exchange":
                raise SystemExit(f"unexpected BitMEX row {r['address']}")
            if not url.startswith("https://s3-eu-west-1.amazonaws.com/public.bitmex.com/"):
                raise SystemExit(f"BitMEX row not from BitMEX's own bucket: {url}")
            add(r["address"], "entity:bitmex", "Exchange",
                grade(BITMEX_NOTE, r["note"], r["address"]), "bitmex por")
        elif ent == "binance":
            if ev != "published":
                raise SystemExit(f"unexpected Binance row {r['address']}")
            if not url.startswith("https://public.bnbstatic.com/static/proof-of-reserve/"):
                raise SystemExit(f"Binance row not from Binance's own host: {url}")
            if typ == "exchange_custodian":
                continue
            if typ != "exchange":
                raise SystemExit(f"unexpected Binance entity_type {typ}")
            add(r["address"], "entity:binance", "Exchange",
                grade(BINANCE_NOTE, r["note"], r["address"]), "binance por")
        else:
            raise SystemExit(f"unexpected exchange entity {ent}")

    # Bitwise BITB: the fund sponsor's own wallet list (live rows balance-matched on chain).
    for r in rows(src / "etf_custody.csv"):
        if r["entity"] != "BITB" or r["evidence"] != "published":
            raise SystemExit(f"unexpected ETF row {r['address']}")
        add(r["address"], "entity:bitwise-bitb", "Etf",
            grade(BITB_NOTE, r["note"], r["address"]), "bitb")

    report(f"dropped {len(dropped)} address(es) named in a government filing: "
           + ", ".join(f"{a} ({e})" for a, e in sorted(dropped)))
    labels = [out[k] for k in sorted(out)]
    return {"labels": labels, "name": "test.onyx.entities.btc", "version": 2}


def self_test():
    """The grader takes only the known note formats (LBL-V2R-F5)."""
    ok = [
        (BITMEX_NOTE, "PoR reserves snapshot 20240101..20260801; published redeem script, "
         "hash160(script)==address verified here", PUBLISHED_CHECKED),
        (BITMEX_NOTE, "PoR reserves snapshot 20240101..20260801; published as an output "
         "descriptor (wpkh), NOT expanded or verified here", PUBLISHED),
        (BINANCE_NOTE, "PoR HotCold snapshot 20250101..20260801, in 3 of 9 archived snapshots",
         PUBLISHED_CHECKED),
        (BINANCE_NOTE, "PoR HotCold snapshot 20250101..20260701, in 3 of 9 archived snapshots",
         PUBLISHED),
        (BITB_NOTE, "issuer wallet list 2024-02-09..2026-08-10; in 9/9 snapshots; "
         "live on issuer page; onchain: 12 txs", PUBLISHED_CHECKED),
        (BITB_NOTE, "issuer wallet list 2024-02-09..2026-08-10; in 2/9 snapshots; "
         "no longer listed (rotated out); onchain: 3 txs", PUBLISHED),
    ]
    for pattern, note, want in ok:
        assert grade(pattern, note, "a") == want, note
    bad = [
        (BITMEX_NOTE, "PoR reserves snapshot 20240101..20260801; hash160(script)==address "
         "verified here, published redeem script"),
        (BITMEX_NOTE, "published redeem script, hash160(script)==address verified here"),
        (BINANCE_NOTE, "PoR HotCold snapshot 20250101..20260801"),
        (BITB_NOTE, "issuer wallet list 2024-02-09..2026-08-10; live on issuer page"),
        (BITB_NOTE, ""),
    ]
    for pattern, note in bad:
        try:
            grade(pattern, note, "a")
        except SystemExit:
            continue
        raise AssertionError(f"graded an unknown note: {note!r}")
    print("self-test passed")


def main():
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    data = build(Path(sys.argv[1]))
    Path(sys.argv[2]).write_text(json.dumps(data, indent=1, sort_keys=True) + "\n")
    print(f"wrote {len(data['labels'])} labels to {sys.argv[2]}")


if __name__ == "__main__":
    main()
