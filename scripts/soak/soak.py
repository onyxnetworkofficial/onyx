#!/usr/bin/env python3
"""Soak orchestrator (ROADMAP L6; started by scripts/soak.sh with its soak.json).

Runs the fault proxy, onyx-node and onyx-node-btc (both through the proxy),
the load (soak-load), and a fault schedule repeated every `cycleMinutes`:

    min 0-8    mixed      every fault type at low rates (27% of calls)
    min 8-10   outage     every call fails (5xx, dropped, timed out, errors)
    min 10-14  lag        stale/lagging answers, a "future head" lie, slow
    min 14-end clean      no faults: intents must drain, the indexer catch up

Restarts: onyx-node-btc every cycle (SIGTERM at min 5, SIGKILL at min 11);
onyx-node from the end of the leak window on (SIGTERM at min 4, SIGKILL at
min 9, in the outage). Before the leak window ends onyx-node runs without a
restart, so memory, descriptors and files can be watched over one long life.
A start that fails (a node error at start) is retried after 5 s, as the
systemd unit does (Restart=on-failure, RestartSec=5s), while the schedule
runs on (a restart never holds the faults in place); a node that is not up
again by the end of its cycle's clean window fails the run.

Every minute it samples each node (RSS, descriptors, threads, state files,
/v1/health) into samples.jsonl; events go to events.jsonl. At the end of each
cycle's clean window it checks that no settle intent is open and the
indexer is not failing. After `minutes` it stops the faults and the load,
waits (up to 30 min) until intents, receipts and credits in flight drain,
then stops both nodes with SIGTERM (exit 0 expected within 75 s).
Python standard library only.
"""

import json
import os
import signal
import subprocess
import sys
import time
import urllib.request

PROFILES = {
    "clean": {},
    "mixed": {
        "error": 0.04,
        "http429": 0.03,
        "http5xx": 0.03,
        "timeout": 0.01,
        "slow": 0.05,
        "stale": 0.04,
        "future": 0.01,
        "drop": 0.02,
        "lost": 0.02,
        "wrong_id": 0.01,
        "garbage": 0.01,
    },
    "outage": {"http5xx": 0.4, "drop": 0.3, "timeout": 0.1, "error": 0.2},
    "lag": {"stale": 0.5, "future": 0.15, "slow": 0.1},
}


def schedule(cycle):
    """(minute in cycle, profile) for a cycle of `cycle` minutes (>= 20)."""
    return [(0, "mixed"), (8, "outage"), (10, "lag"), (14, "clean")]


class Soak:
    def __init__(self, cfg):
        self.cfg = cfg
        self.work = cfg["work"]
        self.events = open(os.path.join(self.work, "events.jsonl"), "a", buffering=1)
        self.samples = open(os.path.join(self.work, "samples.jsonl"), "a", buffering=1)
        self.control = os.path.join(self.work, "faults.json")
        self.stop_file = os.path.join(self.work, "STOP")
        self.profile = None
        self.failures = []
        self.nodes = {}
        self.pending = {}
        self.t0 = time.time()

    def event(self, kind, **kw):
        kw.update(t=round(time.time(), 1), m=round((time.time() - self.t0) / 60, 2), event=kind)
        self.events.write(json.dumps(kw) + "\n")
        print(f"[{kw['m']:7.2f} min] {kind} {json.dumps({k: v for k, v in kw.items() if k not in ('t', 'm', 'event')})}", flush=True)

    def fail(self, what, **kw):
        self.failures.append(dict(what=what, **kw))
        self.event("INVARIANT_FAILED", what=what, **kw)

    def set_profile(self, name):
        if name == self.profile:
            return
        tmp = self.control + ".tmp"
        with open(tmp, "w") as f:
            json.dump({"profile": name, "rates": PROFILES[name], "lag_blocks": 50}, f)
        os.replace(tmp, self.control)
        self.profile = name
        self.event("profile", profile=name)

    # ------------------------------------------------------------ nodes
    def node_env(self, name):
        c = self.cfg
        port = c["nodePort"] if name == "node" else c["btcPort"]
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "ONYX_ENV": "testnet",
            # A local soak on test labels; a testnet node must name its rights (L14-R1).
            "ONYX_RIGHTS": "cleared",
            "ONYX_LISTEN": f"127.0.0.1:{port}",
            "ONYX_PUBLIC_URL": f"http://127.0.0.1:{port}",
            "ONYX_RPC_URL": f"http://127.0.0.1:{c['proxyPort']}",
            "ONYX_DEPLOYMENT": os.path.join(self.work, "etc", "deployment.json"),
            "ONYX_CATALOG": os.path.join(self.work, "etc", "catalog.json"),
            "ONYX_CAS_DIR": os.path.join(self.work, f"cas-{name}"),
            "ONYX_STATE_DIR": os.path.join(self.work, f"state-{name}"),
            "ONYX_ALLOW_TEST_LABELS": "1",
            "ONYX_INDEXER": "on",
            "ONYX_INDEX_EVERY_SECS": "5",
            "ONYX_INDEX_RESCAN_POLLS": "60",
            "ONYX_PAYMENTS": "self-settle",
            "ONYX_OPERATOR_KEY_FILE": c["operatorKey"],
            "ONYX_REPORTER_EVERY_SECS": "10",
            "ONYX_GATE_REFRESH_SECS": "30",
        }
        if name == "btc":
            env["ONYX_RESERVE_GATE"] = os.path.join(self.work, "etc", "reserve-gate.json")
        return env, port

    def launch(self, name, why):
        """Begin starting `name` without waiting (the loop keeps the fault
        schedule running meanwhile); `tick_starts` finishes it."""
        self.pending[name] = {"why": why, "since": time.time(), "attempts": 0, "proc": None, "next": time.time()}

    def tick_starts(self):
        """Advance every pending start: spawn, wait for /v1/health, and after
        a failed start try again 5 s later (systemd: Restart=on-failure,
        RestartSec=5s)."""
        for name, st in list(self.pending.items()):
            env, port = self.node_env(name)
            p = st["proc"]
            now = time.time()
            if p is None:
                if now < st["next"]:
                    continue
                st["attempts"] += 1
                binary = self.cfg["nodeBin"] if name == "node" else self.cfg["btcBin"]
                with open(os.path.join(self.work, f"{name}.log"), "a") as log:
                    log.write(f"=== start {st['attempts']} ({st['why']}) at {time.strftime('%H:%M:%S')}\n")
                    log.flush()
                    st["proc"] = subprocess.Popen([binary], env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
                st["spawned"] = now
                continue
            if p.poll() is None and health(port, 2) is not None:
                self.nodes[name] = p
                del self.pending[name]
                self.event("started", node=name, pid=p.pid, attempts=st["attempts"], secs=round(now - st["since"], 1), why=st["why"], profile=self.profile)
                continue
            code = p.poll()
            if code is None and now - st["spawned"] < 120:
                continue
            if code is None:
                p.kill()
                p.wait()
                code = "no health in 120 s"
            self.event("start_failed", node=name, attempt=st["attempts"], code=code, profile=self.profile, tail=tail(os.path.join(self.work, f"{name}.log"), 2))
            st["proc"] = None
            st["next"] = now + 5

    def start(self, name, why):
        """Start `name` and wait until it is healthy (first start, clean)."""
        self.launch(name, why)
        t = time.time()
        while name in self.pending:
            self.tick_starts()
            if time.time() - t > 900:
                self.fail("node did not start in 15 min", node=name)
                raise SystemExit(1)
            time.sleep(0.5)

    def stop(self, name, sig):
        p = self.nodes.get(name)
        if p is None:
            return None
        t = time.time()
        p.send_signal(sig)
        try:
            code = p.wait(timeout=80)
        except subprocess.TimeoutExpired:
            p.kill()
            code = p.wait()
            self.fail("SIGTERM stop took over 80 s", node=name)
        secs = round(time.time() - t, 1)
        signame = "SIGTERM" if sig == signal.SIGTERM else "SIGKILL"
        self.event("stopped", node=name, signal=signame, code=code, secs=secs, tail=tail(os.path.join(self.work, f"{name}.log"), 3))
        if sig == signal.SIGTERM and code != 0:
            self.fail("SIGTERM exit code not 0", node=name, code=code)
        del self.nodes[name]
        return code, secs

    def restart(self, name, sig):
        if name in self.pending:
            return
        self.stop(name, sig)
        self.launch(name, "restart after " + ("SIGTERM" if sig == signal.SIGTERM else "SIGKILL"))

    # ------------------------------------------------------------ samples
    def sample(self, label=None):
        c = self.cfg
        s = {"t": round(time.time(), 1), "m": round((time.time() - self.t0) / 60, 2), "profile": self.profile}
        if label:
            s["label"] = label
        s["anvilBlock"] = anvil_block(c["anvil"])
        for name, p in list(self.nodes.items()):
            port = c["nodePort"] if name == "node" else c["btcPort"]
            s[name] = proc_stats(p.pid)
            s[name]["state"] = dir_sizes(os.path.join(self.work, f"state-{name}"))
            s[name]["health"] = slim_health(health(port, 5))
        s["loadLines"] = count_lines(os.path.join(self.work, "load.jsonl"))
        s["violations"] = count_violations(os.path.join(self.work, "load.jsonl"))
        try:
            with open(os.path.join(self.work, "proxy-stats.json")) as f:
                ps = json.load(f)
            s["proxy"] = {"requests": ps.get("requests"), "faults": sum(v for k, v in ps["faults"].items() if ":" not in k)}
        except (OSError, ValueError, KeyError):
            pass
        self.samples.write(json.dumps(s) + "\n")
        return s

    def check_clean_window(self, cycle_no):
        """End of a cycle's clean window: nothing stuck. `intentsOpen` also
        counts the intents of settles running now (load continues), so an
        intent is stuck only if open intents outnumber running settles in
        four reads 10 s apart."""
        released = {"node": [], "btc": []}
        for i in range(4):
            for name, p in self.nodes.items():
                port = self.cfg["nodePort"] if name == "node" else self.cfg["btcPort"]
                h = slim_health(health(port, 5)) or {}
                if h.get("intentsOpen") is not None:
                    released[name].append(h["intentsOpen"] - (h.get("settlesInFlight") or 0))
            if i < 3:
                time.sleep(10)
        s = self.sample(label=f"end of clean window {cycle_no}")
        real_final = anvil_finalized(self.cfg["anvil"])
        for name in ("node", "btc"):
            if name in self.pending:
                self.fail("node not started again by the end of a clean window", node=name, cycle=cycle_no, attempts=self.pending[name]["attempts"])
                continue
            h = (s.get(name) or {}).get("health")
            if h is None:
                self.fail("no health at the end of a clean window", node=name, cycle=cycle_no)
                continue
            r = released[name]
            if not r or min(r) > 0:
                self.fail("settle intents still open after the faults cleared", node=name, cycle=cycle_no, released=r)
            if h.get("indexerFailing"):
                self.fail("indexer still failing after the faults cleared", node=name, cycle=cycle_no, error=h.get("indexerError"))
            nb = h.get("nextBlock") or 0
            if real_final and nb + 400 < real_final:
                self.fail("indexer behind after the faults cleared", node=name, cycle=cycle_no, nextBlock=nb, finalized=real_final)
        if s.get("violations"):
            self.fail("load saw violations", count=s["violations"])

    # ------------------------------------------------------------ run
    def run(self):
        c = self.cfg
        self.set_profile("clean")
        proxy = subprocess.Popen(
            [sys.executable, "-I", os.path.join(c["repo"], "scripts", "soak", "rpc_fault_proxy.py"),
             "--listen", str(c["proxyPort"]), "--upstream", c["anvil"], "--control", self.control,
             "--stats", os.path.join(self.work, "proxy-stats.json")],
            stdout=open(os.path.join(self.work, "proxy.log"), "a"), stderr=subprocess.STDOUT, start_new_session=True,
        )
        time.sleep(1)
        load = None
        try:
            self.start("node", "first start")
            self.start("btc", "first start")
            load = subprocess.Popen(
                [c["loadBin"], "run", "--node", f"http://127.0.0.1:{c['nodePort']}", "--btc",
                 f"http://127.0.0.1:{c['btcPort']}", "--revenue", c["revenue"], "--clients", str(c["clients"]),
                 "--log", os.path.join(self.work, "load.jsonl"), "--stop-file", self.stop_file],
                stdout=open(os.path.join(self.work, "load.log"), "a"), stderr=subprocess.STDOUT, start_new_session=True,
            )
            self.event("load_started", clients=c["clients"])
            self.loop()
        finally:
            self.drain(load)
            for p in self.nodes.values():
                p.kill()
            for st in self.pending.values():
                if st["proc"] is not None and st["proc"].poll() is None:
                    st["proc"].kill()
            proxy.terminate()
        ok = not self.failures
        with open(os.path.join(self.work, "soak-result.json"), "w") as f:
            json.dump({"pass": ok, "failures": self.failures, "minutes": round((time.time() - self.t0) / 60, 1)}, f, indent=1)
        return 0 if ok else 1

    def loop(self):
        c = self.cfg
        cycle = c["cycleMinutes"]
        end = self.t0 + c["minutes"] * 60
        leak_end = self.t0 + c["leakWindowMinutes"] * 60
        next_sample = time.time()
        done = set()
        while time.time() < end:
            now = time.time()
            m = (now - self.t0) / 60
            k = int(m // cycle)
            mc = m - k * cycle
            # The last cycle may be cut short by the end: no fault in its
            # last 14 minutes, so the end-of-run drain starts clean.
            remaining = (end - now) / 60
            prof = "clean"
            for start, name in schedule(cycle):
                if mc >= start:
                    prof = name
            if remaining < 16 and prof != "clean":
                prof = "clean"
            self.set_profile(prof)
            actions = []
            if remaining > 16:
                actions = [(5, "btc", signal.SIGTERM), (11, "btc", signal.SIGKILL)]
                if now >= leak_end:
                    actions += [(4, "node", signal.SIGTERM), (9, "node", signal.SIGKILL)]
            for at, name, sig in actions:
                key = (k, at, name)
                if mc >= at and key not in done:
                    done.add(key)
                    self.restart(name, sig)
            if mc >= cycle - 1 and (k, "check") not in done and remaining > 0.5:
                done.add((k, "check"))
                self.check_clean_window(k)
            if time.time() >= next_sample:
                self.sample()
                next_sample += 60
            for name, p in list(self.nodes.items()):
                if p.poll() is not None:
                    self.fail("node exited by itself", node=name, code=p.returncode, tail=tail(os.path.join(self.work, f"{name}.log")))
                    del self.nodes[name]
                    self.launch(name, "after an unexpected exit")
            self.tick_starts()
            time.sleep(1)

    def drain(self, load):
        c = self.cfg
        self.set_profile("clean")
        with open(self.stop_file, "w") as f:
            f.write("stop\n")
        if load is not None:
            try:
                load.wait(timeout=120)
            except subprocess.TimeoutExpired:
                load.kill()
        self.event("load_stopped")
        t = time.time()
        while self.pending and time.time() - t < 600:
            self.tick_starts()
            time.sleep(1)
        drained = False
        while time.time() - t < 1800 and self.nodes:
            s = self.sample(label="drain")
            real_final = anvil_finalized(c["anvil"])
            busy = []
            for name in list(self.nodes):
                h = (s.get(name) or {}).get("health") or {}
                if not h:
                    busy.append(f"{name}: no health")
                    continue
                for k in ("intentsOpen", "receiptsPending", "settlesInFlight", "creditsInFlight"):
                    if h.get(k):
                        busy.append(f"{name}: {k} {h[k]}")
                if h.get("indexerFailing") or (real_final and (h.get("nextBlock") or 0) + 400 < real_final):
                    busy.append(f"{name}: indexer {h.get('nextBlock')} of {real_final}")
            if not busy:
                drained = True
                break
            self.event("draining", busy=busy)
            time.sleep(60)
        self.event("drained" if drained else "drain_timeout", secs=round(time.time() - t))
        if not drained and self.nodes:
            self.fail("intents, receipts or credits did not drain within 30 min of the faults clearing")
        final = self.sample(label="final")
        self.event("final", sample=final)
        for name in list(self.nodes):
            self.stop(name, signal.SIGTERM)


# ---------------------------------------------------------------- helpers
def http_json(url, timeout, body=None):
    req = urllib.request.Request(url, data=body, headers={"Content-Type": "application/json"} if body else {})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())


def health(port, timeout):
    try:
        return http_json(f"http://127.0.0.1:{port}/v1/health", timeout)
    except Exception:
        return None


def rpc(url, method, params):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    try:
        return http_json(url, 10, body).get("result")
    except Exception:
        return None


def anvil_block(url):
    r = rpc(url, "eth_blockNumber", [])
    return int(r, 16) if r else None


def anvil_finalized(url):
    r = rpc(url, "eth_getBlockByNumber", ["finalized", False])
    return int(r["number"], 16) if r else None


def slim_health(h):
    if h is None:
        return None
    st = h.get("settles") or {}
    rep = h.get("reporter") or {}
    idx = h.get("indexer") or {}
    out = {
        "intentsOpen": st.get("intentsOpen"),
        "recovered": st.get("recovered"),
        "expiredUnused": st.get("expiredUnused"),
        "cancelled": st.get("cancelled"),
        "unknownOutcomes": st.get("unknownOutcomes"),
        "settlesInFlight": st.get("inFlight"),
        "intentWriteErrors": st.get("intentWriteErrors"),
        "clockSkewSeconds": st.get("clockSkewSeconds"),
        "receiptsPending": h.get("receiptsPending"),
        "receiptsDropped": h.get("receiptsDropped"),
        "receiptWriteErrors": h.get("receiptWriteErrors"),
        "pendingSettleWriteErrors": h.get("pendingSettleWriteErrors"),
        "acceptingPayments": h.get("acceptingPayments"),
        "creditsInFlight": rep.get("inFlight"),
        "reporterStuckSince": rep.get("stuckSince"),
        "reporterError": rep.get("lastError"),
        "nextBlock": idx.get("nextBlock"),
        "finalizedHead": idx.get("finalizedHead"),
        "servable": idx.get("servable"),
        "indexerFailing": idx.get("failing", bool(idx.get("lastError"))),
        "indexerError": idx.get("lastError"),
        "rescans": idx.get("rescans"),
        "removed": idx.get("removed"),
    }
    sc = h.get("servingChecks")
    if sc:
        out["servingChecks"] = [{k: g.get(k) for k in ("gate", "cleared", "judged", "rootReadErrors")} for g in sc]
    return out


def proc_stats(pid):
    out = {"pid": pid}
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    out["rssKb"] = int(line.split()[1])
                elif line.startswith("Threads:"):
                    out["threads"] = int(line.split()[1])
        out["fds"] = len(os.listdir(f"/proc/{pid}/fd"))
    except OSError:
        pass
    return out


def dir_sizes(d):
    out = {}
    for root, _, files in os.walk(d):
        for f in files:
            p = os.path.join(root, f)
            try:
                out[os.path.relpath(p, d)] = os.path.getsize(p)
            except OSError:
                pass
    return out


def count_lines(p):
    try:
        with open(p, "rb") as f:
            return sum(1 for _ in f)
    except OSError:
        return 0


def count_violations(p):
    try:
        with open(p, "rb") as f:
            return sum(1 for line in f if b'"VIOLATION"' in line)
    except OSError:
        return 0


def tail(p, n=6):
    try:
        with open(p, errors="replace") as f:
            return [line.rstrip()[:300] for line in f.readlines()[-n:]]
    except OSError:
        return []


def main():
    with open(sys.argv[1]) as f:
        cfg = json.load(f)
    sys.exit(Soak(cfg).run())


if __name__ == "__main__":
    main()
