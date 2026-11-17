#!/usr/bin/env python3
"""No pipe into a reader that stops early where pipefail is on (THREATS OPS-PIPE-1).

Usage: python3 -I scripts/check-pipefail-grep.py [--self-test] [--list]

Under `set -o pipefail`, `producer | grep -q PATTERN` can fail although the pattern matched: grep
exits at the first match, the producer's next write dies of SIGPIPE (exit 141) and the pipeline
takes that status. It needs the producer to write more than a buffer after the match, so it shows
as a rare, size-dependent failure, or as a check that is skipped in some runs. The same holds for
`| head`, `| grep -m N`, `| sed ... q` and `| awk ... exit`, and for `x=$(producer | head -1)`
under `set -e`.

Flagged: in a shell file that sets pipefail (or is sourced by one), in a workflow step that runs
with pipefail, and in a fenced block of docs/ops that sets it, every pipe whose reader is one of
those. Write it so that it does not depend on timing:
    v=$(producer); grep -q PATTERN <<<"$v"        grep -q PATTERN file
    [ "$(producer | grep -c PATTERN)" != 0 ]      producer | awk '/PATTERN/{f=1} END{exit !f}'
    first=$(producer | sed -n '1p')               (sed without q reads to the end)
or, where the producer's output is always small (it fits one buffer, so no write follows the
reader's exit), say so on the same line or in the comment lines right above the statement:
    # pipefail-ok: <reason>
Comment lines are skipped. Exit 1 names each line; the self-test plants each form and its fix.
"""
import glob
import os
import re
import sys

ALLOW = re.compile(r"#\s*pipefail-ok:\s*\S")
PIPEFAIL = re.compile(r"\bset\s+(?:[-+]\w+\s+)*-\w*o\s+pipefail\b|\bbash\s+(?:-\w+\s+)*-\w*o\s+pipefail\b")
SOURCED = re.compile(r"^\s*(?:source|\.)\s+[\"']?([^\s\"';]+)")
# A single `|` (not `||`, not `|&` handled apart), then optional env assignments, then the reader.
PIPE = r"(?<![|>])\|&?(?!\|)\s*(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*(?:command\s+)?"
READERS = [
    ("grep -q", re.compile(PIPE + r"[ef]?grep\s+(?:(?:-[A-Za-z]+|--[a-z-]+)\s+)*(?:-[A-Za-z]*q[A-Za-z]*|--quiet|--silent)(?=\s|$)")),
    ("grep -m", re.compile(PIPE + r"[ef]?grep\s+(?:(?:-[A-Za-z]+|--[a-z-]+)\s+)*(?:-[A-Za-z]*m\s*\d+|--max-count)")),
    ("head", re.compile(PIPE + r"head(?=\s|$|\))(?!\s+(?:-n\s*|--lines=)-\d)")),
    ("sed q", re.compile(PIPE + r"sed\s+(?:-[A-Za-z]+\s+)*(?:(['\"])[^'\"]*(?<![A-Za-z])q[;}\s]*\1|\d*q(?=\s|$))")),
    ("awk exit", re.compile(PIPE + r"awk\s+(?:-[A-Za-z]\s*\S+\s+)*'(?:(?!END)[^'])*\bexit\b")),
]


def statements(lines: list[str]):
    """(first line number, last line number, text) of each statement, continuation lines joined."""
    out = []
    cur: list[str] = []
    start = 0
    open_end = False  # the last line kept ended in a backslash, a pipe or &&
    for n, raw in enumerate(lines, 1):
        s = raw.strip()
        if s.startswith("#"):
            continue
        joins_prev = s.startswith("|") or s.startswith("&&")
        if cur and not (joins_prev or open_end):
            out.append((start, n - 1, " ".join(cur)))
            cur = []
        if not s:
            open_end = False
            continue
        if not cur:
            start = n
        open_end = s.endswith(("\\", "|", "&&"))
        cur.append(s[:-1].rstrip() if s.endswith("\\") else s)
    if cur:
        out.append((start, len(lines), " ".join(cur)))
    return out


def allowed(lines: list[str], first: int, last: int) -> bool:
    if any(ALLOW.search(lines[i - 1]) for i in range(first, min(last, len(lines)) + 1)):
        return True
    i = first - 1  # the comment lines right above the statement
    while i >= 1 and lines[i - 1].strip().startswith("#"):
        if ALLOW.search(lines[i - 1]):
            return True
        i -= 1
    return False


def problems(text: str, offset: int = 0) -> list[tuple[int, str, str]]:
    """(line, reader, statement) of each pipe into an early-exiting reader with no allow comment."""
    lines = text.splitlines()
    out = []
    for first, last, stmt in statements(lines):
        hits = [name for name, rx in READERS if rx.search(stmt)]
        if hits and not allowed(lines, first, last):
            out.append((first + offset, ", ".join(hits), stmt))
    return out


def workflow_blocks(text: str):
    """(offset, block) of each `run: |` block of a workflow that runs with pipefail: GitHub adds
    `-o pipefail` when the step (or the file's defaults) names `shell: bash`; a block may set it."""
    lines = text.splitlines()
    # `defaults: run: shell: bash`, of the file or of one job: the whole file is read (the stricter side).
    file_bash = bool(re.search(r"^\s*defaults:\s*\n\s+run:\s*\n\s+shell:\s*bash\b", text, re.M))
    i = 0
    while i < len(lines):
        m = re.match(r"^(\s*(?:-\s+)?)run:\s*(.*)$", lines[i])
        if not m:
            i += 1
            continue
        indent = len(m.group(1))
        body = [(i + 1, m.group(2))] if m.group(2) and m.group(2) not in ("|", ">", "|-", ">-") else []
        j = i + 1
        while j < len(lines) and (not lines[j].strip() or len(lines[j]) - len(lines[j].lstrip()) > indent):
            body.append((j + 1, lines[j]))
            j += 1
        # The step's own keys sit around the block, at the indent of `run:` (after `- `: two deeper).
        k, step = i - 1, []
        while k >= 0 and not re.match(r"^\s*-\s", lines[k]):
            step.append(lines[k]); k -= 1
        if k >= 0:
            step.append(lines[k])
        while j < len(lines) and not re.match(r"^\s*-\s", lines[j]) and (not lines[j].strip() or len(lines[j]) - len(lines[j].lstrip()) >= indent):
            step.append(lines[j]); j += 1
        block = "\n".join(b for _, b in body)
        if body and (file_bash or any(re.match(r"^\s*(?:-\s+)?shell:\s*bash\b", s) for s in step) or PIPEFAIL.search(block)):
            yield body[0][0] - 1, block
        i = max(j, i + 1)


def doc_blocks(text: str):
    """(offset, block) of each fenced block of a document that sets pipefail."""
    lines = text.splitlines()
    start = None
    for n, line in enumerate(lines, 1):
        if line.lstrip().startswith("```"):
            if start is None:
                start = n
            else:
                block = "\n".join(lines[start:n - 1])
                if PIPEFAIL.search(block):
                    yield start, block
                start = None


def scan(path: str, text: str, sourced: set[str]) -> list[tuple[int, str, str]]:
    if path.endswith((".yml", ".yaml")):
        return [p for off, block in workflow_blocks(text) for p in problems(block, off)]
    if path.endswith(".md"):
        return [p for off, block in doc_blocks(text) for p in problems(block, off)]
    if PIPEFAIL.search(text) or os.path.normpath(path) in sourced:
        return problems(text)
    return []


def repo_files() -> list[str]:
    found = set()
    for pat in ("scripts/**/*.sh", "apps/**/*.sh", "deploy/**/*", "contracts/**/*.sh", "crates/**/*.sh",
                "adapters/**/*.sh", "recipes/**/*.sh", "*.sh", ".github/workflows/*.yml",
                ".github/workflows/*.yaml", "docs/ops/**/*.md"):
        for p in glob.glob(pat, recursive=True):
            parts = p.split(os.sep)
            if os.path.isfile(p) and not {"node_modules", "target", "out", "lib", "dist"} & set(parts):
                found.add(p)
    return sorted(found)


def read(path: str) -> str | None:
    try:
        with open(path, encoding="utf-8") as fh:
            return fh.read()
    except (UnicodeDecodeError, OSError):
        return None


def sourced_by_pipefail(texts: dict[str, str]) -> set[str]:
    """Shell files that set no pipefail themselves but are sourced by a file that does."""
    out = set()
    for path, text in texts.items():
        if not PIPEFAIL.search(text):
            continue
        for line in text.splitlines():
            m = SOURCED.match(line)
            if m:
                for base in ("", os.path.dirname(path)):
                    cand = os.path.normpath(os.path.join(base, m.group(1)))
                    if cand in texts:
                        out.add(cand)
    return out


def self_test() -> int:
    pf = "set -euo pipefail\n"
    bad = [
        ("grep -q in an if", "if producer | grep -q word; then echo yes; fi"),
        ("grep -qE, negated", "! cat file | grep -qE 'a|b' || exit 1"),
        ("grep -Fq after &&", "[ -n \"$x\" ] && echo \"$x\" | grep -Fq word"),
        ("grep --quiet", "producer | grep --quiet word || die no"),
        ("grep -m 1", "first=$(producer | grep -m 1 word)"),
        ("grep -qx with an option before", "cut -f3 file | grep -i -qx 2 && pass"),
        ("head -1 in an assignment", "x=$(producer | head -1)"),
        ("head -n1 then more stages", "port=$(grep -o 'x[0-9]*' log | head -n1 | sed 's/x//')"),
        ("head -c", "yes | head -c 100 >file"),
        ("sed with q", "v=$(producer | sed -n '1p;q')"),
        ("sed 1q", "producer | sed 1q"),
        ("awk that exits", "producer | awk '/word/{print; exit}'"),
        ("the reader on a continuation line", "producer --flag \\\n  | grep -q word || die no"),
        ("a pipe at the end of the line", "producer |\n  grep -q word"),
        ("an env assignment before the reader", "producer | LC_ALL=C grep -q word"),
        ("|& into grep -q", "producer |& grep -q word"),
        ("an allow comment two statements above does not count", "# pipefail-ok: small\necho one\nproducer | grep -q word"),
        ("the reader after a backslash, inside quotes", "check \"name\" \\\n  '[[ $(producer | head -1) ]]'"),
        ("an empty allow comment does not count", "producer | grep -q word # pipefail-ok:"),
    ]
    good = [
        ("a here-string", "grep -q word <<<\"$v\""),
        ("a file", "grep -q word file || die no"),
        ("grep -c reads to the end", "[ \"$(producer | grep -c word)\" != 0 ]"),
        ("awk that reads to the end", "producer | awk '/word/{f=1} END{exit !f}'"),
        ("sed without q", "first=$(producer | sed -n '1p')"),
        ("tail", "last=$(producer | tail -1)"),
        ("head -n -1 reads to the end", "producer | head -n -1 | while read -r p; do echo \"$p\"; done"),
        ("|| is not a pipe", "producer || grep -q word file"),
        ("head of a file", "head -1 file"),
        ("a comment line", "# producer | grep -q word"),
        ("allowed on the line", "producer | grep -q word  # pipefail-ok: one line of output"),
        ("allowed in the comment above", "# The address is one line.\n# pipefail-ok: one line of output\nproducer \\\n  | grep -q word"),
        ("allowed above a statement that goes on after a backslash", "# pipefail-ok: small\ncheck \"name\" \\\n  '[[ $(producer | head -1) ]]'"),
        ("grep -v is not grep -q", "producer | grep -v word | wc -l"),
    ]
    failed = []
    for name, text in bad:
        if len(problems(pf + text)) != 1:
            failed.append(f"not flagged: {name}")
    for name, text in good:
        if problems(pf + text):
            failed.append(f"flagged: {name}")
    # Where pipefail is in effect.
    plain = "producer | grep -q word\n"
    cases = [
        ("a shell file without pipefail is left alone", scan("a.sh", plain, set()) == []),
        ("a shell file with pipefail is read", len(scan("a.sh", pf + plain, set())) == 1),
        ("set -o pipefail alone counts", len(scan("a.sh", "set -o pipefail\n" + plain, set())) == 1),
        ("a file sourced by a pipefail file is read",
         len(scan("lib.sh", plain, sourced_by_pipefail({"a.sh": pf + ". lib.sh\n", "lib.sh": plain}))) == 1),
        ("a file sourced by its folder's neighbour is found",
         sourced_by_pipefail({"s/a.sh": pf + "source lib.sh\n", "s/lib.sh": plain}) == {"s/lib.sh"}),
        ("a file sourced by a file without pipefail is left alone",
         sourced_by_pipefail({"a.sh": ". lib.sh\n", "lib.sh": plain}) == set()),
        ("a workflow step with shell: bash is read",
         len(scan("w.yml", "jobs:\n  a:\n    steps:\n      - name: x\n        shell: bash\n        run: |\n          producer | grep -q word\n", set())) == 1),
        ("its line number is the file's",
         scan("w.yml", "jobs:\n  a:\n    steps:\n      - name: x\n        shell: bash\n        run: |\n          echo\n          producer | grep -q word\n", set())[0][0] == 8),
        ("shell: bash after the block counts",
         len(scan("w.yml", "steps:\n  - run: |\n      producer | head -1\n    shell: bash\n", set())) == 1),
        ("a workflow step with the default shell is left alone (no pipefail there)",
         scan("w.yml", "steps:\n  - name: x\n    run: |\n      producer | grep -q word\n  - name: y\n    shell: bash\n    run: echo\n", set()) == []),
        ("a workflow block that sets pipefail is read",
         len(scan("w.yml", "steps:\n  - run: |\n      set -o pipefail\n      producer | grep -q word\n", set())) == 1),
        ("the file's default shell counts",
         len(scan("w.yml", "defaults:\n  run:\n    shell: bash\njobs:\n  a:\n    steps:\n      - run: producer | grep -q word\n", set())) == 1),
        ("a document block that sets pipefail is read",
         len(scan("d.md", "text\n```sh\nset -euo pipefail\nproducer | grep -q word\n```\n", set())) == 1),
        ("its line number is the file's",
         scan("d.md", "text\n```sh\nset -euo pipefail\nproducer | grep -q word\n```\n", set())[0][0] == 4),
        ("a document block without pipefail is left alone",
         scan("d.md", "```sh\nproducer | grep -q word\n```\n", set()) == []),
        ("bash -eo pipefail -c in a unit counts",
         len(scan("deploy/x.service", "ExecStart=/bin/bash -eo pipefail -c 'producer | grep -q word'\n", set())) == 1),
    ]
    failed += [name for name, ok in cases if not ok]
    for f in failed:
        print(f"self-test: {f}")
    total = len(bad) + len(good) + len(cases)
    print(f"check-pipefail-grep self-test {'ok' if not failed else 'FAILED'}: {total} cases")
    return 1 if failed else 0


def main() -> int:
    if "--self-test" in sys.argv[1:]:
        return self_test()
    os.chdir(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
    texts = {p: t for p in repo_files() if (t := read(p)) is not None}
    sourced = sourced_by_pipefail({p: t for p, t in texts.items() if p.endswith((".sh", ".bash"))})
    found = [(p, n, reader, stmt) for p, t in texts.items() for n, reader, stmt in scan(p, t, sourced)]
    if "--list" in sys.argv[1:]:  # every pipe into such a reader, allowed ones too
        for p, t in texts.items():
            stripped = re.sub(r"pipefail-ok:", "pipefail-listed:", t)
            for n, reader, stmt in scan(p, stripped, sourced):
                mark = " " if (p, n, reader, stmt.replace("pipefail-listed:", "pipefail-ok:")) in found else "A"
                print(f"{mark} {p}:{n}: {reader}: {stmt[:200]}")
        return 0
    for p, n, reader, stmt in found:
        print(f"{p}:{n}: a pipe into {reader} where pipefail is on: {stmt[:160]}")
    if found:
        print(f"check-pipefail-grep: {len(found)} line(s). The reader stops early, the writer can die of SIGPIPE and "
              "the pipe then fails although it matched. Match from a variable or a file, or read to the end "
              "(grep -c, awk ... END); if the writer's output always fits one buffer, say why: # pipefail-ok: <reason>")
        return 1
    print(f"check-pipefail-grep ok: {len(texts)} files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
