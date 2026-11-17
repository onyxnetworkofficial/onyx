#!/usr/bin/env python3
"""Owner rules for what the deploy scripts install as root (THREATS OPS-DEPLOY-1; scripts/*.sh, deploy/).

Usage: python3 -I scripts/check-deploy-owner.py [--self-test]

Two mistakes left a store the node's service user could not read, with no error at deploy time:
1. `chown --reference=X ... X`: a folder fixed from itself changes nothing.
2. `sudo rsync -a ... /var/lib/...`: rsync as root with -a (or -o, -g) keeps the sending account
   as the owner of every file it writes and of the target folder. Such a line must drop them
   (`--no-owner --no-group`) or name the owner (`--chown`).
Comment lines are skipped. Exit 1 names each line; the self-test plants both and their fixes.
"""
import glob
import os
import re
import shlex
import sys


def problems(text: str) -> list[str]:
    out = []
    for n, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        try:
            words = shlex.split(line.replace("\\", ""), comments=False)
        except ValueError:
            words = line.replace("\\", "").split()
        for i, w in enumerate(words):
            if w == "chown":
                args = words[i + 1 :]
                refs = [a.split("=", 1)[1].rstrip("/") for a in args if a.startswith("--reference=")]
                targets = [a.rstrip("/") for a in args if not a.startswith("-")]
                if any(r in targets for r in refs):
                    out.append(f"{n}: chown takes its reference from the path it fixes (changes nothing)")
            if w == "rsync" and "sudo" in words[:i]:
                args = words[i + 1 :]
                short = "".join(a[1:] for a in args if re.fullmatch(r"-[A-Za-z]+", a))
                keeps = bool(set(short) & set("aog")) or "--archive" in args or "--owner" in args or "--group" in args
                fixed = ("--no-owner" in args and "--no-group" in args) or any(a.startswith("--chown") for a in args)
                paths = [a for a in args if not a.startswith("-")]
                if keeps and not fixed and paths and paths[-1].startswith("/var/lib/"):
                    out.append(f"{n}: rsync as root keeps the sender's owner on {paths[-1]} (add --no-owner --no-group or --chown)")
    return out


def self_test() -> int:
    cases = [
        ("sudo chown -R --reference=/var/lib/private/onyx/cas /var/lib/private/onyx/cas", 1),
        ("  sudo chown -R --reference=/var/lib/x/ /var/lib/x", 1),
        ("sudo chown -R --reference=/var/lib/private/onyx/state /var/lib/private/onyx/cas", 0),
        ("# sudo chown --reference=/a /a", 0),
        ("sudo rsync -a --delete cas/ /var/lib/private/onyx/cas/", 1),
        ("sudo rsync -rlptgoD cas/ /var/lib/private/onyx/cas/", 1),
        ("sudo rsync -rlptD --no-owner --no-group --delete cas/ /var/lib/private/onyx/cas/", 0),
        ("sudo rsync -a --chown=onyx:onyx cas/ /var/lib/onyx/cas/", 0),
        ("sudo rsync -a --no-owner cas/ /var/lib/onyx/cas/", 1),
        ("sudo rsync -a --exclude operator.key /etc/onyx/ /var/backups/onyx-prev/etc-onyx/", 0),
        ('rsync -a --delete "$export_dir/cas/" "$host:$remote/cas/"', 0),
        ('  sudo chown -R --reference=/var/lib/x /var/lib/x\n  sudo rsync -a a/ /var/lib/x/', 2),
    ]
    bad = [(t, want, problems(t)) for t, want in cases if len(problems(t)) != want]
    for t, want, got in bad:
        print(f"FAIL: {t!r}: expected {want} problem(s), got {got}")
    print(f"check-deploy-owner self-test {'ok' if not bad else 'FAILED'}: {len(cases)} cases")
    return 1 if bad else 0


def main() -> int:
    if sys.argv[1:] == ["--self-test"]:
        return self_test()
    if sys.argv[1:]:
        print(__doc__, file=sys.stderr)
        return 2
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    files = sorted(glob.glob(os.path.join(root, "scripts", "*.sh")))
    files += sorted(f for f in glob.glob(os.path.join(root, "deploy", "**", "*"), recursive=True) if os.path.isfile(f))
    found = []
    for f in files:
        with open(f, encoding="utf-8", errors="replace") as fh:
            found += [f"{os.path.relpath(f, root)}:{p}" for p in problems(fh.read())]
    for p in found:
        print(p, file=sys.stderr)
    if found:
        return 1
    print(f"deploy owner ok: {len(files)} files, no self-referenced chown, no root rsync keeping the sender's owner")
    return 0


if __name__ == "__main__":
    sys.exit(main())
