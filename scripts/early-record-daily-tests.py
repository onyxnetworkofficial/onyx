#!/usr/bin/env python3
# Tests of scripts/early-record-daily.sh (ROADMAP S26 slice 3, THREATS EARLY-2, EARLY-4, EARLY-9,
# EARLY-12, EARLY-17, EARLY-19) with stub early-record and onyx-publish programs on PATH: no chain, no node,
# no key. The real privacy scan runs, on a stand-in indexer root that holds an empty copy of each
# list it opens plus one made-up name. Checks: config refusals (exit 2: the same node in another
# spelling, a missing output root, the output root lost while the state keeps its days); the dry run
# scans, builds the bulk versions, verifies them against the SECOND node (shard roots included) and
# builds a catalog, day by day, with --prev, and signs, anchors and uploads nothing; a rerun rebuilds
# nothing; the real run signs, anchors and uploads each day (catalog N, then the versions with
# --catalog N) before the next day starts; a failed anchor, catalog upload or upload resumes
# without sending anything twice; a staged folder left by a stopped run is taken up; a relative
# output root is scanned (never a scan of nothing); a scan of a missing or empty folder fails; a catalog whose number is not its place is set aside,
# never signed; a second node that differs (exit 3) or a privacy hit (exit 4) leaves that day
# unpublished; a held lock is exit 5; the node URLs and the key path
# are never printed and the key file is never read.
# The retry of the last stages (docs/READINESS.md A27, THREATS EARLY-21, RETRY-R1-H2, RETRY-R1-M1,
# RETRY-R1-L3): a stop that passes by itself (onyx-publish exit 75 with its `TRANSIENT class=`
# line) is tried again, N failures then done is N+1 tries and exit 0, and no finished stage runs
# twice (but the anchor is read again while it is not final, and its marker cleared when the wait
# gives up: a reorg); a stop that never passes ends with exit 6 after the tries or the budget of
# pauses and failed tries, whichever is first, and no try waits for finality longer than what is
# left of the budget; a refusal ends with exit 7 after one try and no pause; a killed tool is
# exit 1; exits 6 and 7 log one line with the alert tag (a stub `logger`); the texts of an older
# tool are read only when the tool's usage does not name exit 75; settings that are not whole
# numbers in range are exit 2; and the unit file's start limit is reached by a day that keeps
# failing (computed from the template's own numbers). The pauses are a stub `sleep` on PATH that
# only writes its seconds down, so no test waits.
# Review RETRY-R2-M4: the whole run is time-boxed (ONYX_EARLY_RUN_SECS): every tool runs under
# `timeout` with the time left (a stub that writes its seconds down and, when told, says the time
# is up), a run whose time is up at any step ends with exit 8 and the next run goes on, four runs
# and four restart gaps fit the unit's window (from the template's numbers), and EVERY exit but 0
# logs the alert line.
# Run: python3 -I scripts/early-record-daily-tests.py   (check-all runs it; about 15 s, more on a busy machine)
import fcntl
import json
import os
import re
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "early-record-daily.sh"
RPC1 = "https://rpc-one.invalid/v2/secret-one"
RPC2 = "https://rpc-two.invalid/v2/secret-two"
D1, D2, D3 = "2026-11-13", "2026-11-14", "2026-11-15"
PLANTED = "zqxplantedname"  # the stand-in indexer's only entity (a made-up name)

EARLY_RECORD = r'''
import json, os, sys
a = sys.argv[1:]
cmd, o = a[0], dict(zip(a[1::2], a[2::2]))
with open(os.path.join(os.environ["FAKE_CALLS"], "calls.jsonl"), "a") as f:
    f.write(json.dumps({"tool": "early-record", "cmd": cmd, "rpc": os.environ.get("ONYX_EARLY_RPC"), "args": a}) + "\n")
env = lambda k: [d for d in os.environ.get(k, "").split(",") if d]
if cmd == "build":
    if os.environ.get("FAKE_BUILD_FAIL"):
        sys.exit("early-record: node: eth_getLogs 1..=200: rate-limited past the retries; run again to go on")
    sp = os.path.join(o["--state"], "state.json")
    os.makedirs(os.path.join(o["--state"], "before"), exist_ok=True)
    built = json.load(open(sp))["built"] if os.path.exists(sp) else []
    dj = os.path.join(o["--out"], "day.json")
    if os.path.exists(dj) and json.load(open(dj))["day"] in built:
        print(json.dumps({"already_built": json.load(open(dj))["day"]})); sys.exit(0)
    if "--prev" in o and (not built or json.load(open(os.path.join(o["--prev"], "day.json")))["day"] != built[-1]):
        sys.exit("early-record: folder: --prev is not the state's last day")
    left = [d for d in env("FAKE_DAYS") if d not in built]
    if not left:
        print(json.dumps({"nothing_new": "no new complete day at or below the finalized block"})); sys.exit(0)
    d = left[0]
    json.dump({"built": built}, open(os.path.join(o["--state"], "before", d + ".json"), "w"))
    os.makedirs(o["--out"], exist_ok=True)
    extra = os.environ["FAKE_PLANT"] + "," if d in env("FAKE_PLANT_DAYS") else ""
    open(os.path.join(o["--out"], "data.csv"), "w").write("key,kind,detail\n1," + extra + "signed_report,accepted\n2,day_close,x\n")
    open(os.path.join(o["--out"], "schema.json"), "w").write("{}\n")
    if d in env("FAKE_EXCL_DAYS"):
        os.makedirs(os.path.join(o["--out"], "exclusions"))
        for n in ("data.csv", "schema.json"):
            open(os.path.join(o["--out"], "exclusions", n), "w").write("key\n1\n")
    json.dump({"day": d}, open(dj, "w"))
    json.dump({"built": built + [d], "last": {"day": 20770 + len(built)}}, open(sp, "w"))
    if d in env("FAKE_CRASH_AFTER"):
        sys.exit("killed after the folder and the state were written")
    print(json.dumps({"built": d, "rows": 2, "out": o["--out"], "skipped_publisher_keys": [], "days_without_blocks": []}))
elif cmd == "verify":
    d = json.load(open(os.path.join(o["--dir"], "day.json")))["day"]
    if d in env("FAKE_VERIFY_DIFF"):
        sys.exit("early-record: folder: " + o["--dir"] + ": data.csv differs from the chain's: the folder is never built again")
    if d in env("FAKE_VERIFY_DOWN"):
        sys.exit("early-record: node: eth_getBlockByNumber: timed out")
    roots = lambda p: int(os.path.exists(os.path.join(o["--dir"], p, "manifest.bin")) and d not in env("FAKE_NO_ROOTS"))
    print(json.dumps({"verified": d, "rows": 2, "shard_roots": roots(""), "exclusion_shard_roots": roots("exclusions")}))
'''

ONYX_PUBLISH = r'''
import json, os, sys
a = sys.argv[1:]
cmd, o = a[0], dict(zip(a[1::2], a[2::2]))
if cmd == "--help":
    # The usage text of a tool with exit 75 names it; FAKE_OLD is a tool built before it.
    print("usage: onyx-publish <command> [options]")
    if not os.environ.get("FAKE_OLD"):
        print("anchor, catalog-anchor, upload, catalog-upload: exit 75 = a stop that passes by itself (last line TRANSIENT class=...)")
    sys.exit(0)
with open(os.path.join(os.environ["FAKE_CALLS"], "calls.jsonl"), "a") as f:
    f.write(json.dumps({"tool": "onyx-publish", "cmd": cmd, "args": a}) + "\n")
day = lambda p: os.path.basename(p).split(".")[0]
def nth(name):
    """How many times this was asked before, counted in a file."""
    count = os.path.join(os.environ["FAKE_CALLS"], "fails." + name.replace("/", "_"))
    seen = int(open(count).read()) if os.path.exists(count) else 0
    open(count, "w").write(str(seen + 1))
    return seen
if cmd == "build":
    m = os.path.join(o["--out"], "manifest.bin")
    if os.path.exists(m):
        sys.exit("onyx-publish build: already holds a manifest")
    v = json.load(open(os.path.join(o["--prev"], "manifest.bin")))["version"] + 1 if "--prev" in o else 1
    json.dump({"version": v, "dataset": o["--dataset"]}, open(m, "w"))
    print(f"BUILT version={v} rows=2")
elif cmd == "catalog-build":
    if os.path.exists(o["--out"]):
        sys.exit("onyx-publish catalog-build: exists")
    n = json.load(open(o["--prev"]))["number"] + 1 if "--prev" in o else 0
    n += int(os.environ.get("FAKE_CAT_SKEW", "0"))
    json.dump({"number": n, "dirs": open(o["--dirs"]).read().split()}, open(o["--out"], "w"))
    print(f"CATALOG number={n} leaves=2 out={o['--out']}")
elif cmd in ("catalog-anchor", "catalog-upload", "upload") and cmd in dict(
        x.split("=") for x in os.environ.get("FAKE_TRANSIENT", "").split(",") if x):
    # FAKE_TRANSIENT=cmd=N[r],...: the first N runs of cmd (for one catalog or folder) stop in a
    # way that passes by itself, then it is done (or, with r, refused).
    spec = dict(x.split("=") for x in os.environ["FAKE_TRANSIENT"].split(","))[cmd]
    target = o.get("--catalog") if cmd != "upload" else o["--dir"]
    seen = nth(cmd + "." + target)
    if seen < int(spec.rstrip("r")):
        if cmd == "catalog-anchor":
            cls, text = "not_mined", "postReport: transaction 0x" + "ab" * 32 + " not mined before the deadline"
        else:
            what = "catalog" if cmd == "catalog-upload" else "manifest"
            print(what + ": waiting for the anchor to be final (not_recorded_at_finalized_block; 5 s so far)", file=sys.stderr)
            cls, text = "not_final", what + ' refused (409): {"error":{"code":"bulk_not_anchored","message":"not final"},"reason":"not_recorded_at_finalized_block"}'
        print("onyx-publish " + cmd + ": " + text, file=sys.stderr)
        if os.environ.get("FAKE_OLD"):
            sys.exit(1)
        print("onyx-publish " + cmd + ": TRANSIENT class=" + cls + " (exit 75: nothing is lost, run the same command again)", file=sys.stderr)
        sys.exit(75)
    if spec.endswith("r"):
        sys.exit("onyx-publish " + cmd + ": the node at --rpc is not chain 84532")
    print("DONE " + cmd)
elif cmd == "catalog-sign":
    open(o["--catalog"] + ".report", "w").write("report\n")
    print("SIGNED catalog")
elif cmd == "catalog-anchor":
    if day(o["--catalog"]) in os.environ.get("FAKE_ANCHOR_FAIL", ""):
        sys.exit(os.environ.get("FAKE_ANCHOR_SAYS", "onyx-publish catalog-anchor: postReport: would revert, not sent: Stale()"))
    if os.environ.get("FAKE_ANCHOR_EXIT"):
        sys.exit(int(os.environ["FAKE_ANCHOR_EXIT"]))
    # FAKE_ANCHOR_AGAIN: what the anchor says from its second run on for one catalog (the run
    # while the anchor is not final): refuse, or a stop that passes.
    again = os.environ.get("FAKE_ANCHOR_AGAIN")
    if again and nth("again." + o["--catalog"]) >= 1:
        if again == "refuse":
            sys.exit("onyx-publish catalog-anchor: this report's transaction (nonce 7) is still unmined: not raised again")
        print("onyx-publish catalog-anchor: eth_call: node answered HTTP 503", file=sys.stderr)
        print("onyx-publish catalog-anchor: TRANSIENT class=rpc_http (exit 75: nothing is lost, run the same command again)", file=sys.stderr)
        sys.exit(75)
    print("ANCHORED tx=0x1")
elif cmd == "catalog-upload":
    if day(o["--catalog"]) in os.environ.get("FAKE_CATUP_FAIL", ""):
        sys.exit("onyx-publish catalog-upload: the node did not answer")
    print("UPLOADED catalog")
else:
    if os.environ.get("FAKE_UPLOAD_FAIL") and o["--dir"].endswith(os.environ["FAKE_UPLOAD_FAIL"]):
        sys.exit("onyx-publish upload: the node did not answer")
    print("UPLOADED")
'''

# `sleep`: writes its seconds down and returns at once.
SLEEP = r'''
import os, sys
with open(os.path.join(os.environ["FAKE_CALLS"], "sleeps"), "a") as f:
    f.write(sys.argv[1] + "\n")
'''

# `timeout -k 30 SECS command...`: writes the seconds and the command down, then runs it; or, for
# a command named in FAKE_TIMEUP ("tool:first-argument"), says the time is up (124) and runs nothing.
TIMEOUT = r'''#!/bin/sh
secs=$3
shift 3
name=$(basename "$1"):${2:-}
echo "$secs $name" >>"$FAKE_CALLS/timeouts"
case " ${FAKE_TIMEUP:-} " in *" $name "*) exit 124 ;; esac
exec "$@"
'''

# `logger`: writes its arguments down (one alert a line), never to a real log.
LOGGER = r'''
import json, os, sys
with open(os.path.join(os.environ["FAKE_CALLS"], "alerts"), "a") as f:
    f.write(json.dumps(sys.argv[1:]) + "\n")
'''


def stub(path, body):
    path.write_text("#!/usr/bin/env -S python3 -I\n" + textwrap.dedent(body))
    path.chmod(0o755)


def indexer(root):
    """A stand-in indexer root: run the scan on an empty folder and make each list it misses, an
    empty CSV (one made-up entity) or an empty JSON list, until it runs."""
    empty = root / "empty"
    empty.mkdir()
    for _ in range(40):
        r = subprocess.run([sys.executable, "-I", str(HERE / "audit-privacy-scan.py"), str(HERE.parent),
                            str(root / "idx"), str(empty)], capture_output=True, text=True)
        if r.returncode == 0:
            return root / "idx"
        miss = r.stderr.strip().splitlines()[-1].split("'")
        assert "No such file" in r.stderr and len(miss) >= 2, r.stderr
        p = Path(miss[-2])
        assert str(p).startswith(str(root / "idx")), p
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text("[]\n" if p.suffix == ".json" else f"address,entity\n,{PLANTED}\n")
    raise AssertionError("the scan never ran")


class Daily(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.idx = indexer(Path(cls.tmp.name))

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def setUp(self):
        self.t = Path(tempfile.mkdtemp(dir=self.tmp.name))
        self.bin, self.calldir, self.out = self.t / "bin", self.t / "calls", self.t / "out"
        for d in (self.bin, self.calldir, self.out):
            d.mkdir()
        stub(self.bin / "early-record", EARLY_RECORD)
        stub(self.bin / "onyx-publish", ONYX_PUBLISH)
        stub(self.bin / "sleep", SLEEP)
        stub(self.bin / "logger", LOGGER)
        (self.bin / "timeout").write_text(TIMEOUT)
        (self.bin / "timeout").chmod(0o755)
        # The key file is unreadable: a script that read it would fail (a stub never opens it).
        self.key = self.t / "record.key"
        self.key.write_text("not a key\n")
        self.key.chmod(0)
        self.env = {
            "PATH": f"{self.bin}:{os.environ.get('PATH', '/usr/bin:/bin')}", "HOME": str(self.t),
            "TMPDIR": str(self.t),
            "FAKE_CALLS": str(self.calldir), "FAKE_DAYS": f"{D1},{D2}", "FAKE_EXCL_DAYS": D1, "FAKE_PLANT": PLANTED,
            "ONYX_EARLY_RPC": RPC1, "ONYX_EARLY_RPC2": RPC2, "ONYX_EARLY_STATE": str(self.t / "state"),
            "ONYX_EARLY_OUT": str(self.out), "ONYX_EARLY_PUBLISHER": "early-record",
            "ONYX_EARLY_INDEXER": str(self.idx), "ONYX_EARLY_KEY": str(self.key),
            "ONYX_EARLY_ANCHOR_RPC": "https://anchor.invalid", "ONYX_EARLY_BOOK": "0x" + "c0de".rjust(40, "0"),
            "ONYX_EARLY_NODE": "https://node.invalid", "ONYX_EARLY_WAIT_SECS": "5",
        }

    def run_daily(self, *args, want=0, cwd=None, **env):
        e = dict(self.env, **env)
        for k in [k for k, v in e.items() if v is None]:
            del e[k]
        r = subprocess.run(["bash", str(SCRIPT), *args], env=e, capture_output=True, text=True, timeout=60,
                           cwd=cwd)
        text = r.stdout + r.stderr
        self.assertEqual(r.returncode, want, text)
        logged = (self.calldir / "alerts").read_text() if (self.calldir / "alerts").exists() else ""
        for secret in (RPC1, RPC2, "secret-one", "secret-two", str(self.key)):
            self.assertNotIn(secret, text, "a node URL or the key path was printed")
            self.assertNotIn(secret, logged, "a node URL or the key path was in an alert line")
        if r.returncode != 0 and "unknown argument" not in text:
            # RETRY-R2-M4: every exit but 0 logs the alert line, with its exit code.
            self.assertIn(f"onyx-early-record exit {r.returncode}: ", logged)
        return text

    def calls(self, tool=None, cmd=None):
        p = self.calldir / "calls.jsonl"
        lines = [json.loads(x) for x in p.read_text().splitlines()] if p.exists() else []
        p.unlink(missing_ok=True)
        return [c for c in lines if (tool is None or c["tool"] == tool) and (cmd is None or c["cmd"] == cmd)]

    def forget_fails(self):
        """Start the stub's count of failed runs again (FAKE_TRANSIENT)."""
        for p in self.calldir.glob("fails.*"):
            p.unlink()

    def sleeps(self):
        p = self.calldir / "sleeps"
        got = [int(x) for x in p.read_text().split()] if p.exists() else []
        p.unlink(missing_ok=True)
        return got

    def timeouts(self):
        """The (seconds, command) each tool was given since the last look."""
        p = self.calldir / "timeouts"
        got = [(int(x.split()[0]), x.split()[1]) for x in p.read_text().splitlines()] if p.exists() else []
        p.unlink(missing_ok=True)
        return got

    def alerts(self):
        """The alert lines logged since the last look: each as logger's arguments."""
        p = self.calldir / "alerts"
        got = [json.loads(x) for x in p.read_text().splitlines()] if p.exists() else []
        p.unlink(missing_ok=True)
        return got

    def opt(self, call, k):
        a = call["args"]
        return a[a.index(k) + 1] if k in a else None

    def test_config_refusals(self):
        self.run_daily(want=2, ONYX_EARLY_RPC2=None)
        self.run_daily(want=2, ONYX_EARLY_RPC2=RPC1)
        # The same scheme://host:port in another spelling is the same node.
        self.run_daily(want=2, ONYX_EARLY_RPC2="HTTPS://RPC-ONE.invalid:443/v3/another-key")
        self.run_daily(want=2, ONYX_EARLY_RPC2="https://rpc-one.invalid:bad/")
        self.run_daily(want=2, ONYX_EARLY_OUT=str(self.t / "no-such-folder"))
        self.run_daily(want=2, ONYX_EARLY_PUBLISHER="Not An Id")
        self.run_daily(want=2, ONYX_EARLY_ANCHOR_RPC="https://anchor.invalid/v2/key")
        self.run_daily(want=2, ONYX_EARLY_ANCHOR_RPC="https://user@localhost:8545")
        self.run_daily(want=2, ONYX_EARLY_KEY=str(self.t / "missing.key"))
        self.run_daily("--bogus", want=2)
        # The dry run needs no key, no anchor node, no book and no upload node.
        self.run_daily("--dry-run", ONYX_EARLY_KEY=None, ONYX_EARLY_ANCHOR_RPC=None, ONYX_EARLY_BOOK=None,
                       ONYX_EARLY_NODE=None)
        self.assertEqual(self.calls("onyx-publish", "catalog-sign"), [])

    def test_dry_run_then_real_run_then_resume(self):
        self.run_daily("--dry-run")
        v = self.out / "versions"
        self.assertEqual(sorted(p.name for p in v.iterdir()), [D1, D2])
        c = self.calls()
        # Day by day: the bulk versions, then verify (which compares their shard roots), then the
        # catalog; day 2 starts after day 1 is done.
        self.assertEqual([(x["tool"][0], x["cmd"]) for x in c][3:], [
            ("o", "build"), ("o", "build"), ("e", "verify"), ("o", "catalog-build"),
            ("o", "build"), ("e", "verify"), ("o", "catalog-build")])
        builds = [x for x in c if x["cmd"] == "build" and x["tool"] == "early-record"]
        self.assertEqual(len(builds), 3)  # day 1, day 2, nothing new
        self.assertTrue(all(x["rpc"] == RPC1 for x in builds))
        self.assertEqual(self.opt(builds[1], "--prev"), str(v / D1))
        verifies = [x for x in c if x["cmd"] == "verify"]
        self.assertEqual([x["rpc"] for x in verifies], [RPC2, RPC2], "verify runs against the second node")
        self.assertEqual([self.opt(x, "--state-before") for x in verifies],
                         [str(self.t / "state" / "before" / f"{d}.json") for d in (D1, D2)])
        bulk = [(self.opt(x, "--out"), self.opt(x, "--dataset"), self.opt(x, "--prev"), self.opt(x, "--publisher"))
                for x in c if x["tool"] == "onyx-publish" and x["cmd"] == "build"]
        self.assertEqual(bulk, [
            (str(v / D1), "testnet.early-contributors", None, "early-record"),
            (str(v / D1 / "exclusions"), "testnet.early-contributors.exclusions", None, "early-record"),
            (str(v / D2), "testnet.early-contributors", str(v / D1), "early-record"),
        ])
        # Each version is created at the end of its day (00:00 UTC of the next day).
        self.assertEqual([self.opt(x, "--created") for x in c if x["tool"] == "onyx-publish" and x["cmd"] == "build"],
                         ["1794614400", "1794614400", "1794700800"])
        cats = [(self.opt(x, "--out"), self.opt(x, "--prev")) for x in c if x["cmd"] == "catalog-build"]
        cdir = self.out / "catalogs"
        self.assertEqual(cats, [(str(cdir / f"{D1}.bin"), None), (str(cdir / f"{D2}.bin"), str(cdir / f"{D1}.bin"))])
        self.assertEqual((cdir / f"{D1}.dirs").read_text().split(), [str(v / D1), str(v / D1 / "exclusions")])
        self.assertFalse([x for x in c if x["cmd"] in ("catalog-sign", "catalog-anchor", "catalog-upload", "upload")])
        # A second dry run builds, verifies and scans nothing again.
        self.run_daily("--dry-run")
        self.assertEqual([(x["tool"], x["cmd"]) for x in self.calls()], [("early-record", "build")])
        # The real run: each day signed, anchored and uploaded before the next day starts.
        self.run_daily()
        c = self.calls()
        self.assertEqual([x["cmd"] for x in c], ["build", "catalog-sign", "catalog-anchor", "catalog-upload",
                                                 "upload", "upload", "catalog-sign", "catalog-anchor",
                                                 "catalog-upload", "upload"])
        for x in c:
            if x["cmd"] in ("catalog-sign", "catalog-anchor"):
                self.assertEqual(self.opt(x, "--key") or self.opt(x, "--gas-key"), str(self.key))
            if x["cmd"] == "catalog-anchor":
                self.assertEqual(self.opt(x, "--rpc"), "https://anchor.invalid")
        ups = [(self.opt(x, "--dir"), self.opt(x, "--catalog")) for x in c if x["cmd"] == "upload"]
        self.assertEqual(ups, [(str(v / D1), "0"), (str(v / D1 / "exclusions"), "0"), (str(v / D2), "1")])
        self.assertIn("nothing new", self.run_daily())
        # Day 3: its anchor fails, then the rerun sends only what is left.
        self.calls()
        self.run_daily(want=7, FAKE_DAYS=f"{D1},{D2},{D3}", FAKE_ANCHOR_FAIL=D3)
        self.calls()
        self.run_daily(FAKE_DAYS=f"{D1},{D2},{D3}")
        c = self.calls()
        self.assertEqual([x["cmd"] for x in c], ["build", "catalog-anchor", "catalog-upload", "upload"])
        self.assertEqual(self.opt(c[-1], "--catalog"), "2")
        self.assertEqual(self.opt(c[-1], "--dir"), str(v / D3))

    def test_a_second_node_that_differs_stops_publication(self):
        self.run_daily(want=3, FAKE_VERIFY_DIFF=D2)
        c = self.calls()
        v = self.out / "versions"
        # Day 1 is published all the way (never left anchored and not uploaded); day 2 stops at
        # its verify, before any catalog, signature or upload of it.
        self.assertEqual([self.opt(x, "--out") for x in c if x["cmd"] == "catalog-build"],
                         [str(self.out / "catalogs" / f"{D1}.bin")])
        self.assertEqual([self.opt(x, "--catalog") for x in c if x["cmd"] in ("catalog-sign", "catalog-anchor",
                                                                             "catalog-upload")],
                         [str(self.out / "catalogs" / f"{D1}.bin")] * 3)
        self.assertEqual([self.opt(x, "--dir") for x in c if x["cmd"] == "upload"],
                         [str(v / D1), str(v / D1 / "exclusions")])
        self.assertTrue((self.out / "done" / f"{D1}.published").exists())
        self.run_daily(want=3, FAKE_VERIFY_DIFF=D2)
        # A second node that is down is a retry, not a difference.
        self.run_daily(want=1, FAKE_VERIFY_DOWN=D2)

    def test_a_catalog_out_of_place_is_never_signed(self):
        self.run_daily(want=1, FAKE_CAT_SKEW="1")
        cdir = self.out / "catalogs"
        self.assertFalse((cdir / f"{D1}.bin").exists())
        self.assertTrue((cdir / f"{D1}.bin.refused").exists())
        self.assertFalse(self.calls("onyx-publish", "catalog-sign"))
        self.run_daily()  # built again in its place, then signed with the next day's
        self.assertEqual(len(self.calls("onyx-publish", "catalog-sign")), 2)
        self.assertEqual([json.loads((cdir / f"{d}.bin").read_text())["number"] for d in (D1, D2)], [0, 1])

    def test_upload_failures_resume(self):
        # An upload that stops: the rerun uploads again (it sends only what the node misses) and
        # anchors, signs and uploads the catalog no second time.
        self.run_daily(want=7, FAKE_UPLOAD_FAIL="exclusions")
        self.calls()
        self.run_daily()
        c = self.calls()
        self.assertEqual([(x["cmd"], self.opt(x, "--dir")) for x in c if x["cmd"] == "upload"][:2],
                         [("upload", str(self.out / "versions" / D1)),
                          ("upload", str(self.out / "versions" / D1 / "exclusions"))])
        cats = [self.opt(x, "--catalog") or self.opt(x, "--out") for x in c if x["cmd"].startswith("catalog-")]
        self.assertEqual(cats,
                         [str(self.out / "catalogs" / f"{D2}.bin")] * 4, "day 1's catalog is not sent again")
        # A catalog upload that stops: the rerun uploads it, then the version, nothing else.
        self.run_daily(want=7, FAKE_DAYS=f"{D1},{D2},{D3}", FAKE_CATUP_FAIL=D3)
        self.calls()
        self.run_daily(FAKE_DAYS=f"{D1},{D2},{D3}")
        self.assertEqual([x["cmd"] for x in self.calls()], ["build", "catalog-upload", "upload"])

    def test_a_stop_that_passes_is_tried_again(self):
        # Day 1 (with exclusions): the anchor is not mined in time twice, the catalog upload and
        # each folder's upload meet an anchor that is not final once.
        text = self.run_daily(FAKE_DAYS=D1, FAKE_TRANSIENT="catalog-anchor=2,catalog-upload=1,upload=1")
        c = [x["cmd"] for x in self.calls("onyx-publish")]
        # N failures, N+1 tries; a stage that is done (the signature, the catalog upload, the
        # first folder) is never run again. The anchor alone is read again before each new try
        # of a wait that was not final (RETRY-R1-M1: the record may have left the chain).
        self.assertEqual(c[c.index("catalog-sign"):], ["catalog-sign"] + ["catalog-anchor"] * 3
                         + ["catalog-upload", "catalog-anchor", "catalog-upload"]
                         + ["upload", "catalog-anchor", "upload"] * 2)
        self.assertEqual(self.sleeps(), [300, 600, 300, 300, 300])
        for line in (f"{D1}: catalog-anchor try 2 of 5 after 300 s: not_mined",
                     f"{D1}: catalog-anchor try 3 of 5 after 600 s: not_mined",
                     f"{D1}: catalog-anchor done at try 3 of 5",
                     f"{D1}: catalog-upload try 2 of 5 after 300 s: not_final",
                     f"{D1}: catalog-upload done at try 2 of 5",
                     f"{D1}: upload try 2 of 5 after 300 s: not_final",
                     f"{D1}: upload-exclusions try 2 of 5 after 300 s: not_final",
                     f"{D1}: upload-exclusions done at try 2 of 5",
                     f"{D1}: published (catalog 0)",
                     "done: the last stages paused 1800 s in all between tries"):
            self.assertIn(f"early-record-daily: {line}\n", text)
        # The tool's own lines still reach the log, the waiting ones too.
        self.assertIn("not mined before the deadline", text)
        self.assertIn("waiting for the anchor to be final", text)
        self.assertTrue((self.out / "done" / f"{D1}.published").exists())
        self.assertEqual(self.alerts(), [], "a run that ends well alerts nobody")
        self.assertIn("nothing new", self.run_daily(FAKE_DAYS=D1))
        self.assertEqual(self.sleeps(), [])

    def test_the_anchor_is_read_again_while_it_is_not_final(self):
        # RETRY-R1-M1: the day is marked anchored from a read at the newest block. If that block
        # is reorganized away the anchor would be skipped for good. While the wait says "not
        # final" the anchor runs again (it reads the book first); a refusal there stops the run.
        text = self.run_daily(want=7, FAKE_DAYS=D1, FAKE_TRANSIENT="catalog-upload=2", FAKE_ANCHOR_AGAIN="refuse")
        self.assertIn(f"{D1}: catalog-anchor, run again while the anchor is not final, was refused", text)
        self.assertFalse((self.out / "done" / f"{D1}.anchored").exists())
        self.assertEqual([x["cmd"] for x in self.calls("onyx-publish")][-3:],
                         ["catalog-anchor", "catalog-upload", "catalog-anchor"])
        self.assertEqual(len(self.alerts()), 1)
        # Its own passing stop does not end the wait: the upload is tried again and gets through.
        self.forget_fails()
        self.sleeps()
        text = self.run_daily(FAKE_DAYS=D1, FAKE_TRANSIENT="catalog-upload=1", FAKE_ANCHOR_AGAIN="pass")
        self.assertIn(f"{D1}: catalog-anchor, run again while the anchor is not final, stopped (rpc_http): the wait goes on", text)
        self.assertIn(f"{D1}: published (catalog 0)", text)
        # A wait that gives up on "not final" clears the marker: the next run anchors again.
        self.forget_fails()
        self.calls()
        self.run_daily(want=6, FAKE_DAYS=f"{D1},{D2}", FAKE_TRANSIENT="catalog-upload=999", ONYX_EARLY_RETRY_TRIES="2")
        self.assertFalse((self.out / "done" / f"{D2}.anchored").exists())
        self.calls()
        self.run_daily(FAKE_DAYS=f"{D1},{D2}")
        self.assertEqual([x["cmd"] for x in self.calls()], ["build", "catalog-anchor", "catalog-upload", "upload"])

    def test_an_older_tool_without_the_exit_code_is_tried_again_by_its_text(self):
        text = self.run_daily(FAKE_DAYS=D1, FAKE_OLD="1", FAKE_TRANSIENT="catalog-anchor=1,catalog-upload=1")
        self.assertEqual(self.sleeps(), [300, 300])
        self.assertIn(f"{D1}: catalog-anchor try 2 of 5 after 300 s: not_mined", text)
        self.assertIn(f"{D1}: catalog-upload try 2 of 5 after 300 s: not_final", text)
        self.assertEqual(len(self.calls("onyx-publish", "catalog-anchor")), 3)  # two tries, one read again
        # Its other texts: each is that tool's last line for a stop that passes, or not.
        texts = (
            ("onyx-publish catalog-anchor: eth_chainId: node answered HTTP 503", "rpc_http"),
            ("onyx-publish catalog-anchor: postReport: rpc: eth_estimateGas: node answered HTTP 429", "rpc_http"),
            ("onyx-publish catalog-anchor: eth_call: node unreachable: connection refused", "unreachable"),
            # The run of 2026-11-18 started by hand 28 s after the first: the same bytes again.
            ('onyx-publish catalog-anchor: postReport: rpc: eth_sendRawTransaction: node error: '
             '{"code":-32000,"message":"already known"}', "earlier_pending"),
            ("onyx-publish catalog-anchor: eth_chainId: node call timed out", "unreachable"),
            ('onyx-publish catalog-upload: catalog refused (503): {"error":{"code":"bulk_chain_unavailable"}}', "node_busy"),
            ("onyx-publish catalog-upload: catalog refused (502): null", "node_busy"))
        for says, reason in texts:
            # Day 2 is built, scanned and verified once; its anchor never gets through here.
            text = self.run_daily(want=6, FAKE_OLD="1", FAKE_ANCHOR_FAIL=D2, FAKE_ANCHOR_SAYS=says,
                                  ONYX_EARLY_RETRY_TRIES="2")
            self.assertIn(f"{D2}: catalog-anchor try 2 of 2 after 300 s: {reason}\n", text, says)
            self.assertEqual(self.sleeps(), [300])
        # RETRY-R1-L3: a tool that has exit 75 says "passes by itself" with it and with nothing
        # else. Its exit 1 is a refusal whatever the text looks like.
        for says, _ in texts[:4]:
            self.run_daily(want=7, FAKE_ANCHOR_FAIL=D2, FAKE_ANCHOR_SAYS=says)
            self.assertEqual(self.sleeps(), [], says)

    def test_a_stop_that_never_passes_ends_within_the_bound(self):
        forever = dict(FAKE_DAYS=D1, FAKE_TRANSIENT="catalog-anchor=999")
        text = self.run_daily(want=6, **forever)
        self.assertEqual(len(self.calls("onyx-publish", "catalog-anchor")), 5)
        pauses = self.sleeps()
        self.assertEqual(pauses, [300, 600, 1200, 2400])
        self.assertLessEqual(sum(pauses), 5400, "within the default budget")
        self.assertRegex(text, rf"{D1}: catalog-anchor gave up after 5 of 5 tries \(not_mined; 45\d\d s of the run's 5400 s budget used\)")
        self.assertFalse((self.out / "done" / f"{D1}.anchored").exists())
        # RETRY-R1-H2: exit 6 is loud by itself: one line with the unit's alert tag, at error
        # priority, naming the day, the stage and the class, and no URL or key path.
        (alert,) = self.alerts()
        self.assertEqual(alert[:5], ["-t", "onyx-early-record-alert", "-p", "user.err", "--"])
        self.assertRegex(alert[5], rf"^onyx-early-record exit 6: {D1}: catalog-anchor gave up after 5 of 5 tries \(not_mined;")
        for secret in (RPC1, RPC2, str(self.key), "anchor.invalid", "node.invalid"):
            self.assertNotIn(secret, alert[5])
        # The budget of the whole run ends it before the tries do: 100 + 200 + 400, and the next
        # pause (800) would pass 1000.
        self.calls()
        text = self.run_daily(want=6, ONYX_EARLY_RETRY_TRIES="20", ONYX_EARLY_RETRY_SECS="100",
                              ONYX_EARLY_RETRY_BUDGET_SECS="1000", **forever)
        pauses = self.sleeps()
        self.assertEqual(pauses, [100, 200, 400])
        self.assertLessEqual(sum(pauses), 1000)
        self.assertRegex(text, r"gave up after 4 of 20 tries \(not_mined\): the next pause of 800 s is past the run's budget \(7\d\d of 1000 s used\)")
        self.assertEqual(len(self.calls("onyx-publish", "catalog-anchor")), 4)
        self.assertEqual(len(self.alerts()), 1)
        # The budget is the run's, not a stage's: the anchor used 400 of 700, the catalog upload
        # has no room for its 400.
        self.forget_fails()
        text = self.run_daily(want=6, FAKE_DAYS=D1, FAKE_TRANSIENT="catalog-anchor=1,catalog-upload=1",
                              ONYX_EARLY_RETRY_SECS="400", ONYX_EARLY_RETRY_BUDGET_SECS="700")
        self.assertEqual(self.sleeps(), [400])
        self.assertIn(f"{D1}: catalog-upload gave up after 1 of 5 tries (not_final)", text)
        # The wait gave up while the anchor was not final: the marker is cleared (RETRY-R1-M1).
        self.assertFalse((self.out / "done" / f"{D1}.anchored").exists())
        self.calls()
        # One try means no retry.
        self.run_daily(want=6, FAKE_DAYS=D1, FAKE_TRANSIENT="catalog-upload=999", ONYX_EARLY_RETRY_TRIES="1")
        self.assertEqual(len(self.calls("onyx-publish", "catalog-upload")), 1)
        self.assertEqual(self.sleeps(), [])
        # A pause never grows past an hour.
        self.run_daily(want=6, FAKE_DAYS=D1, FAKE_TRANSIENT="catalog-upload=999", ONYX_EARLY_RETRY_TRIES="4",
                       ONYX_EARLY_RETRY_SECS="2000", ONYX_EARLY_RETRY_BUDGET_SECS="6000")
        self.assertEqual(self.sleeps(), [2000, 3600])
        # The run after it goes on where it stopped: no second signature; the anchor is read again
        # (its marker was cleared), which sends nothing for a recorded report.
        self.calls()
        self.alerts()
        self.run_daily(FAKE_DAYS=D1)
        self.assertEqual([x["cmd"] for x in self.calls()],
                         ["build", "catalog-anchor", "catalog-upload", "upload", "upload"])
        self.assertEqual(self.alerts(), [])

    def test_no_try_waits_for_finality_longer_than_the_budget_left(self):
        # RETRY-R1-H2: with ONYX_EARLY_WAIT_SECS at an hour, five "not final" tries were 4.6 hours
        # of one run. Each try now gets the lower of the setting and what is left of the budget.
        self.run_daily(FAKE_DAYS=D1, FAKE_TRANSIENT="catalog-upload=2", ONYX_EARLY_WAIT_SECS="4000",
                       ONYX_EARLY_RETRY_BUDGET_SECS="1000")
        waits = [int(self.opt(x, "--wait-secs")) for x in self.calls("onyx-publish", "catalog-upload")]
        self.assertEqual(self.sleeps(), [300, 600])
        self.assertEqual(len(waits), 3)
        self.assertEqual(waits[0], 1000, "the first try: the whole budget, not the hour")
        # Never more than what the pauses (300, then 600) leave; the failed tries' own seconds
        # count too, so it may be a little less (no lower bound but the minute: no clock assert).
        self.assertTrue(60 <= waits[1] <= 700, waits)
        self.assertTrue(60 <= waits[2] <= 100, waits)
        # A setting below the budget is kept as it is.
        self.forget_fails()
        self.run_daily(FAKE_DAYS=f"{D1},{D2}", ONYX_EARLY_WAIT_SECS="900")
        self.assertEqual({self.opt(x, "--wait-secs") for x in self.calls("onyx-publish") if "--wait-secs" in x["args"]},
                         {"900"})

    def unit_numbers(self):
        """The template's start limit, restart gap and timeout, in seconds."""
        unit = (HERE.parent / "deploy" / "systemd" / "user" / "onyx-early-record.service").read_text()
        secs = {"h": 3600, "min": 60, "s": 1}

        def span(name):
            (v,) = re.findall(rf"^{name}=(\d+)(h|min|s)$", unit, re.M)
            return int(v[0]) * secs[v[1]]

        (burst,) = re.findall(r"^StartLimitBurst=(\d+)$", unit, re.M)
        return unit, span("StartLimitIntervalSec"), int(burst), span("RestartSec"), span("TimeoutStartSec")

    def test_a_day_that_keeps_failing_reaches_the_units_start_limit(self):
        # RETRY-R1-H2, RETRY-R2-M4. The unit as installed starts a failed run again after
        # RestartSec and ends failed for good at the (StartLimitBurst + 1)th start inside
        # StartLimitIntervalSec. A run of 4.6 hours never got there. The script now ends ANY run
        # after ONYX_EARLY_RUN_SECS (every tool runs under `timeout` with the time left, plus its
        # 30 s to be killed), so the proof needs no guess about how long a step takes: BURST
        # runs of the longest allowed length, each with its 30 s and a restart gap after it
        # (four gaps, the last one too), fit in 90 % of the interval.
        unit, interval, burst, restart, timeout = self.unit_numbers()
        script = SCRIPT.read_text()
        (lo, hi) = (int(x) for x in re.search(r'whole "\$run_secs" (\d+) (\d+) ', script).groups())
        (default,) = (int(x) for x in re.findall(r"ONYX_EARLY_RUN_SECS:-(\d+)\}", script))
        (kill,) = (int(x) for x in re.findall(r"timeout -k (\d+) ", script))
        self.assertTrue(lo <= default <= hi)
        self.assertLessEqual(burst * (hi + kill + restart), interval * 9 // 10,
                             f"{burst} runs of {hi} s and their gaps do not fit the unit's {interval} s")
        self.assertLess(hi + kill, timeout // 2, "a run ends by itself, never by the unit's timeout")
        # The budget of the last stages is inside the box.
        (budget_hi,) = (int(x) for x in re.findall(r'whole "\$retry_budget" \d+ (\d+) ', script))
        self.assertLess(budget_hi, hi)
        # The template names the refusal among the exits that are not started again; a passing
        # stop that was given up (6) and a run whose time was up (8) are, so the unit's limit is
        # what ends a day that keeps failing.
        (prevent,) = re.findall(r"^RestartPreventExitStatus=(.+)$", unit, re.M)
        self.assertEqual(prevent.split(), ["2", "3", "4", "5", "7"])
        self.assertIn("Restart=on-failure", unit)
        # And the bound is real: a run whose stage never passes pauses within the highest budget.
        self.run_daily(want=6, FAKE_DAYS=D1, FAKE_TRANSIENT="catalog-upload=999", ONYX_EARLY_RETRY_TRIES="20",
                       ONYX_EARLY_RETRY_BUDGET_SECS=str(budget_hi), ONYX_EARLY_WAIT_SECS="3600")
        self.assertLessEqual(sum(self.sleeps()), budget_hi)
        waits = [int(self.opt(x, "--wait-secs")) for x in self.calls("onyx-publish", "catalog-upload")]
        self.assertLessEqual(max(waits), 3600)
        self.assertTrue(all(a >= b for a, b in zip(waits, waits[1:])), waits)

    def test_every_tool_runs_inside_the_runs_time(self):
        # RETRY-R2-M4: nothing the run starts can outlast it. Each early-record, scan and
        # onyx-publish call is given, through `timeout`, what is left of ONYX_EARLY_RUN_SECS.
        self.run_daily(ONYX_EARLY_RUN_SECS="3000")
        given = self.timeouts()
        tools = self.calls()
        # One `timeout` a tool call, the two scans (one a day) and the question about exit 75.
        self.assertEqual(len(given), len(tools) + 2 + 1)
        self.assertEqual({name for _, name in given},
                         {"early-record:build", "early-record:verify", "env:-u", "onyx-publish:--help",
                          "onyx-publish:build", "onyx-publish:catalog-build", "onyx-publish:catalog-sign",
                          "onyx-publish:catalog-anchor", "onyx-publish:catalog-upload", "onyx-publish:upload"})
        secs = [x for x, _ in given]
        self.assertTrue(all(0 < x <= 3000 for x in secs), secs)
        self.assertEqual(secs, sorted(secs, reverse=True), "what is left only goes down")
        # The pauses of a retry are inside the box too: with 900 s for the run, the second
        # pause (600 s after 300 s) has no room, and that is exit 8, not a longer run.
        self.forget_fails()
        text = self.run_daily(want=8, FAKE_DAYS=f"{D1},{D2},{D3}", FAKE_TRANSIENT="catalog-anchor=999",
                              ONYX_EARLY_RUN_SECS="900", ONYX_EARLY_RETRY_BUDGET_SECS="6000")
        # (The stub pause takes no time: 300 and 600 fit one by one, the third, 1200 s, cannot.)
        self.assertEqual(self.sleeps(), [300, 600])
        self.assertIn("no time for the next pause of 1200 s", text)

    def test_a_run_whose_time_is_up_stops_with_exit_8_and_goes_on_next_time(self):
        # RETRY-R2-M4: the time can be up at any step, also before the last stages (a slow build,
        # a verify that hangs, a backlog). The stub `timeout` says so for one step at a time.
        steps = ("early-record:build", "env:-u", "onyx-publish:build", "early-record:verify",
                 "onyx-publish:catalog-build", "onyx-publish:catalog-sign", "onyx-publish:catalog-anchor",
                 "onyx-publish:catalog-upload", "onyx-publish:upload")
        for step in steps:
            self.alerts()
            text = self.run_daily(want=8, FAKE_DAYS=D1, FAKE_TIMEUP=step)
            self.assertIn("the run's time is up", text, step)
            self.assertIn("run again: it goes on where it stopped", text, step)
            (alert,) = self.alerts()
            self.assertEqual(alert[:5], ["-t", "onyx-early-record-alert", "-p", "user.err", "--"])
            self.assertTrue(alert[5].startswith("onyx-early-record exit 8: the run's time is up"), alert)
            self.assertEqual(self.sleeps(), [], step)
            self.assertFalse((self.out / "done" / f"{D1}.published").exists(), step)
            # A signature cut off half-way is not kept (it would be skipped, and refused, for good).
            self.assertFalse((self.out / "catalogs" / f"{D1}.bin.report").exists()
                             and step == "onyx-publish:catalog-sign")
        # The run after them finishes the day; each step ran to its end exactly once.
        self.calls()
        self.run_daily(FAKE_DAYS=D1)
        self.assertTrue((self.out / "done" / f"{D1}.published").exists())
        self.assertEqual([x["cmd"] for x in self.calls("onyx-publish")], ["upload", "upload"])
        self.assertEqual(self.alerts(), [])

    def test_every_exit_but_0_logs_the_alert_line(self):
        # RETRY-R2-M4: not only the last stages' 6 and 7. One line each, with the exit code.
        with open(self.out / "lock", "a") as held:
            fcntl.flock(held, fcntl.LOCK_EX)
            self.run_daily("--dry-run", want=5)
        runs = (
            (1, ("--dry-run",), {"FAKE_BUILD_FAIL": "1"}),
            (2, ("--dry-run",), {"ONYX_EARLY_PUBLISHER": "Not An Id"}),
            (3, (), {"FAKE_VERIFY_DIFF": D2}),
            (6, (), {"FAKE_TRANSIENT": "catalog-upload=999", "ONYX_EARLY_RETRY_TRIES": "1"}),
            (7, (), {"FAKE_CATUP_FAIL": D2}),
            (8, (), {"FAKE_TIMEUP": "onyx-publish:upload"}),
            (4, ("--dry-run",), {"FAKE_DAYS": f"{D1},{D2},{D3}", "FAKE_PLANT_DAYS": D3}),
        )
        self.assertEqual([a[5].split(":")[0] for a in self.alerts()], ["onyx-early-record exit 5"])
        for code, args, env in runs:
            self.run_daily(*args, want=code, **env)
            got = self.alerts()
            self.assertEqual([a[:5] for a in got], [["-t", "onyx-early-record-alert", "-p", "user.err", "--"]], code)
            self.assertTrue(got[0][5].startswith(f"onyx-early-record exit {code}: "), got)

    def test_a_refusal_is_never_tried_again(self):
        refusals = (
            {},  # the book would reject the report (the tool's exit 1, no TRANSIENT line)
            # A gas key without money, whatever else the line says (also from an older tool).
            {"FAKE_ANCHOR_SAYS": "onyx-publish catalog-anchor: postReport: rpc: eth_estimateGas: node error: "
                                 "insufficient funds for gas * price + value: node answered HTTP 503",
             "FAKE_OLD": "1"},
            {"FAKE_ANCHOR_SAYS": "onyx-publish catalog-anchor: the node at --rpc is not chain 84532"},
            {"FAKE_ANCHOR_SAYS": "onyx-publish catalog-anchor: eth_call: node answered HTTP 401", "FAKE_OLD": "1"},
            # A text that only looks like a passing stop earlier in the log, not on the last line.
            {"FAKE_ANCHOR_SAYS": "transaction 0xab not mined before the deadline\nonyx-publish catalog-anchor: "
                                 "this key already anchored another catalog 0 (sequence 1)", "FAKE_OLD": "1"},
            # A stuck transaction the tool gave up on, or one it did not send (RETRY-R1-H1).
            {"FAKE_ANCHOR_SAYS": "onyx-publish catalog-anchor: an unmined transaction of the gas key 0xab holds nonce 7 "
                                 "and this tool did not send it (seen for 1800 s)"},
        )
        for env in refusals:
            # The day is built, scanned and verified by the first run; every run stops at its anchor.
            text = self.run_daily(want=7, FAKE_DAYS=D1, FAKE_ANCHOR_FAIL=D1, **env)
            self.assertEqual(len(self.calls("onyx-publish", "catalog-anchor")), 1, env)
            self.assertEqual(self.sleeps(), [], env)
            self.assertIn(f"{D1}: catalog-anchor refused at try 1 (onyx-publish exit 1): not tried again", text)
            self.assertIn(env.get("FAKE_ANCHOR_SAYS", "would revert, not sent").splitlines()[-1], text,
                          "the tool's own line is kept")
            self.assertFalse((self.out / "done" / f"{D1}.anchored").exists())
            # Exit 7 is loud by itself too (RETRY-R1-H2).
            (alert,) = self.alerts()
            self.assertEqual(alert[1], "onyx-early-record-alert")
            self.assertTrue(alert[5].startswith(f"onyx-early-record exit 7: {D1}: catalog-anchor refused at try 1"))
        # An exit code that is neither 75 nor 1 (a crash, a usage error) is not a passing stop.
        for code in ("2", "101"):
            self.run_daily(want=7, FAKE_DAYS=D1, FAKE_ANCHOR_EXIT=code)
            self.assertEqual(self.sleeps(), [])
        self.alerts()
        # A tool that was killed (128 + the signal) did not answer at all: not "refused", and
        # the run after it goes on (RETRY-R1-L3).
        for code in ("137", "143"):
            text = self.run_daily(want=1, FAKE_DAYS=D1, FAKE_ANCHOR_EXIT=code)
            self.assertIn(f"{D1}: catalog-anchor: onyx-publish was killed (exit {code}, signal {int(code) - 128})", text)
            self.assertNotIn("refused", text)
            self.assertEqual(self.sleeps(), [])
        self.assertEqual([a[5][:25] for a in self.alerts()], ["onyx-early-record exit 1:"] * 2)
        # A refusal after a passing stop ends the tries at once too.
        self.calls()
        text = self.run_daily(want=7, FAKE_DAYS=D1, FAKE_TRANSIENT="catalog-anchor=1r")
        self.assertEqual(len(self.calls("onyx-publish", "catalog-anchor")), 2)
        self.assertEqual(self.sleeps(), [300])
        self.assertIn(f"{D1}: catalog-anchor refused at try 2 (onyx-publish exit 1)", text)

    def test_exit_75_survives_a_log_copy_that_fails(self):
        # RETRY-R1-L3: the tool's stderr goes through `tee` into OUT/stage.err. With pipefail a
        # tee that fails would give the pipe its own exit code and a passing stop would look like
        # a refusal. The tool's own code is read (the pipe's first place).
        (self.bin / "tee").write_text("#!/bin/sh\ncat >&2\nexit 1\n")
        (self.bin / "tee").chmod(0o755)
        text = self.run_daily(want=6, FAKE_DAYS=D1, FAKE_TRANSIENT="catalog-anchor=999", ONYX_EARLY_RETRY_TRIES="2")
        self.assertIn(f"{D1}: catalog-anchor try 2 of 2 after 300 s: transient", text)
        self.assertNotIn("refused", text)

    def test_retry_settings_are_whole_numbers_in_range(self):
        bad = {
            "ONYX_EARLY_RETRY_TRIES": ("0", "21", "five", "-1", "1.5", "05", " 5", "5 ", "1e1", "999999"),
            "ONYX_EARLY_RETRY_SECS": ("0", "3601", "x", "-300", "0300", "300s"),
            # Above 6000 a failing day would not reach the unit's start limit.
            "ONYX_EARLY_RETRY_BUDGET_SECS": ("0", "599", "6001", "18000", "-1", "1h", "5400.0"),
            # Above 8400 four runs and their gaps would not fit the unit's 12 hours.
            "ONYX_EARLY_RUN_SECS": ("0", "599", "8401", "21600", "2h", "-7200", "07200"),
        }
        for name, values in bad.items():
            for v in values:
                text = self.run_daily("--dry-run", want=2, **{name: v})
                self.assertIn(f"{name} is a whole number from", text, v)
        self.assertEqual(self.calls(), [], "nothing runs with a bad setting")
        self.run_daily("--dry-run", FAKE_DAYS=D1, ONYX_EARLY_RETRY_TRIES="20", ONYX_EARLY_RETRY_SECS="3600",
                       ONYX_EARLY_RETRY_BUDGET_SECS="6000", ONYX_EARLY_RUN_SECS="8400")

    def test_a_left_over_staged_folder_is_taken_up(self):
        # The run stops after early-record wrote the folder and its state, before the rename.
        self.run_daily("--dry-run", want=1, FAKE_CRASH_AFTER=D1)
        staged = self.out / "versions" / "next"
        self.assertTrue((staged / "day.json").exists())
        self.calls()
        self.run_daily("--dry-run")
        builds = [x for x in self.calls() if x["tool"] == "early-record" and x["cmd"] == "build"]
        self.assertEqual(self.opt(builds[0], "--prev"), None, "the staged day is taken up without --prev")
        self.assertEqual(self.opt(builds[1], "--prev"), str(self.out / "versions" / D1))
        self.assertFalse(staged.exists())
        self.assertEqual(sorted(p.name for p in (self.out / "versions").iterdir()), [D1, D2])

    def test_a_relative_out_is_scanned_and_a_lost_out_stops(self):
        rel = self.t / "here"
        (rel / "o").mkdir(parents=True)
        self.run_daily("--dry-run", cwd=str(rel), ONYX_EARLY_OUT="o", ONYX_EARLY_STATE="s")
        # The scan's notes (e.g. a label set the public tree leaves out) may come first.
        scanned = next(
            x for x in (rel / "o" / "scan.log").read_text().splitlines() if x.startswith("files scanned")
        )
        self.assertRegex(scanned, r"^files scanned: \{'/[^']+/o/versions/" + D2 + r"': [1-9]")
        # The output root lost while the state keeps its days: a new version 1 would follow.
        for p in sorted((rel / "o").rglob("*"), reverse=True):
            p.unlink() if p.is_file() else p.rmdir()
        self.calls()
        self.run_daily("--dry-run", want=2, cwd=str(rel), ONYX_EARLY_OUT="o", ONYX_EARLY_STATE="s")
        self.assertFalse(self.calls("early-record", "build"))

    def test_the_scan_never_passes_nothing(self):
        scan = [sys.executable, "-I", str(HERE / "audit-privacy-scan.py"), str(HERE.parent), str(self.idx)]
        env = dict(os.environ, FAIL_ON_HIT="1")
        run = lambda target: subprocess.run(scan + [str(target)], env=env, capture_output=True, text=True)
        self.assertEqual(run(self.t / "missing").returncode, 1)
        (self.t / "empty").mkdir()
        self.assertEqual(run(self.t / "empty").returncode, 1)
        (self.t / "clean").mkdir()
        (self.t / "clean" / "data.csv").write_text("key,kind\n1,day_close\n")
        r = run(self.t / "clean")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_a_verify_that_compared_no_root_is_not_trusted(self):
        self.run_daily("--dry-run", want=1, FAKE_NO_ROOTS=D1)
        self.assertFalse((self.out / "done" / f"{D1}.verified").exists())

    def test_a_privacy_hit_stops_publication(self):
        self.run_daily("--dry-run", want=4, FAKE_PLANT_DAYS=D1)
        c = self.calls()
        self.assertFalse([x for x in c if x["tool"] == "onyx-publish"], "nothing built for a folder with a hit")

    def test_a_held_lock_and_a_failed_build(self):
        with open(self.out / "lock", "a") as held:
            fcntl.flock(held, fcntl.LOCK_EX)
            self.run_daily("--dry-run", want=5)
        self.run_daily("--dry-run", want=1, FAKE_BUILD_FAIL="1")
        self.assertFalse((self.out / "versions" / D1).exists())

if __name__ == "__main__":
    # scripts/early-record-anvil.sh runs the real tools through the dry run on its chain with this root.
    if sys.argv[1:2] == ["--stand-in-indexer"]:
        root = Path(sys.argv[2])
        root.mkdir(parents=True)
        print(indexer(root))
        sys.exit(0)
    unittest.main(verbosity=1)
