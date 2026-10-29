#!/usr/bin/env python3
"""Data and privacy audit scan (ROADMAP L8; docs/audit/2026-10-13-data-privacy.md).

Read-only. Looks for, in every scanned file (and inside `.gz` and `.b64.txt` files):
- names that may be persons: excluded upstream pool ids (name, tags) and every entity string of
  the indexer files that name people or are excluded (filings_gov, owners_dune_thirdparty,
  mining_pool_payouts);
- e-mail addresses, social-media profile links, coinbase handle phrases with an unreviewed token;
- Bitcoin addresses, classed as label-set, pool-allowlist, EXCLUDED:<source> (wBTC, cbBTC,
  filings, payouts, third-party lists, OKX PoR or BITB rows outside label set v2) or other.
Prints the source class of a name hit, never the name (set SHOW=1 for a private local review).

usage: python3 -I scripts/audit-privacy-scan.py <onyx-root> <the entity indexer> <target>...
  target: "git" (every tracked file), "history" (every blob reachable from any ref), or a
  directory relative to <onyx-root> (every file under it, e.g. apps/web/build)
"""
import csv, gzip, json, os, re, subprocess, sys
from collections import Counter, defaultdict

onyx, idx, targets = sys.argv[1], sys.argv[2], sys.argv[3:]
lab = os.path.join(idx, "fixtures/labels")

def rows(name):
    with open(os.path.join(lab, name), newline="") as f:
        return list(csv.DictReader(l for l in f if not l.startswith("#")))

def addr(a):
    return a.split(":", 1)[1] if a.startswith("btc:") else a

# --- reference sets -------------------------------------------------------
label_addrs = set()
for n in ("test.onyx.entities.btc.v1.json", "test.onyx.entities.btc.v2.json"):
    for l in json.load(open(os.path.join(onyx, "fixtures/labels", n)))["labels"]:
        label_addrs.add(addr(l["address"]))
pool_addrs = set()
for p in json.load(open(os.path.join(onyx, "fixtures/pools/pool-allowlist.json"))):
    pool_addrs.update(p["addresses"])

excluded_addr_src = {}  # address -> source file (excluded sources only)
for n in ("exchange_por_wbtc.csv", "exchange_por_cbbtc.csv", "exchange_por_cbbtc_2026-08-16.csv",
          "filings_gov.csv", "mining_pool_payouts.csv", "unattributed_exchange_a.csv",
          "exchange_seeds_converted.csv", "exchange_dune_thirdparty.csv",
          "exchange_dune_binance_cold.csv", "government_dune_thirdparty.csv",
          "grayscale_dune_thirdparty.csv", "owners_dune_thirdparty.csv"):
    for r in rows(n):
        a = addr(r["address"].strip())
        if a and a not in label_addrs:
            excluded_addr_src.setdefault(a, n)
# OKX PoR / BITB addresses outside v2 (only totals may be quoted)
for n in ("exchange_por.csv", "etf_custody.csv"):
    for r in rows(n):
        a = addr(r["address"].strip())
        if a and a not in label_addrs:
            excluded_addr_src.setdefault(a, n + " (not in v2)")
okx_csv = os.path.join(lab, "okx-snapshot/okx_por_2026011716_v3.csv-btc.csv")
with open(okx_csv, newline="") as f:
    for line in f:
        for t in re.findall(r"\b(?:bc1[02-9ac-hj-np-z]{11,71}|[13][1-9A-HJ-NP-Za-km-z]{25,34})\b", line):
            if t not in label_addrs:
                excluded_addr_src.setdefault(t, "okx-snapshot (not in v2)")

# Names that may be persons: excluded pool ids (name, tags), and every
# entity string of the person-bearing indexer files.
pools_up = json.load(open(os.path.join(idx, "fixtures/groundtruth/pool-list.json")))
excl_ids = {13, 17, 21, 23, 30, 40, 52, 53, 65, 66, 69, 75, 89, 101, 109, 112, 115, 118, 131, 147, 148, 154}
allow_names = {p["name"].lower() for p in json.load(open(os.path.join(onyx, "fixtures/pools/pool-allowlist.json")))}
names = {}  # lowercase needle -> class
for p in pools_up:
    if p["id"] in excl_ids:
        for s in [p["name"], *p.get("tags", [])]:
            s = s.strip().strip("/").strip()
            if len(s) >= 4 and s.lower() not in allow_names:
                names.setdefault(s.lower(), f"excluded-pool id {p['id']}")
for n in ("filings_gov.csv", "owners_dune_thirdparty.csv", "mining_pool_payouts.csv"):
    for r in rows(n):
        e = (r.get("entity") or "").strip().lower()
        if len(e) >= 5 and e not in allow_names and e.replace("_", " ") not in allow_names:
            names.setdefault(e, n)
            names.setdefault(e.replace("_", " "), n)
# Generic person-ish patterns.
PHRASE = re.compile(rb"(?<![A-Za-z])mi" + rb"ned by[ \t]*[^\s\"'/`]*", re.I)
PHRASE_OK = {"", "example-user", "...", "<user>", "<handle>", "<x>", "<name>"}
for p_ in json.load(open(os.path.join(onyx, "fixtures/pools/pool-allowlist.json"))):
    for t_ in p_["tags"]:
        for m_ in re.findall(rb"(?<![A-Za-z])mi" + rb"ned by[ \t]*([^\s\"'/`]*)", t_.encode(), re.I):
            PHRASE_OK.add(m_.lower().decode())
# Bounded runs: an unbounded `+` backtracks quadratically over a long base64 line.
EMAIL = re.compile(rb"[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9.-]{1,253}\.[A-Za-z]{2,24}")
SOCIAL = re.compile(rb"(?<![A-Za-z0-9.])(?:twitter\.com|x\.com|t\.me|linkedin\.com/in|facebook\.com|instagram\.com|reddit\.com/u(?:ser)?)/[A-Za-z0-9_.-]+", re.I)
BTCADDR = re.compile(rb"(?<![A-Za-z0-9])(?:bc1[02-9ac-hj-np-z]{11,71}|[13][1-9A-HJ-NP-Za-km-z]{25,34})(?![A-Za-z0-9])")
EMAIL_OK = re.compile(rb"@(?:[a-z0-9.-]*\.)?example$|@f\.example$|@node\.example$|legal@chain\.link$|noreply@anthropic\.com$|@sample", re.I)

def files():
    for t in targets:
        if t == "history":
            objs = subprocess.run(["git", "-C", onyx, "rev-list", "--all", "--objects"], capture_output=True, check=True).stdout.decode().splitlines()
            seen = {}
            for l in objs:
                parts = l.split(" ", 1)
                if len(parts) == 2:
                    seen.setdefault(parts[0], parts[1])
            ids = "\n".join(seen).encode() + b"\n"
            out = subprocess.run(["git", "-C", onyx, "cat-file", "--batch-check=%(objectname) %(objecttype)"], input=ids, capture_output=True, check=True).stdout.decode().split("\n")
            for l in out:
                if l.endswith(" blob"):
                    oid = l.split()[0]
                    blob = subprocess.run(["git", "-C", onyx, "cat-file", "blob", oid], capture_output=True, check=True).stdout
                    yield "history", seen[oid] + "@" + oid[:10], blob
            continue
        if t == "git":
            out = subprocess.run(["git", "-C", onyx, "ls-files"], capture_output=True, check=True).stdout
            for f in out.decode().splitlines():
                yield "git", f, os.path.join(onyx, f)
        else:
            base = os.path.join(onyx, t)
            for d, _, fs in os.walk(base):
                for f in fs:
                    p = os.path.join(d, f)
                    yield t, os.path.relpath(p, base), p

def decoded(raw, path):
    out = [raw]
    if path.endswith(".gz"):
        try: out.append(gzip.decompress(raw))
        except Exception: pass
    if path.endswith(".b64.txt"):
        import base64
        try: out.append(base64.b64decode(b"".join(raw.split())))
        except Exception: pass
    return out

needles = sorted(names, key=len, reverse=True)
name_res = [(n, re.compile(rb"(?<![a-z0-9])" + re.escape(n.encode()) + rb"(?![a-z0-9])")) for n in needles]


def name_hits(low):
    # bytes.find first (fast), the boundary regex only for needles present.
    return {n for n, r in name_res if low.find(n.encode()) >= 0 and r.search(low)}
stats = Counter()
hits = defaultdict(list)
addr_class = defaultdict(Counter)
for tgt, rel, p in files():
    if isinstance(p, bytes):
        raw, p = p, rel
    elif os.path.isdir(p) or not os.path.exists(p):
        continue
    else:
        raw = open(p, "rb").read()
    stats[tgt] += 1
    for blob in decoded(raw, p):
        low = blob.lower()
        for m in name_hits(low):
            hits[(tgt, rel)].append(f"name[{names[m]}:{m if os.environ.get('SHOW') else ''}]")
        for m in set(EMAIL.findall(blob)) if b"@" in blob else ():
            if not EMAIL_OK.search(m):
                hits[(tgt, rel)].append("email " + m.decode(errors="replace"))
        for m in set(PHRASE.findall(blob)):
            t = m.lower().split(b"by", 1)[1].strip().decode(errors="replace")
            if t not in PHRASE_OK:
                hits[(tgt, rel)].append("handle-phrase token" + (" " + t if os.environ.get("SHOW") else ""))
        for m in set(SOCIAL.findall(blob)):
            hits[(tgt, rel)].append("social " + m.decode(errors="replace"))
        for m in set(BTCADDR.findall(blob)):
            a = m.decode()
            if a in label_addrs: c = "label-set"
            elif a in pool_addrs: c = "pool-allowlist"
            elif a in excluded_addr_src: c = "EXCLUDED:" + excluded_addr_src[a]
            else: c = "other"
            addr_class[(tgt, rel)][c] += 1
            if c.startswith("EXCLUDED"):
                hits[(tgt, rel)].append(f"address from {c} ({a[:8]}...)")

print("files scanned:", dict(stats))
print("person-name needles:", Counter(names.values()).most_common(), "excluded addresses:", len(excluded_addr_src))
print("\n# hits")
for k, v in sorted(hits.items()):
    print(k[0], k[1], sorted(Counter(v).items())[:12])
print("\n# btc address tokens per file (class counts)")
for k, v in sorted(addr_class.items()):
    print(k[0], k[1], dict(v))
