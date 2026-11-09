#!/usr/bin/env python3
"""The web server's Content-Security-Policy headers (THREATS WEB-CSP-1, WEB-CSP-2). Run by scripts/check-docs.sh.

deploy/caddy/onyx-web.Caddyfile names two policies: one for the web workers
(`/app/immutable/workers/*`, the verifier and the slab, which take their policy from their own
answer) and one for every other answer. Each answer must carry exactly one, the right one. Reading
the file is not enough: in Caddy a header block with a delete (`-Server`) is applied as the answer is
written, so a policy in it once replaced the workers' own (review WEB-CSP-R2). So:

  scripts/check-caddy-headers.py               real Caddy on a loopback copy of the Caddyfile (the
                                               site address on 127.0.0.1, its files a small planted
                                               tree): the page, a chunk and an answer the server
                                               writes itself (site policy), a worker and its wasm (worker
                                               policy). Caddy is ONYX_CADDY or `caddy` on PATH; with
                                               neither it says so and passes, except in CI (CI=true),
                                               where it fails.
  scripts/check-caddy-headers.py --self-test   also puts the old layout back (the site policy in the
                                               site-wide header block, the workers' set at once) and
                                               requires the check to fail.
  scripts/check-caddy-headers.py --live URL BUILD
                                               the deployed site: /, one chunk, and every file under
                                               BUILD/app/immutable/workers/ (after scripts/deploy-api.sh
                                               or deploy-site.sh).
"""
import glob, os, re, shutil, socket, subprocess, sys, tempfile, time, urllib.error, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CADDYFILE = os.path.join(ROOT, "deploy/caddy/onyx-web.Caddyfile")
UA = "onyx-dev"
CSP = "Content-Security-Policy"


def policies(text):
    """The worker and site policies: each a deferred header block of its own, exactly once."""
    out = {}
    for name in ("workers", "not_workers"):
        pat = r"^\s*header @%s \{\n\s*%s \"([^\"]+)\"\n\s*defer\n\s*\}$" % (name, CSP)
        found = re.findall(pat, text, re.M)
        if len(found) != 1:
            sys.exit(f"check-caddy-headers: deploy/caddy/onyx-web.Caddyfile needs one deferred `header @{name}` block with a {CSP}; found {len(found)}")
        out[name] = found[0]
    used = sum(CSP in line.split("#", 1)[0] for line in text.splitlines())
    if used != 2:
        sys.exit(f"check-caddy-headers: {CSP} must be set only in the two blocks; found {used} lines")
    return out["workers"], out["not_workers"]


def fetch(url):
    """(status, every Content-Security-Policy value) of a GET."""
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return r.status, r.headers.get_all(CSP) or []
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get_all(CSP) or []


def check(base, cases):
    """cases: (path, wanted status or None for any, wanted policy). Returns the problems."""
    bad = []
    for path, status, want in cases:
        got_status, got = fetch(base + path)
        if status is not None and got_status != status:
            bad.append(f"{path}: status {got_status}, wanted {status}")
        if got != [want]:
            bad.append(f"{path}: {CSP} {got!r}, wanted exactly [{want!r}]")
    return bad


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def loopback(text, worker, site):
    """Run real Caddy on a loopback copy; return the problems (None when no Caddy is at hand)."""
    caddy = os.environ.get("ONYX_CADDY") or shutil.which("caddy")
    if not caddy:
        return None
    tmp = tempfile.mkdtemp(prefix="onyx-caddy-")
    try:
        www = os.path.join(tmp, "www")
        files = {
            "index.html": "<!doctype html><title>x</title>",
            "app/immutable/chunks/a.js": "export const a = 1;",
            "app/immutable/workers/w.worker-x.js": "self.onmessage = () => {};",
            "app/immutable/workers/assets/v_bg-x.wasm": "\0asm\1\0\0\0",
        }
        for rel, body in files.items():
            os.makedirs(os.path.dirname(os.path.join(www, rel)), exist_ok=True)
            with open(os.path.join(www, rel), "w") as f:
                f.write(body)
        # Global options get `auto_https off`; every site address becomes a loopback http one; the
        # site's root is the planted tree.
        lines, port, n_sites, opts = [], free_port(), 0, None
        for line in text.splitlines():
            if opts is None and line.strip() and not line.lstrip().startswith("#"):
                opts = line == "{"
                if opts:
                    lines += [line, "\tauto_https off"]
                    continue
            if re.match(r"^[^\s#{}][^{]*\{$", line):
                # The first site that is not the port-80 redirect block serves the files; every
                # other site (that block included, THREATS TLS-9) gets a port of its own.
                main = n_sites == 0 and not line.startswith("http://")
                lines.append(f"http://127.0.0.1:{port if main else free_port()} {{")
                n_sites += main
                continue
            lines.append(re.sub(r"^(\s*root \* ).*$", lambda m: m.group(1) + www, line))
        if not opts or n_sites == 0:
            return ["the Caddyfile does not start with a global options block, or has no site"]
        conf = os.path.join(tmp, "Caddyfile")
        with open(conf, "w") as f:
            f.write("\n".join(lines) + "\n")
        env = dict(os.environ, XDG_CONFIG_HOME=os.path.join(tmp, "cfg"), XDG_DATA_HOME=os.path.join(tmp, "data"), HOME=tmp)
        log = open(os.path.join(tmp, "caddy.log"), "w")
        proc = subprocess.Popen([caddy, "run", "--config", conf, "--adapter", "caddyfile"], env=env, stdout=log, stderr=subprocess.STDOUT)
        base = f"http://127.0.0.1:{port}"
        try:
            for _ in range(100):
                if proc.poll() is not None:
                    break
                try:
                    socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
                    break
                except OSError:
                    time.sleep(0.1)
            if proc.poll() is not None:
                log.close()
                return ["caddy did not start: " + open(os.path.join(tmp, "caddy.log")).read()[-800:]]
            return check(base, [
                ("/", 200, site),
                ("/app/immutable/chunks/a.js", 200, site),
                ("/v1/nothing", 404, site),
                ("/app/immutable/workers/w.worker-x.js", 200, worker),
                ("/app/immutable/workers/assets/v_bg-x.wasm", 200, worker),
            ])
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
            log.close()
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def live(url, build):
    worker, site = policies(open(CADDYFILE).read())
    base = url.rstrip("/")
    imm = os.path.join(build, "app/immutable")
    chunks = sorted(glob.glob(os.path.join(imm, "chunks/*.js")))
    workers = sorted(p for p in glob.glob(os.path.join(imm, "workers/**/*"), recursive=True) if os.path.isfile(p))
    if not chunks or not workers:
        sys.exit(f"check-caddy-headers: {build} has no chunk or no worker file")
    rel = lambda p: "/" + os.path.relpath(p, build)
    cases = [("/", 200, site), (rel(chunks[0]), 200, site)] + [(rel(p), 200, worker) for p in workers]
    bad = check(base, cases)
    for b in bad:
        print("FAIL", b)
    if bad:
        sys.exit(1)
    print(f"check-caddy-headers: {base}: {len(cases)} answers carry the right {CSP} ({len(workers)} worker files)")


def main():
    args = sys.argv[1:]
    if args[:1] == ["--live"] and len(args) == 3:
        return live(args[1], args[2])
    if args not in ([], ["--self-test"]):
        sys.exit(__doc__)
    text = open(CADDYFILE).read()
    worker, site = policies(text)
    bad = loopback(text, worker, site)
    if bad is None:
        # CI installs Ubuntu's Caddy (the server's 2.6.2, .github/workflows/ci.yml): there a missing
        # one is a failure, never a pass (review WEB-CSP-R3).
        if os.environ.get("CI") == "true":
            sys.exit("check-caddy-headers: no Caddy in CI (ONYX_CADDY or caddy on PATH)")
        print("check-caddy-headers: no Caddy (ONYX_CADDY or caddy on PATH): the loopback check is skipped")
        return
    for b in bad:
        print("FAIL", b)
    if bad:
        sys.exit(1)
    print(f"check-caddy-headers: real Caddy on a loopback copy: 5 answers carry the right {CSP}")
    if args == ["--self-test"]:
        # The layout review WEB-CSP-R2 caught: the site policy in the site-wide header block (deferred
        # by its -Server) and the workers' policy set at once, so the site-wide one replaced it.
        # The site-wide block is the one with Cross-Origin-Opener-Policy (the port-80 block also
        # deletes Server, THREATS TLS-9).
        coop = '\t\tCross-Origin-Opener-Policy "same-origin"\n'
        faulty = text.replace(coop, coop + f'\t\t{CSP} "{site}"\n', 1)
        faulty = re.sub(r"(header @workers \{\n\s*%s \"[^\"]+\")\n\s*defer\n" % CSP, r"\1\n", faulty, count=1)
        if text.count(coop) != 1 or faulty.count("defer") != text.count("defer") - 1:
            sys.exit("check-caddy-headers --self-test: could not plant the fault")
        # Caught for the right reason only: both worker answers served (200) with the site's policy,
        # and every other answer right; any other failure (Caddy not starting, a port taken) is not
        # a catch (review WEB-CSP-R3).
        got = loopback(faulty, worker, site) or []
        want = [f"{p}: {CSP} {[site]!r}, wanted exactly [{worker!r}]" for p in ("/app/immutable/workers/w.worker-x.js", "/app/immutable/workers/assets/v_bg-x.wasm")]
        if got != want:
            sys.exit("check-caddy-headers --self-test: the old layout (site policy in the site-wide block, the workers' set at once) was not caught as such: " + "; ".join(got or ["no failure"]))
        print("check-caddy-headers --self-test: the old layout (site policy in the site-wide block, the workers' set at once) is caught")


if __name__ == "__main__":
    main()
