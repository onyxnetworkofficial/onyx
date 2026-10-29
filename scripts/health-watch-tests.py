#!/usr/bin/env python3
# Unit tests for scripts/health-watch.py (ROADMAP L12) against a fake onyx-node, JSON-RPC node and
# website on 127.0.0.1 (no outside network). Every threshold in deploy/health-watch/thresholds.json
# is tested at its edge (at the limit: ok; one past: alert), every malformed answer must alert,
# never pass, and RPC down, a stalled RPC and a wrong chain id must alert.
# Time (ROADMAP E12): every test runs on a virtual clock. The fake server never sleeps; a test's
# delays are spent on that clock by a transport in front of it (VirtualNet), so the deadline and
# timeout tests assert exact budgets and hold on a loaded machine. Real sockets still answer every
# request, and test_node_timeout_counts_as_down runs a real socket timeout against a server that
# never answers until the test is over.
# Run: python3 -I scripts/health-watch-tests.py   (check-all runs it; about 10 s)
import copy
import importlib.util
import io
import json
import os
import re
import subprocess
import sys
import tempfile
import threading
import time
import types
import unittest
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
SCRIPT = HERE / "health-watch.py"
spec = importlib.util.spec_from_file_location("health_watch", SCRIPT)
hw = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hw)

NOW = 1_800_000_000
DEPLOYER = "0x80c91f45b8353ff5c2b1e4c127a2a21746b37972"
OPERATOR = "0xf1b27c3f48e9972eb8050e6dff8594e05e3e488c"
REVENUE = "0xa32d520380362378d22a7a51993678c7fbbfe44d"
USDC = "0x036cbd53842c5426634e7929541ec2318f3dcf7e"
# The real socket timeout behind the virtual transport: a cap on a broken fake server only. A
# healthy answer from 127.0.0.1 never comes near it, however loaded the machine.
REAL_TIMEOUT_S = 120


def w(n):
    return "0x" + format(n, "064x")


def healthy_node():
    return {
        "status": "ok", "chainId": 84532, "payments": "eip155:84532", "acceptingPayments": True,
        "receiptsPending": 0, "receiptsDropped": 0, "receiptWriteErrors": 0, "pendingSettleWriteErrors": 0,
        "settles": {"intentsOpen": 0, "intentWriteErrors": 0, "unknownOutcomes": 0, "clockSkewSeconds": 3},
        "reporter": {"posts": 1, "stuckSince": None, "writeErrors": 0, "lastError": None},
        "indexer": {"nextBlock": 1001, "finalizedHead": 1000, "conflicts": 0, "candidatesDropped": 0,
                    "candidatesEvicted": 0, "stateWriteErrors": 0, "failing": False},
        "level3": healthy_level3(),
    }


def healthy_level3():
    # crates/onyx-api/src/signed_serve.rs fn health_of (ROADMAP C62).
    return {"breakerSecs": 600, "hostSkewSecs": 300, "now": NOW, "logReadable": True, "logError": None,
            "posts": 3, "postsRecorded": 3, "postsPending": 0, "postsRefused": 0, "postsRefusedRetrying": 0,
            "postsRefusedFinal": 0,
            "oldestPendingSince": None, "oldestPendingSecs": None, "stopped": None}


class Fake:
    """Mutable world the fake server answers from."""

    def __init__(self):
        self.health = healthy_node()
        self.health_raw = None  # bytes to send instead of the JSON
        self.health_status = 200
        self.chain = 84532
        self.head = 1200
        self.head_ts = NOW - 2
        self.final = 1000
        self.eth = 2 * 10**15
        self.eth_of = {}  # address -> wei, overriding self.eth
        self.token = int(USDC, 16)
        self.allocated, self.paid, self.held = 7500, 1250, 6250
        self.rpc_raw = None  # callable(request dict) -> bytes, overrides everything
        self.rpc_slow_odd = 0.0  # every odd-numbered RPC request takes this long (first try of each call)
        self.rpc_n = 0
        self.rpc_status = 200
        self.delay = {}  # "rpc" | "node" | "site" -> virtual seconds an answer takes (VirtualNet)
        self.hang = set()  # "node": the real server holds the answer until `release` is set
        self.release = threading.Event()
        self.site_status = 200
        self.demo = {"public": True, "runs": [{"input": None}, {"input": None}]}
        self.demo_raw = None

    def rpc(self, req):
        m, p = req.get("method"), req.get("params")
        if m == "eth_chainId":
            return hex(self.chain)
        if m == "eth_getBlockByNumber":
            if p[0] == "latest":
                return {"number": hex(self.head), "timestamp": hex(self.head_ts)}
            return {"number": hex(self.final), "timestamp": hex(self.head_ts - 1000)}
        if m == "eth_getBalance":
            return hex(self.eth_of.get(p[0].lower(), self.eth))
        if m == "eth_call":
            to, data = p[0]["to"].lower(), p[0]["data"]
            if to == REVENUE and data == hw.SEL_TOKEN:
                return w(self.token)
            if to == REVENUE and data == hw.SEL_ALLOCATED:
                return w(self.allocated)
            if to == REVENUE and data == hw.SEL_PAID_OUT:
                return w(self.paid)
            if to == USDC and data == hw.SEL_BALANCE_OF + "0" * 24 + REVENUE[2:]:
                return w(self.held)
            return "0x"
        raise KeyError(m)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def send(self, status, body, ctype="application/json"):
        self.send_response(status)
        self.send_header("content-type", ctype)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        f = self.server.fake
        if self.path == "/node/v1/health":
            if "node" in f.hang:
                f.release.wait(REAL_TIMEOUT_S)
            body = f.health_raw if f.health_raw is not None else json.dumps(f.health).encode()
            return self.send(f.health_status, body)
        if self.path == "/site/":
            return self.send(f.site_status, b"<!doctype html>", "text/html")
        if self.path == "/site/data/demo.json":
            body = f.demo_raw if f.demo_raw is not None else json.dumps(f.demo).encode()
            return self.send(200, body)
        self.send(404, b"{}")

    def do_POST(self):
        f = self.server.fake
        req = json.loads(self.rfile.read(int(self.headers["content-length"])))
        if f.rpc_raw is not None:
            return self.send(200, f.rpc_raw(req))
        if f.rpc_status != 200:
            return self.send(f.rpc_status, b"rate limited", "text/plain")
        self.send(200, json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": f.rpc(req)}).encode())


class Server(ThreadingHTTPServer):
    daemon_threads = True

    def handle_error(self, request, client_address):
        pass  # a client that timed out on a stalled answer closes first (broken pipe): expected


class VirtualClock:
    """health-watch's clock in the tests: it moves only when a wait is spent on it."""

    def __init__(self):
        self.t = 1000.0

    def now(self):
        return self.t

    def sleep(self, s):
        self.t += max(0.0, s)


class VirtualNet:
    """health-watch's transport in the tests. A request whose fake delay is above the timeout it
    was given spends that timeout on the clock and times out, as a real socket would; any other
    spends its delay and is sent to the real fake server."""

    def __init__(self, fake, clock):
        self.fake, self.clock = fake, clock

    def urlopen(self, req, timeout):
        f, path = self.fake, urllib.parse.urlsplit(req.full_url).path
        kind = "rpc" if path.endswith("/rpc") else "node" if path == "/node/v1/health" else "site" if path == "/site/" else None
        delay = f.delay.get(kind, 0.0)
        if kind == "rpc":
            f.rpc_n += 1
            if f.rpc_slow_odd and f.rpc_n % 2 == 1:
                delay += f.rpc_slow_odd
        if timeout is not None and delay > timeout:
            self.clock.sleep(timeout)
            raise TimeoutError("timed out")
        self.clock.sleep(delay)
        return urllib.request.urlopen(req, timeout=REAL_TIMEOUT_S)


class Base(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = Server(("127.0.0.1", 0), Handler)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.url = f"http://127.0.0.1:{cls.server.server_address[1]}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        self.fake = Fake()
        self.server.fake = self.fake
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.cfg = hw.load_config([])
        self.cfg["rpc"]["timeout_s"] = 0.5
        self.cfg["node"]["timeout_s"] = 0.5
        self.cfg["site"]["timeout_s"] = 0.5
        self.opts = types.SimpleNamespace(
            node=self.url + "/node", rpc=self.url + "/rpc", site=self.url + "/site",
            state=os.path.join(self.tmp.name, "state.json"),
            alert_log=os.path.join(self.tmp.name, "alerts.jsonl"), notify_cmd=None, backoff=0)
        self.now = NOW
        self.clock = VirtualClock()
        self.net = VirtualNet(self.fake, self.clock)
        self.swap(_clock=self.clock.now, _sleep=self.clock.sleep, _urlopen=self.net.urlopen)
        self.addCleanup(self.fake.release.set)

    def swap(self, **seams):
        for name, fn in seams.items():
            self.addCleanup(setattr, hw, name, getattr(hw, name))
            setattr(hw, name, fn)

    def real_net(self):
        """This test runs on the real clock and real socket timeouts."""
        self.swap(_clock=time.monotonic, _sleep=time.sleep,
                  _urlopen=lambda req, timeout: urllib.request.urlopen(req, timeout=timeout))

    def run_watch(self, now=None):
        buf = io.StringIO()
        code = hw.run_once(self.cfg, self.opts, now=self.now if now is None else now, out=buf)
        lines = [json.loads(x) for x in buf.getvalue().splitlines()]
        for x in lines:  # one JSON object per line, every field present
            self.assertEqual(set(x), {"ts", "check", "status", "value", "limit", "detail", "notify"})
        by = {x["check"]: x for x in lines}
        self.assertEqual(len(by), len(lines), "a check printed twice")
        return code, by

    def status(self, check, now=None):
        code, by = self.run_watch(now)
        self.assertIn(check, by, f"{check} not printed: {sorted(by)}")
        return code, by[check]["status"], by

    def assert_alert(self, check, now=None):
        code, st, by = self.status(check, now)
        self.assertEqual(st, "alert", by[check])
        self.assertEqual(code, 1)
        return by[check]

    def assert_ok(self, check, now=None):
        code, st, by = self.status(check, now)
        self.assertEqual(st, "ok", by[check])
        bad = [k for k, v in by.items() if v["status"] == "alert"]
        self.assertEqual(code, 1 if bad else 0, bad)
        return by[check]

    def fresh(self):
        """Forget state (debounce counters and memories) between edge runs."""
        if os.path.exists(self.opts.state):
            os.remove(self.opts.state)

    def edge(self, check, setter, limit):
        """At the limit: ok. One past: alert."""
        self.fresh()
        setter(limit)
        self.assert_ok(check)
        self.fresh()
        setter(limit + 1)
        self.assert_alert(check)

    def alerts(self):
        try:
            with open(self.opts.alert_log) as f:
                return [json.loads(x) for x in f]
        except FileNotFoundError:
            return []


class AllGreen(Base):
    def test_everything_ok_exit_zero_no_notification(self):
        code, by = self.run_watch()
        self.assertEqual(code, 0, {k: v for k, v in by.items() if v["status"] != "ok"})
        for check in ["node.up", "node.health", "node.chain", "node.payments", "node.indexer", "node.indexer.lag",
                      "node.indexer.conflicts", "node.indexer.candidates_dropped", "node.indexer.candidates_evicted",
                      "node.indexer.failing", "node.intents.open", "node.intents.open_for",
                      "node.intents.unknown_outcomes", "node.receipts_dropped", "node.clock_skew",
                      "node.reporter.stuck", "node.reporter.last_error", "node.level3", "node.level3.stopped",
                      "node.level3.breaker", "node.level3.pending_for", "node.level3.pending",
                      "node.level3.refused_new", "node.level3.retrying", "rpc.up", "rpc.chain", "rpc.head_age",
                      "rpc.finalized_lag", "balances.deployer_eth", "balances.operator_eth", "balances.revenue", "site.up",
                      "site.public", *[c for c, _ in hw.WRITE_ERRORS]]:
            self.assertEqual(by[check]["status"], "ok", by[check])
        self.assertEqual(self.alerts(), [])
        self.assertEqual(set(by) - hw.CHECKS, set(), "printed a check the config cannot name")


class NodeThresholds(Base):
    def test_indexer_lag_edge(self):
        lim = self.cfg["node"]["indexer_lag_max_blocks"]
        base = 5 * lim  # well above the limit, so the cursor never goes negative
        self.fake.final = base
        def s(v):
            self.fake.health["indexer"]["finalizedHead"] = base
            self.fake.health["indexer"]["nextBlock"] = base - v + 1
        self.edge("node.indexer.lag", s, lim)

    def test_indexer_lag_counts_the_rpc_finalized_head_when_the_node_is_stuck(self):
        lim = self.cfg["node"]["indexer_lag_max_blocks"]
        # The node's own finalizedHead stopped moving with its cursor; the watch's RPC moved on.
        self.fake.health["indexer"].update(finalizedHead=1000, nextBlock=1001)
        self.fake.head, self.fake.final = 1000 + lim + 1500, 1000 + lim
        self.assert_ok("node.indexer.lag")
        self.fresh()
        self.fake.final = 1000 + lim + 1
        line = self.assert_alert("node.indexer.lag")
        self.assertEqual(line["value"], lim + 1)

    def test_counters_edges(self):
        for check, key, limit in (("node.indexer.conflicts", "conflicts", "conflicts_max"),
                                  ("node.indexer.candidates_dropped", "candidatesDropped", "candidates_dropped_max"),
                                  ("node.indexer.candidates_evicted", "candidatesEvicted", "candidates_evicted_max")):
            with self.subTest(check):
                self.fake.health = healthy_node()
                self.edge(check, lambda v, k=key: self.fake.health["indexer"].__setitem__(k, v), self.cfg["node"][limit])

    def test_every_write_error_counter_edge(self):
        for check, path in hw.WRITE_ERRORS:
            with self.subTest(check):
                self.fake.health = healthy_node()
                def s(v, p=path):
                    d = self.fake.health
                    for k in p[:-1]:
                        d = d[k]
                    d[p[-1]] = v
                self.edge(check, s, self.cfg["node"]["write_errors_max"])

    def test_receipts_dropped_edge(self):
        self.edge("node.receipts_dropped", lambda v: self.fake.health.__setitem__("receiptsDropped", v),
                  self.cfg["node"]["receipts_dropped_max"])

    def test_intents_open_edge(self):
        self.edge("node.intents.open", lambda v: self.fake.health["settles"].__setitem__("intentsOpen", v),
                  self.cfg["node"]["intents_open_max"])

    def test_intents_open_for_edge_and_reset(self):
        lim = self.cfg["node"]["intents_open_for_max_s"]
        self.fake.health["settles"]["intentsOpen"] = 2
        self.assert_ok("node.intents.open_for", now=NOW)
        self.assert_ok("node.intents.open_for", now=NOW + lim)
        line = self.assert_alert("node.intents.open_for", now=NOW + lim + 1)
        self.assertEqual(line["value"], lim + 1)
        # One run with nothing open resets the clock.
        self.fake.health["settles"]["intentsOpen"] = 0
        self.assert_ok("node.intents.open_for", now=NOW + lim + 2)
        self.fake.health["settles"]["intentsOpen"] = 1
        self.assert_ok("node.intents.open_for", now=NOW + 2 * lim)

    def test_unknown_outcomes_new_since_last_run(self):
        lim = self.cfg["node"]["unknown_outcomes_new_max"]
        self.fake.health["settles"]["unknownOutcomes"] = 7
        self.assert_ok("node.intents.unknown_outcomes")  # first run: baseline
        self.fake.health["settles"]["unknownOutcomes"] = 7 + lim
        self.assert_ok("node.intents.unknown_outcomes")
        self.fake.health["settles"]["unknownOutcomes"] = 7 + lim + lim + 1
        self.assert_alert("node.intents.unknown_outcomes")
        self.assert_ok("node.intents.unknown_outcomes")  # no new one
        # A restart resets the counter: a lower value counts from zero.
        self.fake.health["settles"]["unknownOutcomes"] = lim + 1
        self.assert_alert("node.intents.unknown_outcomes")

    def test_clock_skew_edges_both_signs_and_unknown(self):
        lim = self.cfg["node"]["clock_skew_max_s"]
        self.edge("node.clock_skew", lambda v: self.fake.health["settles"].__setitem__("clockSkewSeconds", v), lim)
        self.edge("node.clock_skew", lambda v: self.fake.health["settles"].__setitem__("clockSkewSeconds", -v), lim)
        self.fresh()
        self.fake.health["settles"]["clockSkewSeconds"] = None
        self.assert_ok("node.clock_skew")

    def test_reporter_stuck_edge(self):
        lim = self.cfg["node"]["reporter_stuck_max_s"]
        self.edge("node.reporter.stuck", lambda v: self.fake.health["reporter"].__setitem__("stuckSince", NOW - v), lim)

    def test_reporter_absent_is_ok(self):
        self.fake.health["reporter"] = None
        self.assert_ok("node.reporter.stuck")
        self.assert_ok("node.reporter.write_errors")

    def test_reporter_last_error_alerts_on_the_configured_run(self):
        n = self.cfg["after"]["node.reporter.last_error"]
        self.fake.health["reporter"]["lastError"] = "credit reverted"
        for _ in range(n - 1):
            self.assertEqual(self.status("node.reporter.last_error")[1], "warn")
        self.assert_alert("node.reporter.last_error")

    def test_indexer_failing_alerts_on_the_configured_run_both_modes(self):
        n = self.cfg["after"]["node.indexer.failing"]
        for mode in ({"failing": True}, {"lastError": "429 too many requests"}):
            with self.subTest(mode):
                self.fresh()
                self.fake.health = healthy_node()
                self.fake.health["indexer"].pop("failing")
                self.fake.health["indexer"].update(mode)
                for _ in range(n - 1):
                    code, st, _ = self.status("node.indexer.failing")
                    self.assertEqual((st, code), ("warn", 0))
                self.assert_alert("node.indexer.failing")
        self.fake.health["indexer"]["lastError"] = None
        self.assert_ok("node.indexer.failing")

    def test_indexer_off_alerts_when_required(self):
        self.fake.health["indexer"] = None
        self.assert_alert("node.indexer")
        self.cfg["node"]["indexer_required"] = False
        code, st, _ = self.status("node.indexer")
        self.assertEqual(st, "skip")

    def test_indexer_off_is_one_alert_not_two(self):
        # NL20-2: an indexer that is off used to alert twice, the second as "malformed".
        self.fake.health["indexer"] = None
        self.assert_alert("node.indexer")
        self.assert_ok("node.indexer.state_write_errors")

    def test_payments_refused(self):
        self.fake.health["acceptingPayments"] = False
        self.assert_alert("node.payments")
        self.fake.health["payments"] = None  # payments off: nothing to accept
        self.assert_ok("node.payments")

    def test_node_on_wrong_chain(self):
        self.fake.health["chainId"] = 31337
        self.assert_alert("node.chain")

    def test_status_not_ok(self):
        self.fake.health["status"] = "degraded"
        self.assert_alert("node.health")


class NodeDownAndMalformed(Base):
    def test_node_down_alerts_on_third_failure_then_resolves_once(self):
        self.opts.node = "http://127.0.0.1:1/node"  # nothing listens on port 1
        n = self.cfg["after"]["node.up"]
        self.assertEqual(n, 3, "launch checklist section 3: alert after 3 failures")
        for i in range(n - 1):
            code, st, by = self.status("node.up")
            self.assertEqual((st, code), ("warn", 0))
            self.assertNotIn("node.indexer.lag", by, "sub-checks of an unreachable node are not printed as ok")
        self.assert_alert("node.up")
        self.assertEqual([a["check"] for a in self.alerts()], ["node.up"])
        self.opts.node = self.url + "/node"
        code, by = self.run_watch()
        self.assertEqual((by["node.up"]["status"], by["node.up"]["notify"]), ("ok", "resolved"))
        code, by = self.run_watch()
        self.assertIsNone(by["node.up"]["notify"])
        self.assertEqual([a["event"] for a in self.alerts()], ["alert", "resolved"])

    def test_node_http_500_counts_as_down(self):
        self.fake.health_status = 500
        self.cfg["after"]["node.up"] = 1
        self.assertIn("HTTP 500", self.assert_alert("node.up")["detail"])

    def test_node_timeout_counts_as_down(self):
        # A real socket timeout: the server holds its answer until the test ends, so the 0.5 s
        # timeout fires however slowly this machine runs.
        self.real_net()
        self.fake.hang.add("node")
        self.cfg["after"]["node.up"] = 1
        self.assertIn("timed out", self.assert_alert("node.up")["detail"])
        self.fake.hang.clear()
        self.fake.delay["node"] = 1.0  # the same on the virtual transport
        self.swap(_clock=self.clock.now, _sleep=self.clock.sleep, _urlopen=self.net.urlopen)
        self.assertIn("timed out", self.assert_alert("node.up")["detail"])

    def test_malformed_health_always_alerts(self):
        cases = {
            "not json": (b"<html>oops", "node.health"),
            "json array": (b"[1,2]", "node.health"),
            "empty object": (b"{}", "node.health"),
        }
        for name, (raw, check) in cases.items():
            with self.subTest(name):
                self.fresh()
                self.fake.health_raw = raw
                self.assert_alert(check)
        self.fake.health_raw = None
        mutations = {
            "settles missing": (lambda h: h.pop("settles"), "node.intents.open"),
            "intentsOpen a string": (lambda h: h["settles"].__setitem__("intentsOpen", "3"), "node.intents.open"),
            "intentsOpen a bool": (lambda h: h["settles"].__setitem__("intentsOpen", True), "node.intents.open"),
            "intentsOpen negative": (lambda h: h["settles"].__setitem__("intentsOpen", -1), "node.intents.open"),
            "unknownOutcomes null": (lambda h: h["settles"].__setitem__("unknownOutcomes", None), "node.intents.unknown_outcomes"),
            "skew a float": (lambda h: h["settles"].__setitem__("clockSkewSeconds", 1.5), "node.clock_skew"),
            "nextBlock missing": (lambda h: h["indexer"].pop("nextBlock"), "node.indexer.lag"),
            "conflicts null": (lambda h: h["indexer"].__setitem__("conflicts", None), "node.indexer.conflicts"),
            "no failing nor lastError": (lambda h: h["indexer"].pop("failing"), "node.indexer.failing"),
            "indexer a list": (lambda h: h.__setitem__("indexer", []), "node.indexer"),
            "indexer missing": (lambda h: h.pop("indexer"), "node.indexer"),
            "reporter missing": (lambda h: h.pop("reporter"), "node.reporter.stuck"),
            "stuckSince a string": (lambda h: h["reporter"].__setitem__("stuckSince", "soon"), "node.reporter.stuck"),
            "acceptingPayments missing": (lambda h: h.pop("acceptingPayments"), "node.payments"),
            "chainId missing": (lambda h: h.pop("chainId"), "node.chain"),
            "receiptWriteErrors missing": (lambda h: h.pop("receiptWriteErrors"), "node.receipt_write_errors"),
        }
        for name, (mut, check) in mutations.items():
            with self.subTest(name):
                self.fresh()
                self.fake.health = healthy_node()
                mut(self.fake.health)
                line = self.assert_alert(check)
                self.assertIn("malformed", line["detail"])


class RpcChecks(Base):
    def test_rpc_down_alerts_after_configured_runs(self):
        self.opts.rpc = "http://127.0.0.1:1/rpc"
        n = self.cfg["after"]["rpc.up"]
        for _ in range(n - 1):
            self.assertEqual(self.status("rpc.up")[1], "warn")
        line = self.assert_alert("rpc.up")
        self.assertIn("connection refused", line["detail"])
        code, by = self.run_watch()
        self.assertNotIn("balances.revenue", by)

    def test_stalled_rpc_times_out_and_alerts(self):
        self.fake.delay["rpc"] = 1.0
        self.cfg["after"]["rpc.up"] = 1
        t = self.clock.now()
        self.assertIn("timed out", self.assert_alert("rpc.up")["detail"])
        rpc = self.cfg["rpc"]
        self.assertLessEqual(self.clock.now() - t, rpc["timeout_s"] * rpc["tries"] + self.opts.backoff * (rpc["tries"] - 1),
                             "the stall must be bounded by rpc.timeout_s x tries")

    def test_rpc_http_error_is_down(self):
        self.fake.rpc_status = 429
        self.cfg["after"]["rpc.up"] = 1
        self.assertIn("HTTP 429", self.assert_alert("rpc.up")["detail"])

    def test_wrong_chain_alerts_and_skips_balances(self):
        self.fake.chain = 1
        line = self.assert_alert("rpc.chain")
        self.assertEqual((line["value"], line["limit"]), (1, 84532))
        code, by = self.run_watch()
        self.assertNotIn("balances.deployer_eth", by, "balances on the wrong chain mean nothing")
        # The node's lag is then judged on its own finalizedHead only.
        self.assertEqual(by["node.indexer.lag"]["status"], "ok")

    def test_malformed_rpc_answers_alert(self):
        cases = {
            "error object": lambda r: json.dumps({"jsonrpc": "2.0", "id": r["id"], "error": {"code": -32000, "message": "boom"}}).encode(),
            "no result": lambda r: json.dumps({"jsonrpc": "2.0", "id": r["id"]}).encode(),
            "wrong id": lambda r: json.dumps({"jsonrpc": "2.0", "id": r["id"] + 9, "result": "0x14a34"}).encode(),
            "not json": lambda r: b"<html>",
            "not hex": lambda r: json.dumps({"jsonrpc": "2.0", "id": r["id"], "result": "84532"}).encode(),
            "result null": lambda r: json.dumps({"jsonrpc": "2.0", "id": r["id"], "result": None}).encode(),
        }
        for name, raw in cases.items():
            with self.subTest(name):
                self.fresh()
                self.fake.rpc_raw = raw
                self.assert_alert("rpc.chain")

    def test_malformed_block_alerts(self):
        orig = self.fake.rpc
        self.fake.rpc_raw = lambda r: json.dumps({"jsonrpc": "2.0", "id": r["id"], "result":
                                                  {"number": "0xzz"} if r["method"] == "eth_getBlockByNumber" else orig(r)}).encode()
        self.assert_alert("rpc.head_age")
        self.assert_alert("rpc.finalized_lag")

    def test_head_age_edge(self):
        lim = self.cfg["rpc"]["head_age_max_s"]
        self.edge("rpc.head_age", lambda v: setattr(self.fake, "head_ts", NOW - v), lim)

    def test_head_in_the_future_edge(self):
        lim = self.cfg["rpc"]["head_age_max_s"]
        self.edge("rpc.head_age", lambda v: setattr(self.fake, "head_ts", NOW + v), lim)

    def test_finalized_lag_edge(self):
        lim = self.cfg["rpc"]["finalized_lag_max_blocks"]
        self.fake.head = 10 * lim
        self.fake.health["indexer"].update(finalizedHead=0, nextBlock=1)
        self.cfg["node"]["indexer_lag_max_blocks"] = 10**9
        self.edge("rpc.finalized_lag", lambda v: setattr(self.fake, "final", self.fake.head - v), lim)

    def test_finalized_above_latest_alerts(self):
        self.fake.final = self.fake.head + 1
        self.assert_alert("rpc.finalized_lag")

    def test_rpc_url_is_never_printed(self):
        self.opts.rpc = self.url + "/rpc?apikey=SECRET123"
        self.fake.chain = 5
        self.opts.node = None
        buf = io.StringIO()
        hw.run_once(self.cfg, self.opts, now=NOW, out=buf)
        self.assertNotIn("SECRET123", buf.getvalue())
        self.assertNotIn("/rpc", buf.getvalue())


class Balances(Base):
    def test_deployer_eth_edge(self):
        need = int(self.cfg["balances"]["deployer_wei_min"])
        self.fresh()
        self.fake.eth = need
        self.assert_ok("balances.deployer_eth")
        self.fake.eth = need - 1
        self.assert_alert("balances.deployer_eth")

    def test_revenue_invariant_edge(self):
        self.fake.allocated, self.fake.paid = 7500, 1250
        self.fake.held = 6250
        self.assert_ok("balances.revenue")
        self.fake.held = 6249
        line = self.assert_alert("balances.revenue")
        self.assertEqual((line["value"], line["limit"]), ("6249", "6250"))
        self.fake.held = 10**6  # more than owed (unallocated USDC) is fine
        self.assert_ok("balances.revenue")

    def test_paid_out_above_allocated_alerts(self):
        self.fake.paid = self.fake.allocated + 1
        self.assert_alert("balances.revenue")

    def test_wrong_token_alerts(self):
        self.fake.token = int(DEPLOYER, 16)
        self.assertIn("token()", self.assert_alert("balances.revenue")["detail"])

    def test_no_contract_at_revenue_alerts(self):
        self.cfg["addresses"]["revenue"] = "0x" + "11" * 20  # eth_call answers "0x"
        self.assertIn("no contract", self.assert_alert("balances.revenue")["detail"])

    def test_operator_eth_edge(self):
        need = int(self.cfg["balances"]["operator_wei_min"])
        self.fresh()
        self.fake.eth_of[OPERATOR] = need
        self.assert_ok("balances.operator_eth")
        self.fake.eth_of[OPERATOR] = need - 1
        line = self.assert_alert("balances.operator_eth")
        self.assertEqual((line["value"], line["limit"]), (str(need - 1), str(need)))
        self.assertIn("operator", line["detail"])

    def test_operator_eth_is_read_from_the_operator_not_the_deployer(self):
        self.fake.eth_of[OPERATOR] = 0
        code, by = self.run_watch()
        self.assertEqual(by["balances.deployer_eth"]["status"], "ok")
        self.assertEqual(by["balances.operator_eth"]["status"], "alert")

    def test_operator_eth_skips_while_payments_are_off(self):
        self.fake.eth_of[OPERATOR] = 0
        self.fake.health["payments"] = None
        code, by = self.run_watch()
        self.assertEqual(by["balances.operator_eth"]["status"], "skip", by["balances.operator_eth"])
        self.assertIn("payments off", by["balances.operator_eth"]["detail"])
        self.assertEqual(code, 0)

    def test_operator_eth_alerts_when_the_node_cannot_say_payments_are_off(self):
        self.fake.eth_of[OPERATOR] = 0
        self.fake.health["payments"] = None
        self.opts.node = None
        code, by = self.run_watch()
        self.assertEqual(by["balances.operator_eth"]["status"], "alert", "no node answer: keep the reading")

    def test_unset_addresses_print_skip(self):
        self.cfg["addresses"].update(revenue=None, deployer=None, operator=None)
        code, by = self.run_watch()
        self.assertEqual(by["balances.revenue"]["status"], "skip")
        self.assertEqual(by["balances.deployer_eth"]["status"], "skip")
        self.assertEqual(by["balances.operator_eth"]["status"], "skip")
        self.assertEqual(code, 0)


class Site(Base):
    def test_preview_build_alerts(self):
        self.fake.demo = {"runs": [{"input": "input-1.b64.txt"}]}
        self.assert_alert("site.public")

    def test_public_flag_with_label_input_alerts(self):
        self.fake.demo = {"public": True, "runs": [{"input": None}, {"input": "input-2.b64.txt"}]}
        self.assert_alert("site.public")

    def test_malformed_demo_alerts(self):
        for raw in (b"not json", b"[]", b'{"public": true}', b'{"public": "true", "runs": []}'):
            with self.subTest(raw):
                self.fake.demo_raw = raw
                self.assert_alert("site.public")

    def test_site_down_alerts(self):
        self.fake.site_status = 503
        self.assert_alert("site.up")
        self.opts.site = "http://127.0.0.1:1/site"
        self.assert_alert("site.up")
        self.assert_alert("site.public")


class Dedup(Base):
    def setUp(self):
        super().setUp()
        self.opts.rpc = None  # these runs move the clock on; a fixed fake head would age
        self.opts.site = None
        self.cfg["require"]["rpc"] = False

    def test_one_notification_per_alert_then_repeat_then_resolve(self):
        realert = self.cfg["notify"]["realert_after_s"]
        self.fake.health["indexer"]["conflicts"] = 1
        code, by = self.run_watch(now=NOW)
        self.assertEqual(by["node.indexer.conflicts"]["notify"], "alert")
        for t in (NOW + 60, NOW + realert - 1):
            code, by = self.run_watch(now=t)
            self.assertEqual(code, 1, "still alerting: exit 1 even when not notified again")
            self.assertIsNone(by["node.indexer.conflicts"]["notify"])
        code, by = self.run_watch(now=NOW + realert)
        self.assertEqual(by["node.indexer.conflicts"]["notify"], "repeat")
        self.fake.health["indexer"]["conflicts"] = 0
        code, by = self.run_watch(now=NOW + realert + 60)
        self.assertEqual((code, by["node.indexer.conflicts"]["notify"]), (0, "resolved"))
        ev = self.alerts()
        self.assertEqual([(e["event"], e["repeat"]) for e in ev], [("alert", False), ("alert", True), ("resolved", False)])
        self.assertEqual(ev[0]["since"], NOW)
        self.assertEqual(oct(os.stat(self.opts.alert_log).st_mode & 0o777), "0o600")
        self.assertEqual(oct(os.stat(self.opts.state).st_mode & 0o777), "0o600")

    def test_notify_command_gets_json_on_stdin(self):
        out = os.path.join(self.tmp.name, "hook.out")
        hook = os.path.join(self.tmp.name, "hook.py")
        Path(hook).write_text(f"import sys\nopen({out!r}, 'a').write(sys.stdin.read())\n")
        self.opts.notify_cmd = f"{sys.executable} -I {hook}"
        self.fake.health["indexer"]["conflicts"] = 2
        self.run_watch()
        self.run_watch()
        got = [json.loads(x) for x in Path(out).read_text().splitlines()]
        self.assertEqual([(g["event"], g["check"]) for g in got], [("alert", "node.indexer.conflicts")])
        self.assertEqual((got[0]["value"], got[0]["limit"], got[0]["since"]), (2, 0, NOW))

    def test_failed_notify_command_is_retried(self):
        flag = os.path.join(self.tmp.name, "fail")
        Path(flag).write_text("1")
        out = os.path.join(self.tmp.name, "hook.out")
        hook = os.path.join(self.tmp.name, "hook.py")
        Path(hook).write_text(
            f"import os, sys\nd = sys.stdin.read()\nif os.path.exists({flag!r}): sys.exit(3)\nopen({out!r}, 'a').write(d)\n")
        self.opts.notify_cmd = f"{sys.executable} -I {hook}"
        self.fake.health["indexer"]["candidatesDropped"] = 1
        code, by = self.run_watch()
        self.assertEqual(by["node.indexer.candidates_dropped"]["notify"], "alert")
        os.remove(flag)
        code, by = self.run_watch(now=NOW + 60)
        self.assertEqual(by["node.indexer.candidates_dropped"]["notify"], "alert", "unsent alert must be retried")
        code, by = self.run_watch(now=NOW + 120)
        self.assertIsNone(by["node.indexer.candidates_dropped"]["notify"])
        self.assertEqual(len(Path(out).read_text().splitlines()), 1)

    def test_corrupt_state_file_alerts_and_recovers(self):
        Path(self.opts.state).write_text("{not json")
        line = self.assert_alert("watch.state")
        self.assertTrue(os.path.exists(self.opts.state + ".bad"))
        code, by = self.run_watch()
        self.assertEqual((code, by["watch.state"]["status"], by["watch.state"]["notify"]), (0, "ok", "resolved"))


class Config(Base):
    def test_thresholds_file_is_complete_and_strict(self):
        cfg = hw.load_config([])
        self.assertEqual(cfg["chain_id"], 84532)
        bad = os.path.join(self.tmp.name, "bad.json")
        for over, msg in (({"node": {"intents_open_mx": 1}}, "unknown key"),
                          ({"after": {"node.upp": 3}}, "unknown check"),
                          ({"after": {"node.up": 0}}, ">= 1"),
                          ({"rpc": {"head_age_max_s": "60"}}, "must be int"),
                          ({"node": {"conflicts_max": True}}, "must be int"),
                          ({"rpc": {"timeout_s": -1}}, "negative"),
                          ({"addresses": {"revenue": "0x1234"}}, "not a 0x address"),
                          ({"balances": {"deployer_wei_min": "1e15"}}, "decimal"),
                          ({"balances": {"operator_wei_min": "2e13"}}, "decimal"),
                          ({"addresses": {"operator": "0x12"}}, "not a 0x address")):
            with self.subTest(over):
                Path(bad).write_text(json.dumps(over))
                with self.assertRaises(hw.ConfigError) as e:
                    hw.load_config([bad])
                self.assertIn(msg, str(e.exception))

    def test_addresses_match_deployment_md(self):
        text = (ROOT / "docs/ops/deployment.md").read_text()
        table = text.split("### Deployed addresses (Base Sepolia)")[1].split("\n## ")[0]
        rows = dict(re.findall(r"^\| ([^|]+?) \| `(0x[0-9a-fA-F]{40})` \|", table, re.M))
        a = hw.load_config([])["addresses"]
        self.assertEqual(rows.get("Deployer (coordinator, OnyxRevenue owner and treasury)", "").lower(), a["deployer"])
        self.assertEqual(rows.get("OnyxRevenue (x402 `payTo`)", "").lower(), a["revenue"])
        self.assertEqual(rows.get("API operator (OnyxRevenue reporter, self-settles x402 payments)", "").lower(), a["operator"])
        self.assertEqual(rows.get("USDC (Circle's Base Sepolia test token)", "").lower(), a["usdc"])

    def test_launch_checklist_links_the_thresholds_file(self):
        text = (ROOT / "docs/ops/launch-checklist.md").read_text()
        self.assertTrue("](../../deploy/health-watch/thresholds.json)" in text, "launch-checklist.md must link the thresholds file")

    def test_local_overlay_loads(self):
        cfg = hw.load_config([str(ROOT / "deploy/health-watch/local-demo.json")])
        self.assertEqual(cfg["chain_id"], 31337)
        self.assertEqual(cfg["node"], hw.load_config([])["node"], "the overlay keeps every launch threshold")

    def test_systemd_units_point_at_the_script(self):
        svc = (ROOT / "deploy/systemd/user/onyx-health-watch.service").read_text()
        timer = (ROOT / "deploy/systemd/user/onyx-health-watch.timer").read_text()
        self.assertIn('scripts/health-watch.py --once', svc)
        self.assertIn("OnUnitActiveSec=60", timer)
        self.assertTrue("ONYX_WATCH_RPC_URL" in svc and "ONYX_WATCH_RPC_URL" in (ROOT / "docs/ops/runbooks/health-watch.md").read_text())


class Cli(Base):
    def cli(self, *args, env=None):
        return subprocess.run([sys.executable, "-I", str(SCRIPT), *args], capture_output=True, text=True,
                              env={**os.environ, **(env or {})}, timeout=60)

    def test_exit_codes_and_output(self):
        st = os.path.join(self.tmp.name, "cli-state.json")
        # A healthy head needs the real clock here.
        self.fake.head_ts = int(time.time())
        p = self.cli("--once", "--node", self.url + "/node", "--site", self.url + "/site", "--state", st,
                     env={"ONYX_WATCH_RPC_URL": self.url + "/rpc"})
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        lines = [json.loads(x) for x in p.stdout.splitlines()]
        self.assertTrue(lines and all(x["status"] == "ok" for x in lines), p.stdout)
        self.fake.chain = 31337
        p = self.cli("--once", "--node", self.url + "/node", "--rpc", self.url + "/rpc", "--state", st)
        self.assertEqual(p.returncode, 1, p.stdout)
        self.assertIn("alert: rpc.chain", p.stderr)
        p = self.cli("--once", "--state", st)
        self.assertEqual(p.returncode, 2)
        p = self.cli("--node", self.url + "/node", "--state", st)
        self.assertEqual(p.returncode, 2)
        bad = os.path.join(self.tmp.name, "bad.json")
        Path(bad).write_text('{"nope": 1}')
        p = self.cli("--once", "--node", self.url + "/node", "--state", st, "--config", bad)
        self.assertEqual(p.returncode, 2)
        self.assertIn("unknown key nope", p.stderr)

    def test_rpc_file(self):
        f = os.path.join(self.tmp.name, "rpc-url")
        Path(f).write_text(self.url + "/rpc?key=HIDDEN\n")
        self.fake.chain = 7
        p = self.cli("--once", "--rpc-file", f, "--state", os.path.join(self.tmp.name, "s.json"))
        self.assertEqual(p.returncode, 1)
        self.assertNotIn("HIDDEN", p.stdout + p.stderr)


# ---------------------------------------------------------------- review L12-R1

BAD_STATES = {
    "check entry a string (probe s2)": {"version": 1, "checks": {"node.up": "x"}, "mem": {}},
    "mem counter a string (probe s3)": {"version": 1, "checks": {}, "mem": {"unknown_outcomes": "x"}},
    "mem a list (probe s4)": {"version": 1, "checks": {}, "mem": []},
    "notified a string": {"version": 1, "checks": {"node.up": {"status": "alert", "consec": 3, "since": 1, "notified": "soon"}}, "mem": {}},
    "consec a string": {"version": 1, "checks": {"node.up": {"status": "ok", "consec": "1"}}, "mem": {}},
    "consec negative": {"version": 1, "checks": {"node.up": {"status": "ok", "consec": -1}}, "mem": {}},
    "consec a bool": {"version": 1, "checks": {"node.up": {"status": "ok", "consec": True}}, "mem": {}},
    "unknown status": {"version": 1, "checks": {"node.up": {"status": "bogus", "consec": 0}}, "mem": {}},
    "unknown field": {"version": 1, "checks": {"node.up": {"status": "ok", "consec": 0, "x": 1}}, "mem": {}},
    "resolve_pending a string": {"version": 1, "checks": {"node.up": {"status": "ok", "consec": 0, "resolve_pending": "y"}}, "mem": {}},
    "checks a list": {"version": 1, "checks": [], "mem": {}},
    "intents_open_since a float": {"version": 1, "checks": {}, "mem": {"intents_open_since": 1.5}},
    "unknown mem key": {"version": 1, "checks": {}, "mem": {"other": 1}},
    "no version": {"checks": {}, "mem": {}},
}


class StateRobustness(Base):
    """Review L12-R1 #1: valid JSON with wrong inner types crashed every run before the save."""

    def test_every_bad_shape_alerts_moves_the_file_aside_and_recovers(self):
        self.fake.health["settles"]["unknownOutcomes"] = 4
        for name, shape in BAD_STATES.items():
            with self.subTest(name):
                self.fresh()
                for leftover in (self.opts.state + ".bad",):
                    if os.path.exists(leftover):
                        os.remove(leftover)
                Path(self.opts.state).write_text(json.dumps(shape))
                line = self.assert_alert("watch.state")
                self.assertIn("moved to", line["detail"])
                self.assertTrue(os.path.exists(self.opts.state + ".bad"))
                code, by = self.run_watch()
                self.assertEqual((code, by["watch.state"]["status"]), (0, "ok"), by["watch.state"])
                self.assertTrue(hw.validate_state(json.loads(Path(self.opts.state).read_text())) is None)

    def test_last_line_catch_when_validation_misses_a_shape(self):
        orig = hw.validate_state
        hw.validate_state = lambda s: None  # pretend the validator let a bad shape through
        self.addCleanup(setattr, hw, "validate_state", orig)
        Path(self.opts.state).write_text(json.dumps(BAD_STATES["mem counter a string (probe s3)"]))
        self.fake.health["settles"]["unknownOutcomes"] = 4
        line = self.assert_alert("watch.state")
        self.assertIn("TypeError", line["detail"])
        hw.validate_state = orig
        code, by = self.run_watch()
        self.assertEqual(code, 0, {k: v for k, v in by.items() if v["status"] != "ok"})


class MissingTargets(Base):
    """Review L12-R1 #2: a missing RPC made rpc.* and balances.* vanish with exit 0."""

    def test_missing_rpc_alerts_under_the_launch_config(self):
        self.assertTrue(self.cfg["require"]["rpc"], "the launch config requires an RPC")
        self.opts.rpc = None
        code, by = self.run_watch()
        self.assertEqual((code, by["rpc.up"]["status"]), (1, "alert"))
        self.assertIn("no RPC", by["rpc.up"]["detail"])
        for c in ("rpc.chain", "rpc.head_age", "rpc.finalized_lag", "balances.deployer_eth", "balances.operator_eth",
                  "balances.revenue"):
            self.assertEqual(by[c]["status"], "skip", by[c])
            self.assertIn("no RPC", by[c]["detail"])

    def test_missing_rpc_skips_explicitly_when_not_required(self):
        self.cfg["require"]["rpc"] = False
        self.opts.rpc = None
        code, by = self.run_watch()
        self.assertEqual(code, 0)
        for c in ("rpc.up", "rpc.chain", "rpc.head_age", "rpc.finalized_lag", "balances.deployer_eth",
                  "balances.operator_eth", "balances.revenue"):
            self.assertEqual(by[c]["status"], "skip", by[c])

    def test_missing_node_and_site(self):
        self.opts.node, self.opts.site = None, None
        code, by = self.run_watch()
        self.assertEqual(by["node.up"]["status"], "alert", "the launch config requires the node")
        self.assertFalse(self.cfg["require"]["site"])
        self.assertEqual((by["site.up"]["status"], by["site.public"]["status"]), ("skip", "skip"))
        self.cfg["require"]["site"] = True
        code, by = self.run_watch()
        self.assertEqual(by["site.up"]["status"], "alert")

    def test_cron_recipe_exports_the_env_file(self):
        text = (ROOT / "docs/ops/runbooks/health-watch.md").read_text()
        cron = [l for l in text.splitlines() if "* * * * *" in l]
        self.assertTrue(cron and all("set -a" in l and "set +a" in l for l in cron), cron)


class Deadline(Base):
    """Review L12-R1 #3: one overall deadline; remaining checks alert and the state is saved."""

    def test_slow_rpc_hits_the_deadline_and_everything_left_alerts(self):
        self.fake.rpc_slow_odd = 1.0  # every call's first try times out (reviewer probe rpc.py)
        self.cfg["deadline_s"] = 2
        # 0.75 s does not divide deadline_s: a wait not cut to the budget would run past it.
        self.cfg["rpc"]["timeout_s"] = 0.75
        t = self.clock.now()
        code, by = self.run_watch()
        took = self.clock.now() - t
        # Virtual time: every wait of the run is cut to what is left of the budget, so the run ends
        # by deadline_s exactly (bounded() stops it with at most 0.05 s left), on any machine.
        self.assertLessEqual(took, self.cfg["deadline_s"], "the run must end by deadline_s")
        self.assertGreaterEqual(took, self.cfg["deadline_s"] - 0.05, "the run stopped before its budget ran out")
        self.assertEqual(code, 1)
        for c in ("rpc.up", "rpc.chain", "rpc.head_age", "rpc.finalized_lag", "balances.deployer_eth",
                  "balances.operator_eth", "balances.revenue", "node.up", "site.up", "site.public"):
            self.assertIn(c, by)
        late = [c for c, v in by.items() if "deadline" in v["detail"]]
        self.assertIn("node.up", late)
        self.assertIn("site.public", late)
        for c in late:
            self.assertEqual(by[c]["status"], "alert", by[c])
        saved = json.loads(Path(self.opts.state).read_text())
        self.assertIn("node.up", saved["checks"])

    def test_unit_timeout_is_above_the_deadline(self):
        svc = (ROOT / "deploy/systemd/user/onyx-health-watch.service").read_text()
        timeout = int(re.search(r"^TimeoutStartSec=(\d+)$", svc, re.M).group(1))
        cfg = hw.load_config([])
        slack = max(cfg["rpc"]["timeout_s"], cfg["node"]["timeout_s"], cfg["site"]["timeout_s"],
                    cfg["notify"]["command_timeout_s"]) * 2
        self.assertGreaterEqual(timeout, cfg["deadline_s"] + slack)


class HookAndUnit(Base):
    """Review L12-R1 #4, #5, #10."""

    def test_hook_does_not_see_secrets(self):
        out = os.path.join(self.tmp.name, "env.out")
        hook = os.path.join(self.tmp.name, "hook.py")
        Path(hook).write_text(f"import os, sys, json\nsys.stdin.read()\nopen({out!r}, 'w').write(json.dumps(sorted(os.environ)))\n")
        self.opts.notify_cmd = f"{sys.executable} -I {hook}"
        planted = {"ONYX_WATCH_RPC_URL": "http://x/KEY", "ONYX_RPC_URL": "http://y/KEY", "BASE_SEPOLIA_RPC_URL": "z",
                   "BASE_SEPOLIA_PRIVATE_KEY": "0xdead", "CDP_API_KEY_SECRET": "s", "SOME_TOKEN": "t", "MY_PASSWORD": "p"}
        old = {k: os.environ.get(k) for k in planted}
        os.environ.update(planted)
        def restore():
            for k, v in old.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
        self.addCleanup(restore)
        self.fake.health["indexer"]["conflicts"] = 1
        self.run_watch()
        seen = set(json.loads(Path(out).read_text()))
        self.assertEqual(seen & set(planted), set())
        self.assertIn("PATH", seen)

    def test_env_supplies_node_site_and_notify_cmd(self):
        out = os.path.join(self.tmp.name, "hook.out")
        hook = os.path.join(self.tmp.name, "hook.py")
        Path(hook).write_text(f"import sys\nopen({out!r}, 'a').write(sys.stdin.read())\n")
        self.fake.health["indexer"]["conflicts"] = 1
        self.fake.head_ts = int(time.time())
        env = {**os.environ, "ONYX_WATCH_NODE": self.url + "/node", "ONYX_WATCH_SITE": self.url + "/site",
               "ONYX_WATCH_RPC_URL": self.url + "/rpc", "ONYX_WATCH_NOTIFY_CMD": f"{sys.executable} -I {hook} 'two words'"}
        p = subprocess.run([sys.executable, "-I", str(SCRIPT), "--once", "--state", os.path.join(self.tmp.name, "e.json")],
                           capture_output=True, text=True, env=env, timeout=60)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        checks = {json.loads(x)["check"] for x in p.stdout.splitlines()}
        self.assertTrue({"node.up", "site.public", "rpc.chain"} <= checks)
        self.assertEqual(json.loads(Path(out).read_text().splitlines()[0])["check"], "node.indexer.conflicts")

    def test_unit_is_hardened_and_has_no_word_split_args(self):
        svc = (ROOT / "deploy/systemd/user/onyx-health-watch.service").read_text()
        body = "\n".join(l for l in svc.splitlines() if not l.startswith("#"))
        for line in ("ProtectSystem=strict", "ProtectHome=read-only", "ReadWritePaths=%S/onyx-health-watch",
                     "NoNewPrivileges=yes", "PrivateTmp=yes"):
            self.assertIn(line, body.splitlines())
        self.assertNotIn("ONYX_WATCH_ARGS", svc)
        self.assertNotIn("/bin/sh", body)


class Bounds(Base):
    """Review L12-R1 #9: impossible values alert instead of clamping to 0."""

    def test_indexer_cursor_ahead_of_finalized_head_edge(self):
        lim = self.cfg["node"]["indexer_ahead_max_blocks"]
        def s(v):
            self.fake.health["indexer"].update(finalizedHead=1000, nextBlock=1000 + v + 1)
        self.fake.final = 900
        self.edge("node.indexer.lag", s, lim)

    def test_stuck_since_in_the_future_edge(self):
        lim = self.cfg["node"]["clock_skew_max_s"]
        self.edge("node.reporter.stuck", lambda v: self.fake.health["reporter"].__setitem__("stuckSince", NOW + v), lim)


class Level3(Base):
    """ROADMAP C62 (THREATS C10-3, C62-1, C62-2): the level-3 post log on /v1/health, every threshold
    at its edge, and the alert at least two runs before the breaker."""

    def l3(self, **kw):
        self.fake.health["level3"].update(kw)

    def pending(self, n, secs):
        self.l3(postsPending=n, oldestPendingSince=None if secs is None else NOW - secs, oldestPendingSecs=secs)

    def test_pending_for_edge(self):
        lim = self.cfg["node"]["level3_pending_for_max_s"]
        self.assertLess(lim, hw.LEVEL3_BREAKER_S)
        self.edge("node.level3.pending_for", lambda v: self.pending(1, v), lim)
        self.fresh()
        self.pending(0, None)
        line = self.assert_ok("node.level3.pending_for")
        self.assertIsNone(line["value"])

    def test_the_alert_comes_two_runs_before_the_breaker(self):
        # One run a minute (the timer), the post pending from a run's start: the watch alerts on a
        # run while the node still serves, at least two runs before the breaker stops level 3.
        lim = self.cfg["node"]["level3_pending_for_max_s"]
        for offset in (0, 1, 59):
            with self.subTest(offset):
                self.fresh()
                first = None
                for age in range(offset, hw.LEVEL3_BREAKER_S + 2 * hw.WATCH_INTERVAL_S, hw.WATCH_INTERVAL_S):
                    self.pending(1, age)
                    self.l3(stopped="level3_posts_delayed" if age > hw.LEVEL3_BREAKER_S else None)
                    code, by = self.run_watch()
                    if by["node.level3.pending_for"]["status"] == "alert" and first is None:
                        first = age
                        self.assertEqual(by["node.level3.stopped"]["status"], "ok")
                self.assertIsNotNone(first)
                self.assertGreater(first, lim)
                self.assertLessEqual(first + 2 * hw.WATCH_INTERVAL_S, hw.LEVEL3_BREAKER_S)

    def test_the_alert_comes_before_the_breaker_at_the_cap_with_slow_runs(self):
        # Review C61-R2-1: at the largest threshold the config takes, with every run as slow as
        # deadline_s and the timer late by its accuracy (reads of /v1/health that far apart), the
        # first alert still comes while the node serves, with one more run before the breaker.
        deadline = self.cfg["deadline_s"]
        gap = deadline + hw.WATCH_INTERVAL_S + hw.WATCH_ACCURACY_S
        cap = hw.LEVEL3_BREAKER_S - hw.level3_need(deadline)
        self.cfg["node"]["level3_pending_for_max_s"] = cap
        for offset in (0, 1, gap - 1):
            with self.subTest(offset):
                self.fresh()
                first = None
                for age in range(offset, hw.LEVEL3_BREAKER_S + 2 * gap, gap):
                    self.pending(1, age)
                    self.l3(stopped="level3_posts_delayed" if age > hw.LEVEL3_BREAKER_S else None)
                    code, by = self.run_watch()
                    if by["node.level3.pending_for"]["status"] == "alert" and first is None:
                        first = age
                        self.assertEqual(by["node.level3.stopped"]["status"], "ok")
                        self.assertEqual(by["node.level3.breaker"]["status"], "ok")
                self.assertIsNotNone(first)
                self.assertGreater(first, cap)
                self.assertLessEqual(first + hw.WATCH_INTERVAL_S, hw.LEVEL3_BREAKER_S)
        # One second past the cap, the slowest runs alert only with no run left before the breaker.
        self.assertGreater(cap + 1 + gap + hw.WATCH_INTERVAL_S, hw.LEVEL3_BREAKER_S)

    def test_pending_count_edge(self):
        def s(v):
            self.pending(v, 5 if v else None)
        self.edge("node.level3.pending", s, self.cfg["node"]["level3_pending_max"])

    def refused(self, final, retrying=0):
        self.l3(postsRefused=final + retrying, postsRefusedFinal=final, postsRefusedRetrying=retrying)

    def test_refused_new_since_last_run(self):
        lim = self.cfg["node"]["level3_refused_new_max"]
        self.refused(4)
        self.assert_ok("node.level3.refused_new")  # first run: baseline
        self.refused(4 + lim)
        self.assert_ok("node.level3.refused_new")
        self.refused(4 + lim + lim + 1)
        line = self.assert_alert("node.level3.refused_new")
        self.assertEqual(line["value"], lim + 1)
        self.assert_ok("node.level3.refused_new")  # no new one
        # A restart or a new log: a lower total counts from zero.
        self.refused(lim + 1)
        self.assert_alert("node.level3.refused_new")

    def test_retryable_refusals_do_not_count_as_new_and_cannot_mask_one(self):
        # Review C61-R1-2: a refusal that may clear (FromFuture...) is node.level3.retrying's, debounced;
        # it never fires refused_new, and one clearing cannot hide a new refusal for good.
        self.refused(0)
        self.assert_ok("node.level3.refused_new")
        self.refused(0, retrying=1)
        self.assertEqual(self.status("node.level3.refused_new")[1], "ok")
        self.refused(1, retrying=0)  # the retryable one landed; a new terminal one arrived
        self.assert_alert("node.level3.refused_new")

    def test_retrying_edge_and_debounce(self):
        lim = self.cfg["node"]["level3_retrying_max"]
        n = self.cfg["after"]["node.level3.retrying"]
        self.l3(postsRefused=lim + 1, postsRefusedRetrying=lim)
        self.assert_ok("node.level3.retrying")
        self.l3(postsRefusedRetrying=lim + 1)
        for _ in range(n - 1):
            self.assertEqual(self.status("node.level3.retrying")[1], "warn")
        self.assert_alert("node.level3.retrying")

    def test_stopped_alerts_for_every_stop(self):
        for code in ("level3_posts_delayed", "level3_clock_skew", "level3_store_unreadable"):
            with self.subTest(code):
                self.fresh()
                self.fake.health["level3"] = healthy_level3()
                self.l3(stopped=code)
                line = self.assert_alert("node.level3.stopped")
                self.assertIn(code, line["detail"])
        self.fresh()
        self.fake.health["level3"] = healthy_level3()
        self.assert_ok("node.level3.stopped")

    def test_breaker_room_edge(self):
        # The node's breaker must leave the longest gap between two runs and one more run after this
        # watch's alert time (review C61-R2-1).
        lim = self.cfg["node"]["level3_pending_for_max_s"]
        need = hw.level3_need(self.cfg["deadline_s"])
        self.assertEqual(need, self.cfg["deadline_s"] + hw.WATCH_ACCURACY_S + 2 * hw.WATCH_INTERVAL_S)
        self.l3(breakerSecs=lim + need)
        self.assert_ok("node.level3.breaker")
        self.fresh()
        self.l3(breakerSecs=lim + need - 1)
        line = self.assert_alert("node.level3.breaker")
        self.assertEqual((line["value"], line["limit"]), (need - 1, need))

    def test_absent_store_skips_unless_required(self):
        self.fake.health["level3"] = None
        code, by = self.run_watch()
        self.assertEqual(by["node.level3"]["status"], "skip")
        self.assertEqual(code, 0)
        for c in hw.LEVEL3_COUNT_CHECKS + ("node.level3.stopped", "node.level3.breaker"):
            self.assertNotIn(c, by)
        self.fresh()
        self.cfg["node"]["level3_required"] = True
        self.assert_alert("node.level3")

    def test_a_node_before_c62_skips_unless_required(self):
        # Review C61-R1-4: no level3 key at all (an older node).
        self.fake.health.pop("level3")
        code, by = self.run_watch()
        self.assertEqual((by["node.level3"]["status"], code), ("skip", 0))
        self.fresh()
        self.cfg["node"]["level3_required"] = True
        self.assert_alert("node.level3")

    def test_unreadable_log_alerts_every_count(self):
        self.fake.health["level3"] = {**healthy_level3(), "logReadable": False, "logError": "bad json",
                                      "stopped": "level3_store_unreadable",
                                      **{k: None for k in ("posts", "postsRecorded", "postsPending", "postsRefused",
                                                           "postsRefusedRetrying", "postsRefusedFinal",
                                                           "oldestPendingSince", "oldestPendingSecs")}}
        code, by = self.run_watch()
        self.assertEqual(code, 1)
        for c in hw.LEVEL3_COUNT_CHECKS + ("node.level3.stopped",):
            # node.level3.retrying waits its configured runs ("after"); stopped alerts at once.
            want = "warn" if self.cfg["after"].get(c, 1) > 1 else "alert"
            self.assertEqual(by[c]["status"], want, by[c])

    def test_malformed_level3_always_alerts(self):
        cases = [
            ("node.level3", lambda h: h.__setitem__("level3", [])),
            ("node.level3.stopped", lambda h: h["level3"].pop("stopped")),
            ("node.level3.stopped", lambda h: h["level3"].__setitem__("stopped", 5)),
            ("node.level3.breaker", lambda h: h["level3"].__setitem__("breakerSecs", "600")),
            ("node.level3.breaker", lambda h: h["level3"].__setitem__("breakerSecs", -1)),
            ("node.level3.pending_for", lambda h: h["level3"].pop("logReadable")),
            ("node.level3.pending_for", lambda h: h["level3"].update(postsPending=2, oldestPendingSecs=None)),
            ("node.level3.pending_for", lambda h: h["level3"].update(postsPending=0, oldestPendingSecs=10)),
            ("node.level3.pending_for", lambda h: h["level3"].update(postsPending=1, oldestPendingSecs=-5)),
            ("node.level3.pending", lambda h: h["level3"].__setitem__("postsPending", None)),
            ("node.level3.refused_new", lambda h: h["level3"].__setitem__("postsRefused", 1.5)),
            ("node.level3.retrying", lambda h: h["level3"].update(postsRefused=1, postsRefusedRetrying=2)),
            ("node.level3.refused_new", lambda h: h["level3"].update(postsRefused=1, postsRefusedFinal=2)),
            ("node.level3.refused_new", lambda h: h["level3"].pop("postsRefusedFinal")),
            ("node.level3.retrying", lambda h: h["level3"].__setitem__("postsRefusedRetrying", True)),
        ]
        for check, mutate in cases:
            with self.subTest(check=check):
                self.fresh()
                self.fake.health = healthy_node()
                mutate(self.fake.health)
                self.assert_alert(check)

    def test_config_keeps_two_runs_before_the_breaker(self):
        # Review C61-R2-1: the cap counts a slow run (deadline_s) and the timer's accuracy, not only
        # two nominal 60 s runs (480 s let the first alert come after the breaker).
        deadline = hw.load_config([])["deadline_s"]
        room = hw.LEVEL3_BREAKER_S - hw.level3_need(deadline)
        self.assertEqual(room, 385)
        ok = os.path.join(self.tmp.name, "ok.json")
        Path(ok).write_text(json.dumps({"node": {"level3_pending_for_max_s": room}}))
        self.assertEqual(hw.load_config([ok])["node"]["level3_pending_for_max_s"], room)
        bad = os.path.join(self.tmp.name, "bad.json")
        Path(bad).write_text(json.dumps({"node": {"level3_pending_for_max_s": room + 1}}))
        with self.assertRaises(hw.ConfigError) as e:
            hw.load_config([bad])
        self.assertIn("breaker", str(e.exception))
        # A slower deadline lowers the cap.
        slow = os.path.join(self.tmp.name, "slow.json")
        Path(slow).write_text(json.dumps({"deadline_s": deadline + 1, "node": {"level3_pending_for_max_s": room}}))
        with self.assertRaises(hw.ConfigError):
            hw.load_config([slow])
        timer = (ROOT / "deploy/systemd/user/onyx-health-watch.timer").read_text()
        self.assertIn(f"OnUnitActiveSec={hw.WATCH_INTERVAL_S}", timer)
        self.assertIn(f"AccuracySec={hw.WATCH_ACCURACY_S}\n", timer)
        serve = (ROOT / "crates/onyx-api/src/signed_serve.rs").read_text()
        self.assertIn(f"pub const POST_DELAY_ALERT_SECS: u64 = {hw.LEVEL3_BREAKER_S};", serve)


class Docs(Base):
    """Review L12-R1 #6, #7, #8."""

    def test_docs_state_their_limits(self):
        audit = (ROOT / "docs/audit/2026-10-15-health-watch.md").read_text()
        self.assertIn("systemd unit", audit.split("## What this does not show")[1])
        about = hw.load_config([])["rpc"]["_about"]
        self.assertIn("1,004", about)
        self.assertNotIn("about 500", about)
        checklist = (ROOT / "docs/ops/launch-checklist.md").read_text().split("## 3. Monitoring")[1].split("## 4.")[0]
        for word in ("rewardPool()", "coordinatorBond()", "receiptsPending"):
            self.assertIn(word, checklist)
        self.assertIn("not watched", checklist)


if __name__ == "__main__":
    unittest.main(verbosity=1)
