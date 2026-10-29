#!/usr/bin/env python3
"""Indexer bridge (ADR-0031): build label set v3 from the entity indexer label files.

Not the shipped v3: `examples/build-label-set-v3.rs` builds the dated label set v3 from the
issuers' own roster snapshots (ROADMAP P2a, scripts/build-rosters.py). This bridge's undated
output is never written as a fixture (`labels_bridge`).

Usage: python3 -I scripts/build-label-set-v3.py <indexer>/fixtures/labels <out.json>

       python3 -I scripts/build-label-set-v3.py --self-test

v3 = v2 (scripts/build-label-set-v2.py, unchanged: OKX signed PoR, BitMEX and Binance published
PoR, Bitwise BITB issuer list) plus every indexer source in CLEARED. CLEARED is empty today, so
v3 has exactly v2's rows and no v3 fixture is shipped: the site, the history and the API keep
running label set v2. A source enters CLEARED only once its redistribution terms are read and
recorded in fixtures/labels/PROVENANCE.md (hard limit: never ingest data whose license forbids
redistribution).

Excluded (EXCLUDED below, with the reason; the build stops if one is ever listed as cleared):
- exchange_por_wbtc.csv: wbtc.network's terms of service forbid reproducing or distributing its
  materials without written permission and forbid automated access (IDX-R1-F1).
- exchange_por_cbbtc*.csv: Coinbase's terms for the cbBTC reserve page are unread (the page
  refuses our fetches); the rows came from a Wayback capture and a person's relay (IDX-R1-F2).
- cluster expansions and `inferred` rows: heuristic, could label a person (the indexer measures
  24.3% unresolved false merges); *_dune_thirdparty.csv: third-party, terms unclear;
  mining_pool_payouts.csv: payouts often go to individual miners (LBL-V2-F1);
  exchange_seeds_converted.csv: Mt. Gox rows are relayed by a wiki; filings labels: natural
  persons (their addresses are used only as v2's deny list).
"""
import importlib.util
import json
import sys
from pathlib import Path

sys.dont_write_bytecode = True  # importing v2 below must not leave a __pycache__ in scripts/
HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("v2", HERE / "build-label-set-v2.py")
v2 = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(v2)

# Indexer label files whose redistribution terms are read and recorded. None yet.
CLEARED: dict[str, str] = {}

# Indexer label files Onyx never imports, with the reason (ADR-0031, PROVENANCE.md).
EXCLUDED = {
    "exchange_por_wbtc.csv": "wbtc.network terms forbid redistribution and automated access",
    "exchange_por_cbbtc.csv": "Coinbase cbBTC page terms unread; rows via a Wayback capture",
    "exchange_por_cbbtc_2026-08-16.csv": "Coinbase cbBTC page terms unread; relayed by a person",
    "mining_pool_payouts.csv": "payout addresses are often individual miners",
    "exchange_seeds_converted.csv": "Mt. Gox rows relayed by a wiki; the rest inferred",
    "filings_gov.csv": "names natural persons; addresses used only as a deny list",
}


def check_lists():
    clash = sorted(set(CLEARED) & set(EXCLUDED))
    if clash:
        raise SystemExit(f"excluded source listed as cleared: {', '.join(clash)}")
    third = sorted(n for n in CLEARED if "thirdparty" in n or n.startswith("unattributed"))
    if third:
        raise SystemExit(f"third-party or unattributed source listed as cleared: {', '.join(third)}")


def build(src, report=print):
    check_lists()
    base = v2.build(src, report=report)
    if CLEARED:
        raise SystemExit("cleared sources need an import step and a review (ADR-0031)")
    report("v3: no cleared indexer source beyond v2; rows equal v2's")
    return {"labels": base["labels"], "name": "test.onyx.entities.btc", "version": 3}


def self_test():
    v2.self_test()
    check_lists()
    for name in ("exchange_por_wbtc.csv", "exchange_por_cbbtc.csv"):
        CLEARED[name] = "test"
        try:
            check_lists()
        except SystemExit:
            continue
        finally:
            del CLEARED[name]
        raise AssertionError(f"cleared an excluded source: {name}")
    print("v3 self-test passed")


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
