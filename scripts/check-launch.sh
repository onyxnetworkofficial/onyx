#!/usr/bin/env bash
# The launch checklist (docs/ops/launch-checklist.md, ROADMAP L10) stays checkable:
#   1. every relative Markdown link resolves to a file in the repository, and a `#anchor` on a
#      Markdown file to one of its headings (GitHub's slug rules, duplicates as -1, -2); links are
#      read from the whole file, so link text that wraps across lines is still checked;
#   2. every gate in section 1 is a checkbox line `- [ ] **LC-n ...` with an owner (the project owner, loop or
#      both), a state (OPEN, PARTIAL or VERIFIED) and at least one evidence link; gate ids are
#      unique; a gate is ticked exactly when its state is VERIFIED, and VERIFIED names a date and
#      links at least one file in this repository (an outside URL alone is not evidence);
#   3. the "Count:" line matches the gates (total, verified, open).
# Usage: scripts/check-launch.sh [file]   (default docs/ops/launch-checklist.md)
#        scripts/check-launch.sh --self-test   (plants each kind of mistake in a copy; each must fail)
set -euo pipefail
cd "$(dirname "$0")/.."
python3 -I - "$@" <<'PY'
import os, re, sys

DEFAULT = "docs/ops/launch-checklist.md"
LINK = re.compile(r"(?<!!)\[[^\]]*\]\(([^)\s]+)\)")
EXTERNAL = re.compile(r"^[a-z]+:")
GATE = re.compile(r"^- \[( |x)\] \*\*(LC-\d+)\b")
STATE = re.compile(r"State: (OPEN|PARTIAL|VERIFIED)\b")
OWNER = re.compile(r"Owner: (the project owner|loop)\b")
COUNT = re.compile(r"^Count: (\d+) gates, (\d+) verified, (\d+) open\b")


def slug(heading):
    s = heading.strip().lower()
    s = re.sub(r"[^\w\- ]", "", s)
    return s.replace(" ", "-")


def anchors(path, cache={}):
    if path not in cache:
        seen, out, fence = {}, set(), False
        with open(path, encoding="utf-8") as f:
            for line in f:
                if line.startswith("```"):
                    fence = not fence
                m = None if fence else re.match(r"^#{1,6} (.*?)#*\s*$", line)
                if m:
                    s = slug(m.group(1))
                    n = seen.get(s, 0)
                    seen[s] = n + 1
                    out.add(s if n == 0 else f"{s}-{n}")
        cache[path] = out
    return cache[path]


def gate_items(lines):
    """Gate entries of section 1: each checkbox line joined with its continuation lines."""
    items, cur, inside = [], None, False
    for i, line in enumerate(lines, 1):
        if line.startswith("## "):
            inside = line.startswith("## 1.")
        if not inside:
            continue
        if line.startswith("- ["):
            cur = [i, line.rstrip("\n")]
            items.append(cur)
        elif cur and line.startswith("  ") and line.strip():
            cur[1] += " " + line.strip()
        else:
            cur = None
    return items


def check(path, lines):
    bad, base = [], os.path.dirname(path)
    # Links over the whole file (text may wrap), with fenced code blanked out.
    text, fence = [], False
    for line in lines:
        if line.startswith("```"):
            fence = not fence
            text.append("\n")
        else:
            text.append("\n" if fence else line)
    text = "".join(text)
    for m in LINK.finditer(text):
        target, i = m.group(1), text.count("\n", 0, m.start()) + 1
        if EXTERNAL.match(target):
            continue
        file, _, frag = target.partition("#")
        dest = os.path.normpath(os.path.join(base, file)) if file else path
        if not os.path.exists(dest):
            bad.append(f"{path}:{i}: link to a missing file: {target}")
        elif frag and dest.endswith(".md") and frag not in anchors(dest):
            bad.append(f"{path}:{i}: no heading for #{frag} in {dest}")
        elif frag and not dest.endswith(".md"):
            bad.append(f"{path}:{i}: anchor on a non-Markdown file: {target}")
    ids, total, verified = set(), 0, 0
    for i, text in gate_items(lines):
        m = GATE.match(text)
        if not m:
            bad.append(f"{path}:{i}: a checkbox in section 1 that is not a `- [ ] **LC-n` gate")
            continue
        ticked, gid = m.group(1) == "x", m.group(2)
        total += 1
        if gid in ids:
            bad.append(f"{path}:{i}: duplicate gate {gid}")
        ids.add(gid)
        st = STATE.search(text)
        if not OWNER.search(text):
            bad.append(f"{path}:{i}: {gid} names no owner (Owner: the project owner or loop)")
        if not st:
            bad.append(f"{path}:{i}: {gid} has no state (State: OPEN, PARTIAL or VERIFIED)")
        if not LINK.search(text):
            bad.append(f"{path}:{i}: {gid} has no evidence link")
        is_ver = bool(st) and st.group(1) == "VERIFIED"
        verified += is_ver
        if ticked != is_ver:
            bad.append(f"{path}:{i}: {gid} is {'ticked' if ticked else 'unticked'} but its state is {st.group(1) if st else 'missing'}")
        if is_ver and not re.search(r"VERIFIED\s+\d{4}-\d{2}-\d{2}", text):
            bad.append(f"{path}:{i}: {gid} is VERIFIED without a date")
        if is_ver and all(EXTERNAL.match(t) for t in LINK.findall(text)):
            bad.append(f"{path}:{i}: {gid} is VERIFIED with only outside links as evidence")
    counts = [COUNT.match(l) for l in lines]
    counts = [c for c in counts if c]
    if total == 0:
        bad.append(f"{path}: no gates in section 1")
    if len(counts) != 1:
        bad.append(f"{path}: needs exactly one 'Count: N gates, V verified, O open' line")
    elif tuple(map(int, counts[0].groups())) != (total, verified, total - verified):
        bad.append(f"{path}: Count line says {counts[0].group(0)!r}; the gates are {total} gates, {verified} verified, {total - verified} open")
    return bad, total, verified


def load(path):
    with open(path, encoding="utf-8") as f:
        return f.readlines()


def self_test():
    lines = load(DEFAULT)
    base, _, _ = check(DEFAULT, lines)
    if base:
        print("\n".join(base))
        return 1
    first = next(i for i, l in enumerate(lines) if GATE.match(l) and "[ ]" in l)
    ver = next(i for i, l in enumerate(lines) if GATE.match(l) and "[x]" in l)
    def bump(L, dv):
        return [re.sub(r"^Count: (\d+) gates, (\d+) verified",
                       lambda m: f"Count: {int(m.group(1)) + 1} gates, {int(m.group(2)) + dv} verified", l)
                for l in L]
    outside_only = "- [x] **LC-99 Planted.** Owner: loop. State: VERIFIED 2026-10-15. Evidence: [x](https://example.com/x).\n"
    # name: (plant, a substring the refusal must contain)
    plants = {
        "missing file": (lambda L: L + ["See [x](no-such-file.md).\n"], "missing file"),
        "missing file, wrapped link text": (lambda L: L + ["See [a link whose text\n", "wraps](no-such-file.md).\n"], "missing file"),
        "missing anchor": (lambda L: L + ["See [x](deployment.md#no-such-heading).\n"], "no heading"),
        "anchor on code": (lambda L: L + ["See [x](../../scripts/check-all.sh#L3).\n"], "non-Markdown"),
        "ticked open gate": (lambda L: L[:first] + [L[first].replace("- [ ]", "- [x]", 1)] + L[first + 1:], "is ticked"),
        "unticked verified gate": (lambda L: L[:ver] + [L[ver].replace("- [x]", "- [ ]", 1)] + L[ver + 1:], "is unticked"),
        "no owner": (lambda L: [l.replace("Owner: ", "Who: ", 1) if i == first else l for i, l in enumerate(L)], "no owner"),
        "duplicate id": (lambda L: bump(L[:first] + [L[first]] + L[first:], 0), "duplicate"),
        "stale count": (lambda L: [re.sub(r"^Count: (\d+)", lambda m: f"Count: {int(m.group(1)) + 1}", l) for l in L], "Count line"),
        "verified with outside links only": (lambda L: bump(L[:ver] + [outside_only] + L[ver:], 1), "only outside links"),
    }
    fails = 0
    # The copy sits next to the real file so its relative links resolve the same way.
    p = os.path.join(os.path.dirname(DEFAULT), f".self-test-{os.getpid()}.md")
    for name, (plant, why) in plants.items():
        try:
            with open(p, "w", encoding="utf-8") as f:
                f.writelines(plant(list(lines)))
            bad, _, _ = check(p, load(p))
        finally:
            os.remove(p)
        if not any(why in b for b in bad):
            print(f"self-test: planted '{name}' not refused for '{why}': {bad}")
            fails += 1
    if fails == 0:
        print(f"check-launch self-test ok: {len(plants)} planted mistakes refused")
    return 1 if fails else 0


def main(argv):
    if argv[:1] == ["--self-test"]:
        return self_test()
    path = argv[0] if argv else DEFAULT
    bad, total, verified = check(path, load(path))
    if bad:
        print("\n".join(bad))
        return 1
    print(f"launch checklist ok: {total} gates ({verified} verified, {total - verified} open), every link resolves")
    return 0


sys.exit(main(sys.argv[1:]))
PY
