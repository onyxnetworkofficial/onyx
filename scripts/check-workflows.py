#!/usr/bin/env python3
"""GitHub Actions rules (CL10, THREATS CL10-9): every workflow pins each action to a full commit
SHA, sets its token's permissions at the top, uses no secret but the repository's own
GITHUB_TOKEN and never runs on pull_request_target; in desktop.yml only the release job may write
(contents: write) and it runs only for a pushed v*-testnet tag.

  python3 -I scripts/check-workflows.py [--self-test]
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
USES = re.compile(r"^\s*-?\s*uses:\s*(\S+)")
PINNED = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}$")
SECRET = re.compile(r"secrets\.([A-Za-z0-9_]+)")


def jobs(text):
    """{job name: its text} for the top-level `jobs:` map (two-space indented keys)."""
    out, name, buf, inside = {}, None, [], False
    for line in text.splitlines():
        if line.startswith("jobs:"):
            inside = True
            continue
        if inside and re.match(r"^\S", line):
            inside = False
        if not inside:
            continue
        m = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if m:
            if name:
                out[name] = "\n".join(buf)
            name, buf = m.group(1), []
        elif name:
            buf.append(line)
    if name:
        out[name] = "\n".join(buf)
    return out


def problems(name, text):
    p = []
    for i, line in enumerate(text.splitlines(), 1):
        m = USES.match(line)
        if m and not m.group(1).startswith("./") and not PINNED.match(m.group(1)):
            p.append(f"{name}:{i}: action not pinned to a full commit SHA: {m.group(1)}")
        for s in SECRET.findall(line):
            if s != "GITHUB_TOKEN":
                p.append(f"{name}:{i}: secret {s} (only the repository's own GITHUB_TOKEN)")
    if "pull_request_target" in text:
        p.append(f"{name}: pull_request_target runs with a write token on outside code")
    if not re.search(r"^permissions:", text, re.M):
        p.append(f"{name}: no top-level permissions (the token's default may write)")
    if name == "desktop.yml":
        if not re.search(r"^permissions:\s*\{\}\s*$", text, re.M):
            p.append("desktop.yml: top-level permissions must be {}")
        js = jobs(text)
        for j, body in js.items():
            writes = re.findall(r"^\s+([a-z-]+):\s*write\s*$", body, re.M)
            if j == "release":
                if writes != ["contents"]:
                    p.append(f"desktop.yml: the release job must ask for contents: write only (asks {writes})")
                if "refs/tags/v" not in body or "-testnet" not in body or "github.event_name == 'push'" not in body:
                    p.append("desktop.yml: the release job must run only for a pushed v*-testnet tag")
            elif writes:
                p.append(f"desktop.yml: job {j} asks for write ({writes}); only release may write")
        if "release" not in js:
            p.append("desktop.yml: no release job")
    return p


def main():
    found = []
    files = sorted((ROOT / ".github" / "workflows").glob("*.yml"))
    for f in files:
        found += problems(f.name, f.read_text())
    for x in found:
        print(x)
    if not found:
        print(f"workflows ok: {len(files)} files, every action pinned by SHA")
    return 1 if found else 0


def self_test():
    good = (ROOT / ".github" / "workflows" / "desktop.yml").read_text()
    assert problems("desktop.yml", good) == [], problems("desktop.yml", good)
    bad = {
        "tag": good.replace("actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1", "actions/checkout@v7"),
        "secret": good.replace("${{ secrets.GITHUB_TOKEN }}", "${{ secrets.SIGNING_KEY }}"),
        "build writes": good.replace("    permissions:\n      contents: read", "    permissions:\n      contents: write", 1),
        "release packages": good.replace("    permissions:\n      contents: write", "    permissions:\n      contents: write\n      packages: write"),
        "top": good.replace("permissions: {}", "permissions: write-all"),
        "prt": good.replace("  workflow_dispatch:", "  pull_request_target:"),
        "any ref": good.replace("github.event_name == 'push' && ", ""),
    }
    for what, text in bad.items():
        assert text != good, what
        assert problems("desktop.yml", text), f"self-test: {what} was not caught"
    print(f"check-workflows self-test: {len(bad)} planted faults caught")
    return 0


if __name__ == "__main__":
    sys.exit(self_test() if sys.argv[1:] == ["--self-test"] else main())
