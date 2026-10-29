#!/usr/bin/env python3
"""Release step: every served source's terms were read recently (LC-27, THREATS L2-SRC-4).

Reads section 5 of docs/ops/data-sources.md ("Release step: served sources' terms"), prints
every served source with its terms link, last-read date and age, and exits 1 when one is
unread or older than the threshold that section states ("Re-read threshold: N days").

Rules (each one fails the run):
  - every source id of section 1 ("Current sources") has at least one row in section 5, and
    section 5 names no id that section 1 lacks;
  - `Served` is `yes` or `no`;
  - a served row's terms are a URL with a `YYYY-MM-DD` last-read date no older than the
    threshold (and not in the future), or `none: consensus data` with `n/a`, or `via S5 S6`
    (covered by those sources' rows, which must be served, read directly (never another
    `via` row, so no chain or cycle) and pass), or `not recorded` /
    `unread`, which always fails until someone reads the terms and records them.

Usage:
  python3 -I scripts/check-source-terms.py               the release step (dates checked)
  python3 -I scripts/check-source-terms.py --lint        structure only, no dates (check-docs)
  python3 -I scripts/check-source-terms.py --today 2027-01-31   as of another day
  python3 -I scripts/check-source-terms.py --self-test   planted mistakes must each fail
Exit codes: 0 pass, 1 a source fails, 2 the file cannot be read as described above.
"""
import datetime as dt
import re
import sys

DOC = "docs/ops/data-sources.md"
SECTION1 = "## 1. Current sources"
SECTION5 = "## 5. Release step"
THRESHOLD = re.compile(r"Re-read threshold: (\d+) days\b")
SOURCE_ID = re.compile(r"^S\d+$")
URL = re.compile(r"^https://\S+$")
DATE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
VIA = re.compile(r"^via( S\d+)+$")
NONE = "none: consensus data"


class Malformed(Exception):
    pass


def section(lines, head):
    out, inside = [], False
    for line in lines:
        if line.startswith("## "):
            inside = line.startswith(head)
            continue
        if inside:
            out.append(line)
    if not out:
        raise Malformed(f"no section starting {head!r}")
    return out


def table_rows(lines):
    rows = []
    for line in lines:
        if not line.startswith("|"):
            continue
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if all(set(c) <= set("-: ") for c in cells):
            continue  # the |---| line
        rows.append(cells)
    if len(rows) < 2:
        raise Malformed("table has no rows")
    return rows[0], rows[1:]


def strip_code(cell):
    return cell.strip().strip("`").strip()


def parse(text):
    lines = text.splitlines()
    head1, rows1 = table_rows(section(lines, SECTION1))
    ids1 = [r[0] for r in rows1]
    for i in ids1:
        if not SOURCE_ID.match(i):
            raise Malformed(f"section 1: {i!r} is not a source id")
    s5 = section(lines, SECTION5)
    m = THRESHOLD.search("\n".join(s5))
    if not m:
        raise Malformed("section 5 states no 'Re-read threshold: N days'")
    threshold = int(m.group(1))
    head5, rows5 = table_rows(s5)
    want = ["#", "Served", "Terms read at", "Last read", "Covers"]
    if head5[: len(want)] != want:
        raise Malformed(f"section 5 table header must start {want}, got {head5}")
    rows = []
    for r in rows5:
        if len(r) < len(want):
            raise Malformed(f"section 5 row {r}: {len(r)} cells, need {len(want)}")
        rows.append(
            {
                "id": r[0],
                "served": r[1],
                "terms": strip_code(r[2]),
                "last": strip_code(r[3]),
                "covers": r[4],
            }
        )
    return ids1, threshold, rows


def judge(ids1, threshold, rows, today, dates=True):
    """Returns (report lines, problems)."""
    problems, report = [], []
    ids5 = {r["id"] for r in rows}
    for i in ids1:
        if i not in ids5:
            problems.append(f"{i}: in section 1 but has no row in section 5")
    for i in sorted(ids5 - set(ids1), key=lambda s: (len(s), s)):
        problems.append(f"{i}: in section 5 but not in section 1")
    served = {}
    for r in rows:
        if r["served"] not in ("yes", "no"):
            problems.append(f"{r['id']}: Served must be yes or no, got {r['served']!r}")
            continue
        if r["served"] == "yes":
            served.setdefault(r["id"], []).append(r)
    ok_ids = set()
    pending_via = []
    for i, rs in served.items():
        good = True
        for r in rs:
            t, last = r["terms"], r["last"]
            if t == NONE:
                if last != "n/a":
                    problems.append(f"{i}: '{NONE}' takes Last read n/a, got {last!r}")
                    good = False
                report.append((i, t, "n/a", "", "ok"))
            elif VIA.match(t):
                pending_via.append((i, t.split()[1:]))
                if last != "n/a":
                    problems.append(f"{i}: '{t}' takes Last read n/a, got {last!r}")
                    good = False
            elif t in ("not recorded", "unread") or last == "unread":
                report.append((i, t, last, "", "UNREAD"))
                if dates:
                    problems.append(f"{i}: terms unread ({r['covers']}); read them, record URL and date")
                    good = False
            elif URL.match(t):
                if not DATE.match(last):
                    problems.append(f"{i}: Last read must be YYYY-MM-DD, got {last!r}")
                    good = False
                    continue
                try:
                    d = dt.date.fromisoformat(last)
                except ValueError:
                    problems.append(f"{i}: Last read {last!r} is not a date")
                    good = False
                    continue
                age = (today - d).days
                status = "ok"
                if dates and age < 0:
                    status = "FUTURE"
                    problems.append(f"{i}: Last read {last} is after today ({today})")
                    good = False
                elif dates and age > threshold:
                    status = "STALE"
                    problems.append(
                        f"{i}: {t} last read {last}, {age} days ago (threshold {threshold})"
                    )
                    good = False
                report.append((i, t, last, str(age) if dates else "", status if dates else "-"))
            else:
                problems.append(
                    f"{i}: terms must be an https URL, '{NONE}', 'via S..' or 'not recorded'; got {t!r}"
                )
                good = False
        if good:
            ok_ids.add(i)
    # A `via` row is covered only by rows read directly (a URL or consensus data):
    # never by itself or another `via` row, so a cycle or chain cannot pass unread.
    via_ids = {i for i, _ in pending_via}
    ok_ids -= via_ids
    for i, refs in pending_via:
        bad = [s for s in refs if s not in served]
        if bad:
            problems.append(f"{i}: via {' '.join(bad)}, which are not served rows")
            report.append((i, "via " + " ".join(refs), "n/a", "", "FAIL"))
            continue
        chained = [s for s in refs if s in via_ids]
        if chained:
            problems.append(f"{i}: via {' '.join(chained)}, which are 'via' rows themselves")
            report.append((i, "via " + " ".join(refs), "n/a", "", "FAIL"))
            continue
        failing = [s for s in refs if s not in ok_ids]
        if failing and dates:
            problems.append(f"{i}: covered by {' '.join(failing)}, which fail")
        report.append((i, "via " + " ".join(refs), "n/a", "", "FAIL" if failing else "ok"))
    return report, problems


def run(text, today, dates=True, out=sys.stdout):
    try:
        ids1, threshold, rows = parse(text)
    except Malformed as e:
        print(f"check-source-terms: {DOC}: {e}", file=sys.stderr)
        return 2
    report, problems = judge(ids1, threshold, rows, today, dates)
    if dates:
        print(f"Served sources' terms as of {today} (threshold {threshold} days):", file=out)
        for i, t, last, age, status in report:
            print(f"  {status:7} {i:4} {last:10} {age:>4}  {t}", file=out)
    for p in problems:
        print(f"check-source-terms: FAIL {p}", file=sys.stderr)
    if problems:
        return 1
    n = len({r[0] for r in report})
    print(f"source terms ok: {n} served sources" + ("" if dates else " (structure only)"), file=out)
    return 0


GOOD = """# x
## 1. Current sources
| # | Source |
|---|---|
| S1 | chain |
| S2 | okx |
| S3 | labels |
| S4 | blocked |
## 2. Other
## 5. Release step: served sources' terms
Re-read threshold: 90 days.
| # | Served | Terms read at | Last read | Covers |
|---|---|---|---|---|
| S1 | yes | none: consensus data | n/a | blocks |
| S2 | yes | https://example.org/terms | 2026-10-14 | rows |
| S3 | yes | via S2 | n/a | labels |
| S4 | no | https://example.org/no | 2020-01-01 | nothing |
## Scope
"""


def self_test():
    import io

    today = dt.date(2026, 10, 16)
    sink = io.StringIO()
    if run(GOOD, today, out=sink) != 0:
        print("self-test: the good file failed", file=sys.stderr)
        return 1
    cases = {
        "stale date": (GOOD.replace("2026-10-14", "2026-07-17"), 1),
        "future date": (GOOD.replace("2026-10-14", "2026-10-17"), 1),
        "unread terms": (GOOD.replace("| 2026-10-14 |", "| unread |"), 1),
        "not recorded terms": (GOOD.replace("https://example.org/terms", "not recorded"), 1),
        "missing section-1 id": (GOOD.replace("| S4 | no |", "| S9 | no |"), 1),
        "via a failing row": (GOOD.replace("2026-10-14", "2025-01-01"), 1),
        "via an unserved row": (GOOD.replace("via S2", "via S4"), 1),
        "via itself": (GOOD.replace("via S2", "via S3"), 1),
        "via a via row": (
            GOOD.replace("| S4 | no |", "| S4 | yes | via S3 | n/a | chain |\n| S4 | no |"),
            1,
        ),
        "bad served value": (GOOD.replace("| S1 | yes |", "| S1 | maybe |"), 1),
        "http not https": (GOOD.replace("https://example.org/terms", "http://example.org/terms"), 1),
        "bad date": (GOOD.replace("2026-10-14", "2026-13-40"), 1),
        "no threshold": (GOOD.replace("Re-read threshold: 90 days.", ""), 2),
        "no section 5": (GOOD.replace("## 5. Release step", "## 6. Other"), 2),
    }
    # The edge: exactly 90 days old passes, 91 fails.
    edge_ok = GOOD.replace("2026-10-14", "2026-07-18")  # 90 days before 2026-10-16
    if run(edge_ok, today, out=sink) != 0:
        print("self-test: a 90-day-old read failed (must pass at the threshold)", file=sys.stderr)
        return 1
    failed = 0
    for name, (text, want) in cases.items():
        err = sys.stderr
        sys.stderr = io.StringIO()
        try:
            got = run(text, today, out=sink)
        finally:
            sys.stderr = err
        if got != want:
            print(f"self-test: {name}: exit {got}, want {want}", file=sys.stderr)
            failed = 1
    # Structure-only mode ignores dates but not structure.
    err = sys.stderr
    sys.stderr = io.StringIO()
    try:
        lint_stale = run(cases["stale date"][0], today, dates=False, out=sink)
        lint_unread = run(cases["unread terms"][0], today, dates=False, out=sink)
        lint_missing = run(cases["missing section-1 id"][0], today, dates=False, out=sink)
    finally:
        sys.stderr = err
    if (lint_stale, lint_unread, lint_missing) != (0, 0, 1):
        got = f"{lint_stale}/{lint_unread}/{lint_missing}"
        print(f"self-test: --lint gave {got}, want 0/0/1", file=sys.stderr)
        failed = 1
    if not failed:
        print(f"check-source-terms self-test ok: {len(cases) + 5} cases")
    return failed


def main(argv):
    today = dt.datetime.now(dt.timezone.utc).date()
    dates = True
    args = list(argv)
    while args:
        a = args.pop(0)
        if a == "--self-test":
            return self_test()
        if a == "--lint":
            dates = False
        elif a == "--today" and args:
            try:
                today = dt.date.fromisoformat(args.pop(0))
            except ValueError:
                print("--today takes YYYY-MM-DD", file=sys.stderr)
                return 2
        else:
            print(__doc__, file=sys.stderr)
            return 2
    try:
        text = open(DOC, encoding="utf-8").read()
    except OSError as e:
        print(f"check-source-terms: {e}", file=sys.stderr)
        return 2
    return run(text, today, dates)


if __name__ == "__main__":
    import os

    os.chdir(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
    sys.exit(main(sys.argv[1:]))
