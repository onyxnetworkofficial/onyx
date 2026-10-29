#!/usr/bin/env python3
"""Free disk in a Cargo target: keep the newest build of each crate, list or delete older duplicates."""
import os, re, sys, time
# Usage: scripts/sweep-target.py <deps dir> [--delete]. For each (crate name, kind) keep the newest file; older
# duplicates (old hashes left by earlier builds) are listed or deleted. Files changed in the last
# 30 minutes are never touched.
d, delete = sys.argv[1], '--delete' in sys.argv
groups = {}
for f in os.listdir(d):
    p = os.path.join(d, f)
    if not os.path.isfile(p): continue
    m = re.match(r'^(lib)?(.+?)-([0-9a-f]{16})(\..*)?$', f)
    if not m: continue
    key = (m.group(1) or '', m.group(2), m.group(4) or '')
    groups.setdefault(key, []).append((os.path.getmtime(p), p))
now, total, n = time.time(), 0, 0
for key, files in groups.items():
    files.sort(reverse=True)
    for mt, p in files[1:]:
        if now - mt < 1800: continue
        s = os.path.getsize(p); total += s; n += 1
        if delete: os.remove(p)
print(f"{d}: {n} old duplicate files, {total/1e9:.1f} GB {'deleted' if delete else 'would be freed'}")
