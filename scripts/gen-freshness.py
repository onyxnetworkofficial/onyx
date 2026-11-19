#!/usr/bin/env python3
"""Write how fresh each website dataset is (pre-public loop PP11): apps/web/static/data/freshness.json.

Each dataset card on Browse data shows "Data as of" from this file, read from the very data files
the site ships, so the date can never disagree with the values below it. Where the hosted node
serves a round of another day than the one the page ships, the entry holds both (`nodeAsOf`, from
level2/ladders-base-sepolia.json) and the card says which day is which. scripts/build-web.sh runs
it after it assembles static/data (a build folder, not in git), so the file is made fresh with
every build. `--check` only compares an existing file with what the data files give.
Usage: python3 -I scripts/gen-freshness.py [--check]
"""
import datetime, json, os, sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
STATIC = os.path.join(ROOT, "apps", "web", "static")
OUT = os.path.join(STATIC, "data", "freshness.json")


def load(rel):
    with open(os.path.join(STATIC, rel), encoding="utf-8") as f:
        return json.load(f)


def day(unix):
    return datetime.datetime.fromtimestamp(int(unix), datetime.timezone.utc).strftime("%Y-%m-%d")


def build():
    demo = load("data/demo.json")
    live = day(demo["live"]["blocks"][-1]["time"])
    sec = load("level2/sec-holdings.json")
    etf = load("level2/etf-holdings.json")
    eth = load("onchain/eth-chain-stats.json")
    # Dataset id (apps/web/src/lib/catalog.ts) -> the newest date the shipped data holds, and
    # what that date is. Company reserves have none here: their file holds block heights, and it
    # is left out of the public copy (docs/ops/public-snapshot-exclude.txt).
    d = {
        "btc.blocks": (live, "newest block"),
        "btc.chain-stats": (load("onchain/btc-chain-stats.json")["days"][-1]["date"], "newest day"),
        "btc.entity-flow": (live, "newest block"),
        "btc.pools": (load("history/pools-week.json")["days"][-1]["date"], "newest day"),
        "eth.chain-stats": (day(eth["last_time"]), "newest block"),
        "us.treasury.total_public_debt_outstanding": (load("level2/debt-to-penny.json")["feed"]["record_date"], "record date"),
        "usgs.earthquakes.m5_count": (load("level2/usgs-quakes.json")["feed"]["record_date"], "day counted"),
        "noaa.ghcnd.usw00003017.tmax": (load("level2/ncei-tmax.json")["feed"]["record_date"], "day measured"),
        "sec.crypto-units": (max(c["feed"]["period_end"] for c in sec["companies"]), "newest filing period"),
        "sec.etf-btc": (day(etf["window_start"]), "observed"),
    }
    # Contributor loop CL1: the hosted node's other level-2 series, dated by the subject of the round the
    # client check accepted (apps/web/static/level2/ladders-base-sepolia.json).
    ladder_ids = {
        "us.treasury.tga_closing_balance": "record date",
        "noaa.ghcnd.usw00094728.tmax": "day measured",
        "noaa.ghcnd.usw00094728.prcp": "day measured",
        "noaa.ghcnd.usw00003017.prcp": "day measured",
        "usgs.earthquakes.la300km_m2_5_count": "day counted",
        "us.treasury.tga_total_deposits": "record date",  # CL3, the first manifest source
    }
    # Newcomer test 2026-11-18: a dataset can have two records, the recorded round this page ships
    # and checks (its `asOf` above, for example earthquakes of 2026-07-08) and another round the
    # hosted node serves (2026-07-19 in the same ladders file). They are different days of one
    # series, so the entry names the node's day too (`nodeAsOf`) and the card says which is which.
    node = {}
    for v in load("level2/ladders-base-sepolia.json")["values"]:
        served = v["subject"].rsplit(":", 1)[1]
        if v["metric"] in ladder_ids:
            d[v["metric"]] = (served, ladder_ids[v["metric"]])
        elif v["metric"] in d and d[v["metric"]][0] != served:
            node[v["metric"]] = served
    missing = sorted(set(ladder_ids) - set(d))
    if missing:
        raise SystemExit(f"no ladder for {missing}")
    for date in [date for date, _ in d.values()] + list(node.values()):
        datetime.date.fromisoformat(date)  # every date is a real calendar date
    return {
        "version": 1,
        "generated_by": "python3 -I scripts/gen-freshness.py",
        "datasets": {
            k: {"asOf": date, "what": what, **({"nodeAsOf": node[k]} if k in node else {})}
            for k, (date, what) in sorted(d.items())
        },
    }


def main():
    text = json.dumps(build(), indent=1) + "\n"
    if sys.argv[1:] == ["--check"]:
        try:
            with open(OUT, encoding="utf-8") as f:
                have = f.read()
        except FileNotFoundError:
            have = ""
        if have != text:
            print("freshness: apps/web/static/data/freshness.json is stale: run python3 -I scripts/gen-freshness.py")
            sys.exit(1)
        print(f"freshness ok: {len(build()['datasets'])} datasets")
        return
    if sys.argv[1:]:
        raise SystemExit("usage: gen-freshness.py [--check]")
    with open(OUT, "w", encoding="utf-8") as f:
        f.write(text)
    print(f"freshness: wrote {os.path.relpath(OUT, ROOT)}")


if __name__ == "__main__":
    main()
