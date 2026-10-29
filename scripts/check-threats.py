#!/usr/bin/env python3
"""THREATS.md rules (CLAUDE.md: every threat names its test or a TODO with a milestone; IDs are stable).

Checked for every table row whose first cell is a threat ID:
  1. The row is backed by one of:
     - a VERIFIED test: a backticked `scope::test_fn` whose function is a test (Rust `#[test]` /
       `#[tokio::test]` / proptest case, Solidity `test*`/`invariant_*`) defined inside the named
       crate, file, module or type (`onyx-api::...`, `indexer::tests::...`, `File.t.sol::test_x`),
       a bare Solidity `test_x` or a bare Rust test name that exists, or a browser test
       e2e "title" whose exact title is in apps/web/tests/*.mjs, or a check script cited by
       path (`scripts/check-x.sh`) that names the row's ID and that check-all or check-docs runs;
     - ACCEPTED followed by a reason (at least a few words);
     - "docs-only: <path>" where <path> exists in the repository;
     - an open status or a TODO naming an OPEN milestone (rule 2).
     Citations after "TODO" are planned tests and do not count. Rows that predate this rule and
     have none of these are listed in scripts/threats-legacy.txt: that list may only shrink (an
     ID there that now passes must be removed; an ID added since origin/main's copy fails).
  2. A row whose status is open (OPEN, TODO, PARTIAL, PARTLY, DEFERRED, BLOCKED, ESCALATED,
     NOT FIXED, Handed) names an OPEN milestone after its status word: an unticked ROADMAP.md item
     (B3, D2, L10, H1, S2, ...; Phase S items since ADR-0038 item 13), an unticked PLAN.md milestone (M8, U1), or "Needs the project owner".
     A TODO that names only finished milestones (M3, A1, L2, ...) is stale and fails; a dated
     note ("L9 2026-10-14: ...") names the sweep that wrote it, not a milestone.
  3. IDs are unique, except in status-update tables (header "ID | Status after ...").
  4. A cited test exists (R-CLAIM-1): every backticked `path::name` or `test_*` outside a TODO
     names a `fn`/`function`/`mod` in the code. Types and fields (`Type::field`), whole modules
     (`x::tests`, `x::*`) and old names given as "was `...`" or "old `...`" are not checked.
Run from the repo root (scripts/check-docs.sh calls it). Exit 1 with one line per problem.
  --self-test   plant bad rows (fake citations, bare ACCEPTED, missing docs path, stale TODO,
                duplicate ID) in a copy of the real file and require each to fail.
"""
import os
import re
import subprocess
import sys

THREATS = "THREATS.md"
ROADMAP = "ROADMAP.md"
PLAN = "PLAN.md"
LEGACY = "scripts/threats-legacy.txt"

ID_RE = re.compile(r"^[A-Z][A-Za-z0-9]*(?:-[A-Za-z0-9]+)*$")
# Phase P items carry a letter (P2c, P2g): named whole, not stripped (ROADMAP Phase P).
ITEM_RE = re.compile(r"(?<![\w-])((?:[A-HLSUW]|M)\d{1,2}|P\d{1,2}[a-z]?)(?![\w-])")
OPEN_WORDS = ("OPEN", "TODO", "PARTIAL", "PARTLY", "DEFERRED", "BLOCKED", "ESCALATED", "NOT FIXED", "HANDED")
SRC_DIRS = ("crates", "adapters", "contracts/test", "contracts/src", "apps/web/tests", "apps/web/verify", "apps/web/src", "fuzz", "recipes", "experiments")
SKIP_DIRS = {"target", "node_modules", "out", "build", ".svelte-kit", "pkg", "cache", "target-review"}


def cells(line):
    s = line.strip()
    if s.startswith("|"):
        s = s[1:]
    if s.endswith("|"):
        s = s[:-1]
    out, cur, tick = [], "", False
    for ch in s:
        if ch == "`":
            tick = not tick
        if ch == "|" and not tick:
            out.append(cur.strip())
            cur = ""
        else:
            cur += ch
    out.append(cur.strip())
    return out


def milestones():
    """Open and finished milestone names from ROADMAP.md and PLAN.md checkboxes."""
    open_, done = set(), set()
    with open(ROADMAP, encoding="utf-8") as f:
        for line in f:
            m = re.match(r"^- \[( |x)\] ([A-HLPSUW]\d{1,2}[a-z]?)\b", line)
            if m:
                name = m.group(2)
                (done if m.group(1) == "x" else open_).add(name if name[0] == "P" else name.rstrip("ab"))
    with open(PLAN, encoding="utf-8") as f:
        for line in f:
            m = re.match(r"^- \[( |x)\] (?:\*\*)?((?:M|U)\d{1,2})\b", line)
            if m:
                (done if m.group(1) == "x" else open_).add(m.group(2))
    return open_, done - open_


def norm(s):
    s = s.lower().replace("-", "_")
    return s[5:] if s.startswith("onyx_") else s


class Code:
    """Index of the source tree: per file its functions, test functions, modules and types."""

    def __init__(self):
        self.files = {}  # path -> dict(fns, tests, mods, types)
        self.e2e = ""
        for root in SRC_DIRS:
            for dp, dn, fn in os.walk(root):
                dn[:] = [d for d in dn if d not in SKIP_DIRS]
                for f in fn:
                    p = os.path.join(dp, f)
                    if f.endswith((".rs", ".sol", ".mjs", ".js", ".ts")):
                        with open(p, encoding="utf-8", errors="ignore") as h:
                            text = h.read()
                        self.files[p] = self.scan(p, text)
                        if p.startswith("apps/web/tests/") and f.endswith(".mjs"):
                            self.e2e += text
        self.names = set()
        self.all_tests = set()
        for v in self.files.values():
            self.names |= v["fns"] | v["mods"]
            self.all_tests |= v["tests"]

    @staticmethod
    def scan(path, text):
        fns = set(re.findall(r"\b(?:fn|function)\s+([A-Za-z_]\w*)", text))
        mods = set(re.findall(r"\bmod\s+([A-Za-z_]\w*)", text))
        types = set(re.findall(r"\b(?:struct|enum|trait|contract|library|interface)\s+([A-Za-z_]\w*)", text))
        types |= set(re.findall(r"\bimpl(?:<[^>]*>)?\s+(?:[\w:]+\s+for\s+)?([A-Za-z_]\w*)", text))
        tests = set()
        if path.endswith(".sol"):
            tests = {f for f in fns if re.match(r"(test|testFuzz|invariant)_?\w*", f) and f != "test"}
        elif path.endswith(".rs"):
            lines = text.split("\n")
            for i, line in enumerate(lines):
                m = re.match(r"\s*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z_]\w*)\s*\(([^)]*)", line)
                if not m:
                    continue
                attrs = "\n".join(lines[max(0, i - 6) : i])
                if re.search(r"#\[(tokio::)?test\b|#\[test_case|#\[rstest", attrs) or " in " in m.group(2):
                    tests.add(m.group(1))
        return {"fns": fns, "tests": tests, "mods": mods, "types": types}

    def scope(self, seg):
        """Files a path segment can name: a crate or adapter directory, a file (stem or name), a
        module declared in a file, or a type defined in it."""
        s = norm(seg)
        out = set()
        for p, v in self.files.items():
            parts = p.split("/")
            base = parts[-1]
            stem = norm(base.split(".")[0])
            if seg == base or (seg.endswith(".sol") and base == seg):
                out.add(p)
            elif len(parts) > 2 and parts[0] in ("crates", "adapters", "recipes", "fuzz") and norm(parts[1]) in (s, "source_" + s):
                out.add(p)
            elif stem in (s, s + "_tests") or (base == "mod.rs" and norm(parts[-2]) == s):
                out.add(p)
            elif seg in v["mods"] or seg in v["types"]:
                out.add(p)
        return out

    def guard(self, token, rid):
        """A check script cited by path (`scripts/x.sh`, `check-x.sh`) backs a row when it exists,
        names the row's ID (so the guard says which threat it holds), and check-all or check-docs
        runs it."""
        if not re.fullmatch(r"(scripts/)?[\w.-]+\.(sh|py)", token):
            return False
        path = token if token.startswith("scripts/") else f"scripts/{token}"
        if not os.path.exists(path) or not rid:
            return False
        with open(path, encoding="utf-8", errors="ignore") as h:
            if not re.search(rf"(?<![\w-]){re.escape(rid)}(?![\w-])", h.read()):
                return False
        runners = ""
        for r in ("scripts/check-all.sh", "scripts/check-docs.sh"):
            with open(r, encoding="utf-8", errors="ignore") as h:
                runners += h.read()
        return path == "scripts/check-all.sh" or os.path.basename(path) in runners

    def verified(self, token, rid=""):
        """A citation that names a test function inside the scope its path names, or a check
        script that guards this row (`guard`)."""
        if "/" in token or token.endswith((".sh", ".py")):
            return self.guard(token, rid)
        parts = token.split("::")
        last = parts[-1]
        if len(parts) == 1:
            if re.match(r"(test|testFuzz|invariant)_", last):
                return last in self.all_tests
            return last.count("_") >= 2 and last in self.all_tests
        segs = [p for p in parts[:-1] if p not in ("tests", "test")]
        if not segs:
            pool = {p for p, v in self.files.items() if "tests" in v["mods"] or "/tests/" in p or p.endswith("tests.rs")}
        else:
            scopes = [self.scope(s) for s in segs]
            if any(not sc for sc in scopes):
                return False
            pool = set.intersection(*scopes) or scopes[-1]
        return any(last in self.files[p]["tests"] for p in pool)


def citations(cell):
    """Backticked tokens before any TODO in a cell, minus old names ("was `x`", "old `x`")."""
    cell = cell.split("TODO", 1)[0]
    for m in re.finditer(r"`([^`]+)`", cell):
        t = m.group(1).strip().rstrip("()")
        before = cell[max(0, m.start() - 12) : m.start()]
        if re.search(r"\b(was (the )?|old |reviewer's )$", before):
            continue
        # `path::{a, b}` cites path::a and path::b.
        g = re.fullmatch(r"([\w:.\-]+)::\{([^}]+)\}", t)
        if g:
            for name in g.group(2).split(","):
                yield f"{g.group(1)}::{name.strip()}"
            continue
        # `Contract.test_x` (Foundry's dotted form) is `Contract::test_x`.
        t = re.sub(r"^(\w+)\.((?:test|testFuzz|invariant)_?\w*)$", r"\1::\2", t)
        t = re.sub(r"\s+--self-test$", "", t)
        if re.fullmatch(r"[\w:./\-]+", t):
            yield t


def cited_tests(cell):
    """Citations that look like test paths, for the existence rule (4)."""
    for t in citations(cell):
        if not ("::" in t or re.match(r"(test|testFuzz|invariant)_", t)):
            continue
        parts = t.split("::")
        last = parts[-1]
        # `Type::field` and `Type::method` (a bare type name first) are not tests.
        if last in ("*", "tests", "") or (len(parts) > 1 and re.fullmatch(r"[A-Z]\w*", parts[0])):
            continue
        if not re.fullmatch(r"[a-z_]\w*|(test|testFuzz|invariant)_\w*", last):
            continue
        yield t, last


def row_tested(code, c):
    for cell in c[1:]:
        if any(code.verified(t, c[0]) for t in citations(cell)):
            return True
        before_todo = cell.split("TODO", 1)[0]
        for title in re.findall(r'\be2e "([^"]+)"', before_todo):
            if f"'{title}'" in code.e2e or f'"{title}"' in code.e2e or f"`{title}`" in code.e2e:
                return True
    return False


def accepted_with_reason(status, mitigation=""):
    """ACCEPTED with a reason: at least four words after the status word, or in the row's
    mitigation column (older tables put the reason there)."""
    m = re.match(r"ACCEPTED\b(.*)", status, re.S | re.I)
    words = lambda t: len(re.findall(r"[A-Za-z]{2,}", t))
    return bool(m) and (words(m.group(1)) >= 4 or words(mitigation) >= 4)


def docs_only_ok(status):
    paths = re.findall(r"docs-only:\s*`?([\w./+-]+?)`?(?=[\s,;:)]|$)", status)
    paths = [p.rstrip(".") for p in paths]
    return bool(paths) and all(os.path.exists(p) for p in paths)


def legacy_ids():
    if not os.path.exists(LEGACY):
        return [], None
    ids = [l.split("#")[0].strip() for l in open(LEGACY, encoding="utf-8")]
    ids = [i for i in ids if i]
    # None: origin/main has no list yet (first introduction); a set: the list there.
    base = None
    try:
        out = subprocess.run(["git", "show", f"origin/main:{LEGACY}"], capture_output=True, text=True, check=False)
        if out.returncode == 0:
            base = {l.split("#")[0].strip() for l in out.stdout.split("\n")} - {""}
    except OSError:
        pass
    return ids, base


def check(lines, code, open_ms, done_ms, legacy, legacy_base):
    problems = []
    seen = {}
    header = None
    unbacked = set()
    for n, line in enumerate(lines, 1):
        if not line.startswith("|"):
            header = None
            continue
        c = cells(line)
        if header is None:
            header = [h.lower() for h in c]
            continue
        if all(set(x) <= set("-: ") for x in c):
            continue
        rid = c[0]
        if not ID_RE.match(rid):
            continue
        update_table = len(header) > 1 and header[1].startswith("status after")
        if not update_table:
            if rid in seen:
                problems.append(f"THREATS.md:{n}: duplicate ID {rid} (first at line {seen[rid]})")
            else:
                seen[rid] = n
        status = c[-1]
        for cell in c[1:]:
            for t, last in cited_tests(cell):
                if last not in code.names:
                    problems.append(f"THREATS.md:{n}: {rid} cites `{t}`, but no fn/function/mod {last} exists")
        up = status.upper()
        is_open = any(up.startswith(w) for w in OPEN_WORDS)
        undated = re.sub(r"(?<![\w-])(?:[A-HLSUW]\d{1,2}|P\d{1,2}[a-z]?) \d{4}-\d{2}-\d{2}", "", status)
        named = {m.group(1) for m in ITEM_RE.finditer(undated)}
        has_open_ms = bool(named & open_ms) or "Needs the project owner" in status
        todo = "TODO" in status
        if is_open and not has_open_ms:
            stale = sorted(named & done_ms)
            why = f"names only finished milestones {', '.join(stale)}" if stale else "names no open milestone"
            problems.append(f"THREATS.md:{n}: {rid} is open but {why} (ROADMAP item, M8 or Needs the project owner)")
            continue
        if todo and not has_open_ms and not up.startswith("ACCEPTED") and not row_tested(code, c):
            problems.append(f"THREATS.md:{n}: {rid} has a TODO without an open milestone")
            continue
        mitigation = c[-2] if len(c) > 2 else ""
        if up.startswith("ACCEPTED") and not accepted_with_reason(status, mitigation):
            problems.append(f"THREATS.md:{n}: {rid} is ACCEPTED without a reason")
            continue
        if "docs-only:" in status and not docs_only_ok(status):
            problems.append(f"THREATS.md:{n}: {rid} names a docs-only path that does not exist")
            continue
        backed = (
            row_tested(code, c)
            or (up.startswith("ACCEPTED") and accepted_with_reason(status, mitigation))
            or ("docs-only:" in status and docs_only_ok(status))
            or ((todo or is_open) and has_open_ms)
        )
        if not backed and not update_table:
            unbacked.add(rid)
            if rid not in legacy:
                problems.append(f"THREATS.md:{n}: {rid} names no verified test, TODO with an open milestone, ACCEPTED reason or existing docs-only path")
    for rid in sorted(set(legacy) - unbacked):
        if rid in seen:
            problems.append(f"{LEGACY}: {rid} now has a verified test or milestone: remove it from the legacy list")
        else:
            problems.append(f"{LEGACY}: {rid} is not a THREATS row")
    if legacy_base is not None:
        for rid in sorted(set(legacy) - legacy_base):
            problems.append(f"{LEGACY}: {rid} was added; the legacy list may only shrink (cite a real test instead)")
    if len(legacy) != len(set(legacy)):
        problems.append(f"{LEGACY}: an ID is listed twice")
    return problems, len(seen)


def self_test(code, open_ms, done_ms, legacy, legacy_base):
    lines = open(THREATS, encoding="utf-8").read().split("\n")
    base, _ = check(lines, code, open_ms, done_ms, legacy, legacy_base)
    if base:
        print("self-test: the real file must pass first")
        return 1
    planted = {
        "fake crate": "| ZZ-1 | Any | Low | x | y | FIXED: `totally_fake_crate::main` |",
        "fake type method": "| ZZ-1 | Any | Low | x | y | FIXED: `Ledger::anything_at_all` |",
        "constant": "| ZZ-1 | Any | Low | x | y | FIXED: `ONYX_PUBLIC` |",
        "bare unknown name": "| ZZ-1 | Any | Low | x | y | FIXED: `nonexistent_test_name` |",
        "non-test fn": "| ZZ-1 | Any | Low | x | y | FIXED: `onyx-merkle::root` |",
        "fake e2e title": '| ZZ-1 | Any | Low | x | y | FIXED: e2e "a title no browser test has" |',
        "bare ACCEPTED": "| ZZ-1 | Any | Low | x | y | ACCEPTED |",
        "docs-only nowhere": "| ZZ-1 | Any | Low | x | y | FIXED docs-only: nowhere/missing.md |",
        "stale TODO": "| ZZ-1 | Any | Low | x | y | OPEN: TODO M4 |",
        "no test": "| ZZ-1 | Any | Low | x | y | FIXED loop 99 |",
        "duplicate ID": "| T-MERK-1 | Any | Low | x | y | FIXED: `onyx-merkle::root_binds_count_and_order` |",
        "planned test only": "| ZZ-1 | Any | Low | x | y | FIXED. TODO L2: `onyx-merkle::root_binds_count_and_order` |",
        "script not naming the row": "| ZZ-1 | Any | Low | x | y | FIXED: `scripts/check-secrets.sh` |",
    }
    good = {
        "real test": "| ZZ-1 | Any | Low | x | y | FIXED: `onyx-merkle::root_binds_count_and_order` |",
        "real e2e": '| ZZ-1 | Any | Low | x | y | FIXED: e2e "explorer: live headers pass" |',
        "accepted with reason": "| ZZ-1 | Any | Low | x | y | ACCEPTED: testnet only, no money moves; reopen before mainnet |",
        "docs-only real path": "| ZZ-1 | Any | Low | x | y | FIXED docs-only: docs/ops/secrets.md |",
    }
    fails = 0
    table = ["", "| ID | Attacker | Severity | Threat | Mitigation | Status |", "|---|---|---|---|---|---|"]
    for name, row in planted.items():
        p, _ = check(lines + table + [row], code, open_ms, done_ms, legacy, legacy_base)
        if not p:
            print(f"self-test: planted row passed: {name}")
            fails += 1
    for name, row in good.items():
        p, _ = check(lines + table + [row], code, open_ms, done_ms, legacy, legacy_base)
        if p:
            print(f"self-test: good row failed: {name}: {p[0]}")
            fails += 1
    # Product milestones follow the same open/closed rule as protocol milestones.
    for item, wanted in [("U90", False), ("U91", True)]:
        row = f"| ZZ-1 | Any | Low | x | y | TODO {item}: product follow-up |"
        problems, _ = check(lines + table + [row], code, open_ms | {"U90"}, done_ms | {"U91"}, legacy, legacy_base)
        if bool(problems) != wanted:
            print(f"self-test: product milestone {item} open/closed rule failed")
            fails += 1
    # A guard script backs the row it names: T-KEY-1 backed only by its scanner passes.
    swapped = [
        "| T-KEY-1 | Any | Low | x | y | FIXED: `scripts/check-secrets.sh --self-test` |" if l.startswith("| T-KEY-1 |") else l
        for l in lines
    ]
    p, _ = check(swapped, code, open_ms, done_ms, legacy, legacy_base)
    if p:
        print(f"self-test: a row backed by the guard script that names it failed: {p[0]}")
        fails += 1
    # An ID added to the legacy list fails, and so does a listed row that has a test.
    p, _ = check(lines, code, open_ms, done_ms, legacy + ["T-MERK-1"], set(legacy))
    if not any("T-MERK-1 was added" in x for x in p) or not any("remove it from the legacy list" in x for x in p):
        print("self-test: an ID added to the legacy list, or a tested row kept on it, passed")
        fails += 1
    if fails:
        return 1
    print(f"threats self-test ok: {len(planted)} planted rows fail, {len(good) + 1} good rows pass, legacy list only shrinks")
    return 0


def main():
    open_ms, done_ms = milestones()
    code = Code()
    legacy, legacy_base = legacy_ids()
    if "--self-test" in sys.argv[1:]:
        return self_test(code, open_ms, done_ms, legacy, legacy_base)
    with open(THREATS, encoding="utf-8") as f:
        lines = f.read().split("\n")
    problems, rows = check(lines, code, open_ms, done_ms, legacy, legacy_base)
    for p in problems:
        print(p)
    if problems:
        print(f"threats: {len(problems)} problem(s)")
        return 1
    print(f"threats ok: {rows} rows, open milestones {len(open_ms)}, legacy rows without a verified test {len(legacy)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
