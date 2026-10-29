#!/usr/bin/env python3
"""Soak summary and chain-side invariants (ROADMAP L6; run by scripts/soak.sh).

    report.py <soak dir>/soak.json      writes summary.json and summary.md

Reads samples.jsonl, events.jsonl, load.jsonl, proxy-stats.json,
soak-result.json and driver.log from the soak dir, and the anvil chain
directly (not through the fault proxy). Invariants:

  served_with_payment  no 200 on a priced route without payment (load log),
                       and every (payer, nonce) that got a paid 200 is used
                       on chain in a transaction that moved its exact value
                       from the payer to OnyxRevenue
  single_attribution   every authorization our payers used on OnyxRevenue
                       is booked (Settled or Credited) exactly once: none
                       twice (no double credit), none missing after the drain
                       (no lost fee)
  replays_identical    every identical replay that answered 200 got the same body
  soak_run             the orchestrator's own checks (soak-result.json): clean
                       SIGTERM stops, restarts healthy, no intent open and the
                       indexer caught up at the end of every clean window,
                       everything drained at the end
  indexer_final        each node's final view lists every finalized round it holds
  bounded_growth       onyx-node over its restart-free window: RSS of the last
                       30 min within 25% of the first 30 min (after a 15 min
                       warm-up), descriptors within +16, state files under 8 MiB
Python standard library only.
"""

import json
import os
import re
import statistics
import sys
import urllib.request

RSS_TOLERANCE = 1.25
FD_TOLERANCE = 16
STATE_FILE_MAX = 8 << 20
WARMUP_MIN = 15


def load_jsonl(p):
    out = []
    try:
        with open(p) as f:
            for line in f:
                line = line.strip()
                if line:
                    try:
                        out.append(json.loads(line))
                    except ValueError:
                        pass
    except OSError:
        pass
    return out


def rpc(url, method, params):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(url, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as r:
        v = json.loads(r.read())
    if v.get("error"):
        raise RuntimeError(f"{method}: {v['error']}")
    return v["result"]


def topic_addr(a):
    return "0x" + "0" * 24 + a.lower()[2:]


def pct(xs, q):
    if not xs:
        return None
    xs = sorted(xs)
    i = min(len(xs) - 1, max(0, int(round(q / 100 * (len(xs) - 1)))))
    return xs[i]


def logs(url, address, topics, frm, to):
    out = []
    step = 20000
    b = frm
    while b <= to:
        e = min(to, b + step - 1)
        out += rpc(url, "eth_getLogs", [{"address": address, "topics": topics, "fromBlock": hex(b), "toBlock": hex(e)}])
        b = e + 1
    return out


def chain_checks(cfg, load, payers):
    url = cfg["anvil"]
    t_auth, t_transfer, t_settled, t_credited, t_skipped = cfg["topics"].split(",")
    with open(os.path.join(cfg["work"], "etc", "deployment.json")) as f:
        dep = json.load(f)
    frm = int(dep["deployBlock"])
    to = int(rpc(url, "eth_blockNumber", []), 16)
    usdc, revenue = cfg["usdc"].lower(), cfg["revenue"].lower()
    used = {}  # (payer, nonce) -> tx
    paid_tx = {}  # tx -> (payer, value)
    booked = {}  # (payer, nonce) -> count
    skipped = {}
    settled = 0
    for p in payers:
        p = p.lower()
        for lg in logs(url, usdc, [t_auth, topic_addr(p)], frm, to):
            used[(p, lg["topics"][2].lower())] = lg["transactionHash"]
        for lg in logs(url, usdc, [t_transfer, topic_addr(p), topic_addr(revenue)], frm, to):
            paid_tx.setdefault(lg["transactionHash"], []).append(int(lg["data"], 16))
        for lg in logs(url, revenue, [t_settled, None, None, topic_addr(p)], frm, to):
            settled += 1
        # `settle` emits Settled and Credited; `credit` emits Credited: one
        # Credited per booking.
        for lg in logs(url, revenue, [t_credited, None, None, topic_addr(p)], frm, to):
            nonce = "0x" + lg["data"][66:130]
            booked[(p, nonce.lower())] = booked.get((p, nonce.lower()), 0) + 1
        for lg in logs(url, revenue, [t_skipped, None, None, topic_addr(p)], frm, to):
            why = int(lg["data"][66:130], 16)
            skipped[why] = skipped.get(why, 0) + 1
    served = {}
    for r in load:
        if r.get("kind") in ("paid", "retry") and r.get("st") == 200:
            served[(r["payer"].lower(), r["nonce"].lower())] = int(r.get("value") or 0)
    unproven = []
    for (p, n), value in served.items():
        tx = used.get((p, n))
        if tx is None or value not in paid_tx.get(tx, []):
            unproven.append({"payer": p, "nonce": n, "tx": tx})
    to_revenue = [k for k, tx in used.items() if tx in paid_tx]
    double = [{"payer": k[0], "nonce": k[1], "count": c} for k, c in booked.items() if c > 1]
    missing = [{"payer": k[0], "nonce": k[1], "tx": used[k]} for k in to_revenue if booked.get(k, 0) == 0]
    signed = {(r["payer"].lower(), r["nonce"].lower()) for r in load if r.get("kind") == "paid"}
    paid_unserved = [k for k in to_revenue if k not in served]
    return {
        "blocks": [frm, to],
        "servedPaid": len(served),
        "authorizationsSigned": len(signed),
        "usedOnChain": len(used),
        "usedToRevenue": len(to_revenue),
        "booked": sum(booked.values()),
        "bookedBySettle": settled,
        "unprovenServed": unproven[:20],
        "unprovenServedCount": len(unproven),
        "doubleBooked": double[:20],
        "doubleBookedCount": len(double),
        "unbooked": missing[:20],
        "unbookedCount": len(missing),
        "paidButNotServed": len(paid_unserved),
        "receiptSkipped": {str(k): v for k, v in sorted(skipped.items())},
    }


def phase_at(events):
    """A function: time -> profile."""
    changes = sorted((e["t"], e["profile"]) for e in events if e.get("event") == "profile")

    def f(t):
        prof = "clean"
        for ct, p in changes:
            if ct <= t:
                prof = p
            else:
                break
        return prof

    return f


def latency_tables(load, events):
    phase = phase_at(events)
    groups = {}
    for r in load:
        k = r.get("kind")
        if k in (None, "VIOLATION"):
            continue
        ph = "clean" if phase(r["t"] / 1000) == "clean" else "faulted"
        node = r.get("node", "?")
        g = groups.setdefault((k, node, ph), {"ms": [], "st": {}, "err": 0})
        g["ms"].append(r.get("ms", 0))
        st = str(r.get("st", "transport"))
        if r.get("code"):
            st += f" {r['code']}"
        g["st"][st] = g["st"].get(st, 0) + 1
        if "err" in r or (r.get("st", 0) >= 500):
            g["err"] += 1
    out = []
    for (k, node, ph), g in sorted(groups.items()):
        n = len(g["ms"])
        out.append({
            "kind": k, "node": node, "phase": ph, "n": n,
            "p50ms": pct(g["ms"], 50), "p99ms": pct(g["ms"], 99),
            "errorRate": round(g["err"] / n, 4) if n else None,
            "statuses": dict(sorted(g["st"].items(), key=lambda kv: -kv[1])[:6]),
        })
    return out


def series(samples, name, key, sub=None):
    xs = []
    for s in samples:
        v = (s.get(name) or {})
        if sub:
            v = v.get(sub) or {}
        x = v.get(key) if isinstance(v, dict) else None
        if x is not None:
            xs.append((s["m"], x))
    return xs


def state_total(samples, name):
    return [(s["m"], sum((s.get(name) or {}).get("state", {}).values())) for s in samples if s.get(name)]


def sme(xs):
    if not xs:
        return None
    return {"start": xs[0][1], "end": xs[-1][1], "max": max(v for _, v in xs), "min": min(v for _, v in xs)}


def growth(samples, cfg):
    """onyx-node over its restart-free window."""
    lo = WARMUP_MIN
    hi = min(cfg["leakWindowMinutes"], max((s["m"] for s in samples), default=0))
    win = [s for s in samples if lo <= s["m"] <= hi and (s.get("node") or {}).get("rssKb") and not s.get("label")]
    pids = {s["node"]["pid"] for s in win}
    if cfg["leakWindowMinutes"] - lo < 60:
        # A run configured with a short restart-free window (a confirmation
        # run) does not judge growth: not evaluated, not failed.
        return {"ok": None, "why": f"leak window {lo}-{cfg['leakWindowMinutes']} min is under an hour: not evaluated"}
    if len(win) < 20:
        return {"ok": False, "why": f"only {len(win)} samples in the window {lo}-{hi} min"}
    first = [s for s in win if s["m"] <= lo + 30]
    last = [s for s in win if s["m"] >= hi - 30]
    if not first or not last or hi - lo < 60:
        return {"ok": False, "why": f"window {lo}-{hi} min is under an hour"}
    rss0 = statistics.median(s["node"]["rssKb"] for s in first)
    rss1 = statistics.median(s["node"]["rssKb"] for s in last)
    fd0 = statistics.median(s["node"]["fds"] for s in first)
    fd1 = statistics.median(s["node"]["fds"] for s in last)
    # Least-squares slope of RSS over the window, MB per hour.
    xs = [s["m"] for s in win]
    ys = [s["node"]["rssKb"] / 1024 for s in win]
    mx, my = statistics.mean(xs), statistics.mean(ys)
    slope = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / max(1e-9, sum((x - mx) ** 2 for x in xs)) * 60
    files = {}
    for s in samples:
        for name in ("node", "btc"):
            for f, size in ((s.get(name) or {}).get("state") or {}).items():
                key = f"{name}/{f}"
                files[key] = max(files.get(key, 0), size)
    big = {k: v for k, v in files.items() if v > STATE_FILE_MAX}
    ok = len(pids) == 1 and rss1 <= rss0 * RSS_TOLERANCE and fd1 <= fd0 + FD_TOLERANCE and not big
    return {
        "ok": ok, "window": [lo, hi], "samples": len(win), "pids": len(pids),
        "rssMedianFirst30MiB": round(rss0 / 1024, 1), "rssMedianLast30MiB": round(rss1 / 1024, 1),
        "rssSlopeMiBPerHour": round(slope, 2),
        "fdMedianFirst30": fd0, "fdMedianLast30": fd1,
        "stateFileMaxBytes": files, "tooBig": big,
    }


def main():
    with open(sys.argv[1]) as f:
        cfg = json.load(f)
    w = cfg["work"]
    samples = load_jsonl(os.path.join(w, "samples.jsonl"))
    events = load_jsonl(os.path.join(w, "events.jsonl"))
    load = load_jsonl(os.path.join(w, "load.jsonl"))
    payers = open(os.path.join(w, "payers.txt")).read().split()
    try:
        result = json.load(open(os.path.join(w, "soak-result.json")))
    except (OSError, ValueError):
        result = {"pass": False, "failures": [{"what": "no soak-result.json"}]}
    try:
        proxy = json.load(open(os.path.join(w, "proxy-stats.json")))
    except (OSError, ValueError):
        proxy = {}
    driver = open(os.path.join(w, "driver.log"), errors="replace").read()
    rounds_final = len(re.findall(r"^soak round \d+ .*finalized at block", driver, re.M))
    rounds_skipped = len(re.findall(r"^soak round \d+ .*skipped", driver, re.M))

    chain = chain_checks(cfg, load, payers)
    violations = [r for r in load if r.get("kind") == "VIOLATION"]
    replays = [r for r in load if r.get("kind") == "replay" and r.get("st") == 200]
    replay_diff = [r for r in replays if not r.get("same")]

    restarts = [e for e in events if e.get("event") == "stopped"]
    starts = [e for e in events if e.get("event") == "started"]
    start_failed = [e for e in events if e.get("event") == "start_failed"]

    final = next((e["sample"] for e in reversed(events) if e.get("event") == "final"), samples[-1] if samples else {})
    servable = {n: ((final.get(n) or {}).get("health") or {}).get("servable") for n in ("node", "btc")}
    # Held: round 1, the reserve shard and every soak round that finalized.
    expected = 2 + rounds_final

    resources = {}
    for n in ("node", "btc"):
        resources[n] = {
            "rssMiB": sme([(m, round(v / 1024, 1)) for m, v in series(samples, n, "rssKb")]),
            "fds": sme(series(samples, n, "fds")),
            "threads": sme(series(samples, n, "threads")),
            "stateBytes": sme(state_total(samples, n)),
        }
    grow = growth(samples, cfg)
    inv = {
        "served_with_payment": not violations and chain["unprovenServedCount"] == 0,
        "single_attribution": chain["doubleBookedCount"] == 0 and chain["unbookedCount"] == 0,
        "replays_identical": not replay_diff,
        "soak_run": bool(result.get("pass")),
        "indexer_final": all(servable[n] == expected for n in ("node", "btc")),
        "bounded_growth": grow.get("ok"),
    }
    duration = round((samples[-1]["t"] - samples[0]["t"]) / 60, 1) if len(samples) > 1 else 0
    summary = {
        # None: not evaluated in this run (bounded_growth with a short window).
        "pass": all(v is not False for v in inv.values()),
        "invariants": inv,
        "durationMinutes": duration,
        "loadRequests": len(load),
        "violations": violations[:20],
        "replays200": len(replays),
        "replaysDifferent": len(replay_diff),
        "chain": chain,
        "rounds": {"finalized": rounds_final, "skipped": rounds_skipped, "expectedServable": expected, "servable": servable},
        "restarts": {
            "stops": [{k: e.get(k) for k in ("node", "signal", "code", "secs", "m")} for e in restarts],
            "starts": len(starts),
            "startFailures": len(start_failed),
            "maxStartSecs": max((e["secs"] for e in starts), default=None),
            "maxStopSecs": max((e["secs"] for e in restarts if e.get("signal") == "SIGTERM"), default=None),
        },
        "resources": resources,
        "growth": grow,
        "faults": {
            "requests": proxy.get("requests"),
            "byType": {k: v for k, v in sorted(proxy.get("faults", {}).items()) if ":" not in k},
            "profileSeconds": proxy.get("profileSeconds"),
            "upstreamErrors": proxy.get("upstreamErrors"),
        },
        "latency": latency_tables(load, events),
        "orchestratorFailures": result.get("failures"),
    }
    with open(os.path.join(w, "summary.json"), "w") as f:
        json.dump(summary, f, indent=1)
    print(json.dumps({k: summary[k] for k in ("pass", "invariants", "durationMinutes", "loadRequests", "rounds")}, indent=1))
    print(json.dumps(summary["chain"], indent=1)[:3000])
    print(json.dumps(summary["growth"], indent=1)[:3000])
    sys.exit(0 if summary["pass"] else 1)


if __name__ == "__main__":
    main()
