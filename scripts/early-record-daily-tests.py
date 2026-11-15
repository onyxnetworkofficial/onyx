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
# Run: python3 -I scripts/early-record-daily-tests.py   (check-all runs it; about 5 s)
import fcntl
import json
import os
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
with open(os.path.join(os.environ["FAKE_CALLS"], "calls.jsonl"), "a") as f:
    f.write(json.dumps({"tool": "onyx-publish", "cmd": cmd, "args": a}) + "\n")
day = lambda p: os.path.basename(p).split(".")[0]
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
elif cmd == "catalog-sign":
    open(o["--catalog"] + ".report", "w").write("report\n")
    print("SIGNED catalog")
elif cmd == "catalog-anchor":
    if day(o["--catalog"]) in os.environ.get("FAKE_ANCHOR_FAIL", ""):
        sys.exit("onyx-publish catalog-anchor: postReport: would revert")
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
        # The key file is unreadable: a script that read it would fail (a stub never opens it).
        self.key = self.t / "record.key"
        self.key.write_text("not a key\n")
        self.key.chmod(0)
        self.env = {
            "PATH": f"{self.bin}:{os.environ.get('PATH', '/usr/bin:/bin')}", "HOME": str(self.t),
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
        for secret in (RPC1, RPC2, "secret-one", "secret-two", str(self.key)):
            self.assertNotIn(secret, text, "a node URL or the key path was printed")
        return text

    def calls(self, tool=None, cmd=None):
        p = self.calldir / "calls.jsonl"
        lines = [json.loads(x) for x in p.read_text().splitlines()] if p.exists() else []
        p.unlink(missing_ok=True)
        return [c for c in lines if (tool is None or c["tool"] == tool) and (cmd is None or c["cmd"] == cmd)]

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
        self.run_daily(want=1, FAKE_DAYS=f"{D1},{D2},{D3}", FAKE_ANCHOR_FAIL=D3)
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
        self.run_daily(want=1, FAKE_UPLOAD_FAIL="exclusions")
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
        self.run_daily(want=1, FAKE_DAYS=f"{D1},{D2},{D3}", FAKE_CATUP_FAIL=D3)
        self.calls()
        self.run_daily(FAKE_DAYS=f"{D1},{D2},{D3}")
        self.assertEqual([x["cmd"] for x in self.calls()], ["build", "catalog-upload", "upload"])

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
