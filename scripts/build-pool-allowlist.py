#!/usr/bin/env python3
# Build fixtures/pools/pool-allowlist.json from the upstream bitcoin-data/mining-pools list
# (branch `generated`, MIT; ADR-0032, POOL-R1-F4). Reproducible: same upstream bytes, same output.
# Usage: python3 -I scripts/build-pool-allowlist.py <upstream pool-list.json> > fixtures/pools/pool-allowlist.json
# The upstream file itself is not committed: it names people (solo miners' handles). Only reviewed
# pool companies are copied, with the fields Onyx uses. Every upstream id gets one decision below.
import hashlib, json, sys

UPSTREAM_SHA256 = "ddc505c82dfa5868ce234be3ac0c05454bfbd6e1d34fd1e37f3004cde0a5ef0e"

# Excluded upstream ids and the reason class (no names here: some are people).
EXCLUDED = {
    21: "person", 30: "person", 40: "person", 52: "person", 53: "person",
    23: "no_company_site", 65: "no_company_site", 131: "no_company_site", 154: "no_company_site",
    66: "operator_handle", 75: "operator_handle", 115: "operator_handle", 118: "operator_handle",
}


def decision(p):
    """('allow', '') or ('exclude', reason class)."""
    if p["id"] in EXCLUDED:
        return "exclude", EXCLUDED[p["id"]]
    link = p.get("link") or ""
    if not link.startswith(("https://", "http://")) or "github.com" in link:
        return "exclude", "no_company_site"
    if not [t for t in p["tags"] if t] and not p["addresses"]:
        return "exclude", "no_rule"
    return "allow", ""


def main():
    raw = open(sys.argv[1], "rb").read()
    got = hashlib.sha256(raw).hexdigest()
    if got != UPSTREAM_SHA256:
        sys.exit(f"upstream sha256 {got} != pinned {UPSTREAM_SHA256}")
    pools = sorted(json.loads(raw), key=lambda p: p["id"])
    keep = [
        {"id": p["id"], "name": p["name"], "addresses": p["addresses"], "tags": p["tags"], "link": p["link"]}
        for p in pools
        if decision(p)[0] == "allow"
    ]
    if "--decisions" in sys.argv:
        for p in pools:
            d, why = decision(p)
            print(p["id"], d, why if d == "exclude" else p["name"])
        return
    sys.stdout.write(json.dumps(keep, indent=1, ensure_ascii=True) + "\n")


main()
