#!/usr/bin/env python3
"""Every path DATA-LICENSE.md section 1 names must be covered by docs/ops/public-snapshot-exclude.txt
(scripts/public-snapshot.sh), so a new uncleared file cannot slip into a public snapshot.
usage: python3 -I scripts/public-snapshot-coverage.py <repo-root>"""
import fnmatch, pathlib, re, sys

root = pathlib.Path(sys.argv[1])
text = (root / "DATA-LICENSE.md").read_text(encoding="utf-8")
sec = text.split("\n## 1.", 1)[1].split("\n## 2.", 1)[0]
rows = []
for line in sec.splitlines():
    if line.startswith("| Source id"):
        break  # the second table lists sources, not paths
    if line.startswith("| ") and not line.startswith("| Path") and not line.startswith("|---"):
        rows.append(line.split("|")[1])
paths = []
for cell in rows:
    prev = None
    for tok in re.findall(r"`([^`]+)`", cell):
        if tok.startswith(".") and prev:  # `.v2.json` after `....v1.json`
            tok = re.sub(r"\.v\d+\.json$", "", prev) + tok
        elif "/" not in tok and prev:  # `btc-entity-flow-series-*.json` after a path in the same dir
            tok = str(pathlib.PurePosixPath(prev).parent / tok)
        if "/" in tok:
            paths.append(tok)
            prev = tok
pats = [l.strip() for l in (root / "docs/ops/public-snapshot-exclude.txt").read_text().splitlines() if l.strip() and not l.startswith("#")]

def covered(p):
    p = p.rstrip("/").replace("**", "*")
    return any(fnmatch.fnmatch(p, q) or p == q or p.startswith(q.rstrip("*")) and q.endswith("/*") for q in pats)

missing = [p for p in paths if not covered(p)]
for p in missing:
    print(f"DATA-LICENSE section 1 names {p}, which public-snapshot-exclude.txt does not cover")
print(f"snapshot exclude coverage: {len(paths)} paths in DATA-LICENSE section 1, {len(missing)} not covered")
sys.exit(1 if missing else 0)
