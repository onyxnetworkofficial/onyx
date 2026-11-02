#!/usr/bin/env python3
"""Run the workspace's tests as `cargo test --locked --workspace` does, with test binaries in parallel.

`cargo test` runs its test binaries one after another, so a few long ones (two property tests of
about 2 minutes) set the pace while most CPUs wait: 210 binaries took 690 s on the development
machine (2026-11-02). This builds the same tests (`cargo test --no-run`, same flags), runs up to
JOBS binaries at once, each as cargo runs it (from its package folder, with cargo's package
variables), and prints each binary's whole output when it ends. The doc tests run last with
`cargo test --doc`. Any failing binary fails the run, and so does finding no test binary.

  python3 -I scripts/test-workspace.py [--jobs N]

ONYX_TEST_JOBS sets the default for --jobs (half the CPUs, at least 2). The time of each binary
is kept in the target folder (onyx-test-times.json) so the next run starts the longest first.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

PASSED = re.compile(r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed", re.M)


def build(cargo):
    """Build every test binary; return [(exe, manifest_path, package name, version, label)]."""
    cmd = [cargo, "test", "--locked", "--workspace", "--no-run",
           "--message-format=json-render-diagnostics"]
    # Diagnostics and progress go to stderr as usual; the JSON messages come on stdout.
    p = subprocess.run(cmd, stdout=subprocess.PIPE, text=True)
    if p.returncode != 0:
        sys.exit(f"test build failed: {' '.join(cmd)} (exit {p.returncode})")
    out = []
    for line in p.stdout.splitlines():
        try:
            m = json.loads(line)
        except ValueError:
            continue
        if m.get("reason") != "compiler-artifact" or not m.get("executable"):
            continue
        if not m.get("profile", {}).get("test"):
            continue
        # package_id: "path+file:///…/crates/onyx-api#onyx-api@0.0.1" (or "…#0.0.1" when the
        # folder name is the package name).
        pid = m["package_id"]
        tail = pid.rsplit("#", 1)[-1]
        name, _, ver = tail.rpartition("@")
        if not name:
            name = os.path.basename(os.path.dirname(m["manifest_path"]))
        t = m["target"]
        kind = t["kind"][0]
        src = os.path.relpath(t["src_path"], os.path.dirname(m["manifest_path"]))
        label = f"{name} {kind} {src}" if kind != "lib" else f"{name} {src}"
        out.append((m["executable"], m["manifest_path"], name, ver, label))
    return out


def env_for(cargo, manifest, name, ver, lib_dirs):
    e = dict(os.environ)
    e.update({
        "CARGO": cargo,
        "CARGO_MANIFEST_DIR": os.path.dirname(manifest),
        "CARGO_MANIFEST_PATH": manifest,
        "CARGO_PKG_NAME": name,
        "CARGO_PKG_VERSION": ver,
    })
    e["LD_LIBRARY_PATH"] = os.pathsep.join(lib_dirs + [e.get("LD_LIBRARY_PATH", "")]).rstrip(os.pathsep)
    return e


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    default_jobs = os.environ.get("ONYX_TEST_JOBS") or str(max(2, (os.cpu_count() or 4) // 2))
    ap.add_argument("--jobs", type=int, default=int(default_jobs))
    a = ap.parse_args()
    if a.jobs < 1:
        sys.exit("--jobs must be at least 1")
    cargo = os.environ.get("CARGO") or shutil.which("cargo") or "cargo"
    bins = build(cargo)
    if not bins:
        sys.exit("no test binary was built")
    target = os.environ.get("CARGO_TARGET_DIR") or os.path.join(os.getcwd(), "target")
    lib_dirs = [os.path.join(target, "debug", "deps"), os.path.join(target, "debug")]
    times_file = os.path.join(target, "onyx-test-times.json")
    try:
        with open(times_file) as f:
            times = json.load(f)
    except (OSError, ValueError):
        times = {}
    # Longest first (by the last run), so the long ones never start last.
    bins.sort(key=lambda b: -float(times.get(b[4], 0)))
    print(f"test binaries: {len(bins)}, {a.jobs} at a time", flush=True)

    start = time.monotonic()
    queue, running, failed = list(bins), [], []
    passed_tests = 0
    try:
        while queue or running:
            while queue and len(running) < a.jobs:
                exe, manifest, name, ver, label = queue.pop(0)
                # Output to an unnamed temp file, not a pipe: a pipe read only at the end would
                # block a binary that writes more than the pipe holds.
                log = tempfile.TemporaryFile()
                proc = subprocess.Popen(
                    [exe], cwd=os.path.dirname(manifest),
                    env=env_for(cargo, manifest, name, ver, lib_dirs),
                    stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
                running.append((proc, log, label, exe, time.monotonic()))
            time.sleep(0.05)
            for r in list(running):
                proc, log, label, exe, t0 = r
                if proc.poll() is None:
                    continue
                running.remove(r)
                log.seek(0)
                out = log.read().decode("utf-8", "replace")
                log.close()
                took = time.monotonic() - t0
                times[label] = round(took, 2)
                print(f"     Running {label} ({os.path.relpath(exe, target)}) [{took:.1f} s]")
                sys.stdout.write(out if out.endswith("\n") or not out else out + "\n")
                sys.stdout.flush()
                results = PASSED.findall(out)
                passed_tests += sum(int(x[1]) for x in results)
                # A harness-less binary prints no "test result" line: its exit code decides.
                if proc.returncode != 0 or any(x[0] != "ok" for x in results):
                    failed.append((label, proc.returncode))
    except BaseException:
        for proc, *_ in running:
            proc.kill()
        raise
    wall = time.monotonic() - start
    try:
        with open(times_file + ".tmp", "w") as f:
            json.dump(times, f, indent=0, sort_keys=True)
        os.replace(times_file + ".tmp", times_file)
    except OSError:
        pass
    print(f"test binaries done: {len(bins)} run, {passed_tests} tests passed, "
          f"{len(failed)} failed, {wall:.0f} s", flush=True)
    if failed:
        for label, rc in failed:
            print(f"FAILED: {label} (exit {rc})")
        sys.exit(1)
    # The doc tests (rustdoc builds each example; a few seconds).
    rc = subprocess.run([cargo, "test", "--locked", "--workspace", "--doc"]).returncode
    if rc != 0:
        sys.exit(f"doc tests failed (exit {rc})")


if __name__ == "__main__":
    main()
