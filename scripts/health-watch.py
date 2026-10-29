#!/usr/bin/env python3
# Onyx health watch (ROADMAP L12, launch checklist LC-17). Python standard library only.
#
# Polls, read-only (no transaction, no key):
#   node      onyx-node GET /v1/health: up, chain id, indexer lag (against the watch's own RPC
#             finalized head too), conflicts, dropped and evicted candidates, failing, open and
#             unknown settle intents, every write-error counter, dropped receipts, clock skew,
#             payments refused, reporter stuck or erroring; level 3 (ROADMAP C62): the post log's
#             oldest pending post before the 600 s breaker trips (THREATS C10-3), pending, new
#             refused and retrying posts, and level 3 stopped;
#   rpc       the chain node: chain id, head age, finalized lag;
#   balances  deployer ETH, the API operator's ETH (its settles and receipts; skipped while the
#             node reports payments off), OnyxRevenue: USDC.balanceOf(revenue) >= allocated() -
#             paidOut(), and revenue.token() is the configured USDC;
#   site      the public website (when --site is given): / answers, data/demo.json is a public
#             build.
# Thresholds: deploy/health-watch/thresholds.json (the only copy; --config overlays more files).
# Output: one JSON line per check on stdout. Exit 0 when nothing alerts, 1 when any check alerts,
# 2 on a usage, config or lock error. A check that cannot be read (unreachable, timeout, malformed
# answer) fails: it never passes silently.
# Alerts are de-duplicated with a state file: the notifier hears a new alert once, again after
# notify.realert_after_s, and a recovery once. The default notifier appends each event to
# --alert-log (default <state dir>/alerts.jsonl) and prints it on stderr; --notify-cmd runs a
# command per event with the event JSON on stdin (an alert channel, LC-18). A failed command
# leaves the event unsent and it is retried on the next run.
# The RPC URL may carry a provider API key: pass it with --rpc-file or ONYX_WATCH_RPC_URL; output
# shows only scheme://host[:port].
#
# Usage: scripts/health-watch.py --once --node http://127.0.0.1:8080 --rpc-file ~/.config/onyx/rpc-url \
#          [--site https://example.org] [--state ~/.local/state/onyx-watch/state.json] \
#          [--notify-cmd '/usr/local/bin/my-alert'] [--config overlay.json]
#        scripts/health-watch.py --interval 60 ...      (loop instead of --once)
#        scripts/health-watch.py --print-config [--config overlay.json]
# Docs: docs/ops/runbooks/health-watch.md. Tests: scripts/health-watch-tests.py (in check-all).
import argparse
import fcntl
import json
import os
import re
import shlex
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CONFIG = ROOT / "deploy" / "health-watch" / "thresholds.json"
MAX_BODY = 4 << 20
STATE_VERSION = 1

# The config's shape: a dict is a section, a type (or tuple of types) a leaf. Keys starting with
# "_" are comments. The "after" section takes check names.
INT = int
SCHEMA = {
    "chain_id": INT,
    "deadline_s": INT,
    "require": {"node": bool, "rpc": bool, "site": bool},
    "addresses": {"deployer": (str, type(None)), "operator": (str, type(None)), "revenue": (str, type(None)),
                  "usdc": (str, type(None))},
    "node": {
        "timeout_s": (int, float),
        "indexer_required": bool,
        "indexer_lag_max_blocks": INT,
        "indexer_ahead_max_blocks": INT,
        "conflicts_max": INT,
        "candidates_dropped_max": INT,
        "candidates_evicted_max": INT,
        "write_errors_max": INT,
        "receipts_dropped_max": INT,
        "intents_open_max": INT,
        "intents_open_for_max_s": INT,
        "unknown_outcomes_new_max": INT,
        "clock_skew_max_s": INT,
        "reporter_stuck_max_s": INT,
        "level3_required": bool,
        "level3_pending_for_max_s": INT,
        "level3_pending_max": INT,
        "level3_refused_new_max": INT,
        "level3_retrying_max": INT,
    },
    "rpc": {"timeout_s": (int, float), "tries": INT, "head_age_max_s": INT, "finalized_lag_max_blocks": INT},
    "balances": {"deployer_wei_min": str, "operator_wei_min": str},
    "site": {"timeout_s": (int, float)},
    "after": "checks",
    "notify": {"realert_after_s": INT, "command_timeout_s": (int, float)},
}

# Every check this watch can print (the "after" section may only name these).
WRITE_ERRORS = [
    ("node.receipt_write_errors", ("receiptWriteErrors",)),
    ("node.pending_settle_write_errors", ("pendingSettleWriteErrors",)),
    ("node.intent_write_errors", ("settles", "intentWriteErrors")),
    ("node.indexer.state_write_errors", ("indexer", "stateWriteErrors")),
    ("node.reporter.write_errors", ("reporter", "writeErrors")),
]
CHECKS = {
    "node.up", "node.health", "node.chain", "node.payments",
    "node.indexer", "node.indexer.lag", "node.indexer.conflicts", "node.indexer.candidates_dropped",
    "node.indexer.candidates_evicted", "node.indexer.failing",
    "node.intents.open", "node.intents.open_for", "node.intents.unknown_outcomes",
    "node.receipts_dropped", "node.clock_skew", "node.reporter.stuck", "node.reporter.last_error",
    "node.level3", "node.level3.stopped", "node.level3.breaker", "node.level3.pending_for",
    "node.level3.pending", "node.level3.refused_new", "node.level3.retrying",
    "rpc.up", "rpc.chain", "rpc.head_age", "rpc.finalized_lag",
    "balances.deployer_eth", "balances.operator_eth", "balances.revenue",
    "site.up", "site.public",
    "watch.state",
} | {name for name, _ in WRITE_ERRORS}


# ROADMAP C62 (THREATS C10-3): onyx-node stops every level-3 answer once a post has been pending
# more than LEVEL3_BREAKER_S (signed_serve::POST_DELAY_ALERT_SECS). The watch runs every
# WATCH_INTERVAL_S (deploy/systemd/user/onyx-health-watch.timer, AccuracySec WATCH_ACCURACY_S), and
# a run may take up to deadline_s, during which the timer cannot start the next one. Review C61-R2-1:
# two reads of /v1/health can therefore be up to WATCH_INTERVAL_S + WATCH_ACCURACY_S + deadline_s
# apart (155 s with the shipped config, not 60), so the first alert can come that late after the
# threshold; one more run must still fit before the breaker:
# node.level3_pending_for_max_s + level3_need(deadline_s) <= the breaker.
LEVEL3_BREAKER_S = 600
WATCH_INTERVAL_S = 60
WATCH_ACCURACY_S = 5


def level3_need(deadline_s):
    """Seconds the level-3 breaker must leave after the watch's threshold: the longest gap between two
    runs (a slow run of deadline_s plus the timer's interval and accuracy), then one more run."""
    return deadline_s + WATCH_ACCURACY_S + 2 * WATCH_INTERVAL_S
LEVEL3_COUNT_CHECKS = ("node.level3.pending_for", "node.level3.pending", "node.level3.refused_new",
                       "node.level3.retrying")


class ConfigError(Exception):
    pass


class DeadlineHit(Exception):
    """The run's overall budget (deadline_s) ran out: every check not done yet alerts."""


# Monotonic end of the current run's budget (run_once sets it; None outside a run).
_DEADLINE = [None]

# The clock and the transport every wait goes through (ROADMAP E12). Production uses the real ones;
# scripts/health-watch-tests.py swaps in a virtual clock and a transport that spends a fake
# server's delays on that clock, so the deadline tests hold on a loaded machine.
_clock = time.monotonic
_sleep = time.sleep


def _urlopen(req, timeout):
    return urllib.request.urlopen(req, timeout=timeout)


def bounded(timeout):
    """`timeout` cut to what is left of the run's budget; DeadlineHit when nothing is left."""
    if _DEADLINE[0] is None:
        return timeout, False
    left = _DEADLINE[0] - _clock()
    if left <= 0.05:
        raise DeadlineHit()
    return (left, True) if left < timeout else (timeout, False)


class Fault(Exception):
    """A read failed: `down` for unreachable/timeout/HTTP error, else a malformed answer."""

    def __init__(self, msg, down=False):
        super().__init__(msg)
        self.down = down


# ---------------------------------------------------------------- config

def merge(base, over):
    out = dict(base)
    for k, v in over.items():
        out[k] = merge(out[k], v) if isinstance(v, dict) and isinstance(out.get(k), dict) else v
    return out


def validate(cfg, schema=SCHEMA, where=""):
    for k, v in cfg.items():
        if k.startswith("_"):
            continue
        if k not in schema:
            raise ConfigError(f"unknown key {where}{k}")
    for k, want in schema.items():
        if k not in cfg:
            raise ConfigError(f"missing key {where}{k}")
        v = cfg[k]
        if want == "checks":
            if not isinstance(v, dict):
                raise ConfigError(f"{where}{k} must be an object")
            for name, n in v.items():
                if name.startswith("_"):
                    continue
                if name not in CHECKS:
                    raise ConfigError(f"{where}{k}: unknown check {name}")
                if type(n) is not int or n < 1:
                    raise ConfigError(f"{where}{k}.{name} must be an integer >= 1")
        elif isinstance(want, dict):
            if not isinstance(v, dict):
                raise ConfigError(f"{where}{k} must be an object")
            validate(v, want, f"{where}{k}.")
        else:
            types = want if isinstance(want, tuple) else (want,)
            if isinstance(v, bool) and bool not in types:
                raise ConfigError(f"{where}{k} must be {'/'.join(t.__name__ for t in types)}")
            if not isinstance(v, types):
                raise ConfigError(f"{where}{k} must be {'/'.join(t.__name__ for t in types)}")
            if isinstance(v, (int, float)) and not isinstance(v, bool) and v < 0:
                raise ConfigError(f"{where}{k} must not be negative")
    if not where:
        for name in ("deployer", "operator", "revenue", "usdc"):
            a = cfg["addresses"][name]
            if a is not None and not is_address(a):
                raise ConfigError(f"addresses.{name} is not a 0x address: {a!r}")
        for key in ("deployer_wei_min", "operator_wei_min"):
            try:
                int(cfg["balances"][key], 10)
            except ValueError:
                raise ConfigError(f"balances.{key} must be a decimal integer string") from None
        if cfg["rpc"]["tries"] < 1:
            raise ConfigError("rpc.tries must be at least 1")
        if cfg["deadline_s"] < 1:
            raise ConfigError("deadline_s must be at least 1")
        room = LEVEL3_BREAKER_S - level3_need(cfg["deadline_s"])
        if cfg["node"]["level3_pending_for_max_s"] > room:
            raise ConfigError(f"node.level3_pending_for_max_s must be at most {room}: the {LEVEL3_BREAKER_S} s "
                              f"level-3 breaker less the longest gap between two watch runs (deadline_s "
                              f"{cfg['deadline_s']} + {WATCH_INTERVAL_S} + {WATCH_ACCURACY_S} s) and one more "
                              f"{WATCH_INTERVAL_S} s run (THREATS C10-3, C61-R2-1)")


def is_address(a):
    return isinstance(a, str) and len(a) == 42 and a[:2] == "0x" and all(c in "0123456789abcdefABCDEF" for c in a[2:])


def load_config(paths):
    cfg = {}
    for p in [DEFAULT_CONFIG, *paths]:
        try:
            with open(p, encoding="utf-8") as f:
                part = json.load(f)
        except (OSError, ValueError) as e:
            raise ConfigError(f"{p}: {e}") from None
        if not isinstance(part, dict):
            raise ConfigError(f"{p}: not a JSON object")
        cfg = merge(cfg, part)
    validate(cfg)
    return cfg


# ---------------------------------------------------------------- transport

def origin(url):
    """scheme://host[:port] only: a provider URL can carry an API key in its path or query."""
    try:
        p = urllib.parse.urlsplit(url)
        host = p.hostname or "?"
        return f"{p.scheme}://{host}" + (f":{p.port}" if p.port else "")
    except ValueError:
        return "<unparsable url>"


def why_down(e):
    if isinstance(e, urllib.error.HTTPError):
        return f"HTTP {e.code}"
    reason = getattr(e, "reason", e)
    if isinstance(reason, (socket.timeout, TimeoutError)) or "timed out" in str(reason):
        return "timed out"
    if isinstance(reason, ConnectionRefusedError):
        return "connection refused"
    if isinstance(reason, OSError) and reason.strerror:
        return reason.strerror
    return type(reason).__name__


def http(url, timeout, data=None, headers=None):
    """GET (or POST `data`); returns (status, body bytes). Unreachable or timeout: Fault(down)."""
    req = urllib.request.Request(url, data, {"user-agent": "onyx-health-watch", **(headers or {})})
    timeout, capped = bounded(timeout)
    try:
        with _urlopen(req, timeout) as r:
            return r.status, r.read(MAX_BODY + 1)
    except urllib.error.HTTPError as e:
        try:
            body = e.read(MAX_BODY + 1)
        except Exception:  # noqa: BLE001 - a broken error body changes nothing
            body = b""
        return e.code, body
    except (urllib.error.URLError, OSError, ValueError) as e:
        why = why_down(e)
        if capped and why == "timed out":
            raise DeadlineHit() from None
        raise Fault(why, down=True) from None
    except Exception as e:  # noqa: BLE001 - http.client errors (bad status line, reset)
        raise Fault(f"{type(e).__name__}", down=True) from None


def get_json(url, timeout):
    status, body = http(url, timeout)
    if status != 200:
        raise Fault(f"HTTP {status}", down=True)
    if len(body) > MAX_BODY:
        raise Fault("answer larger than 4 MiB")
    try:
        return json.loads(body)
    except ValueError:
        raise Fault("answer is not JSON") from None


class Rpc:
    def __init__(self, url, timeout, tries, backoff=0.5):
        self.url, self.timeout, self.tries, self.backoff, self.n = url, timeout, tries, backoff, 0

    def call(self, method, params):
        last = None
        for attempt in range(self.tries):
            if attempt:
                _sleep(min(self.backoff, bounded(self.backoff)[0]))
            self.n += 1
            body = json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}).encode()
            try:
                status, raw = http(self.url, self.timeout, body, {"content-type": "application/json"})
                if status != 200:
                    raise Fault(f"{method}: HTTP {status}", down=True)
            except Fault as e:
                last = Fault(f"{method}: {e}" if not str(e).startswith(method) else str(e), down=True)
                continue
            try:
                v = json.loads(raw)
            except ValueError:
                raise Fault(f"{method}: answer is not JSON") from None
            if not isinstance(v, dict) or v.get("id") != self.n:
                raise Fault(f"{method}: answer is not a JSON-RPC reply to this request")
            if v.get("error") is not None:
                err = v["error"]
                msg = err.get("message") if isinstance(err, dict) else err
                raise Fault(f"{method}: node error {str(msg)[:120]}")
            if "result" not in v:
                raise Fault(f"{method}: reply has no result")
            return v["result"]
        raise last


def hexint(v, what):
    if not isinstance(v, str) or not v.startswith("0x") or len(v) < 3 or len(v) > 66:
        raise Fault(f"{what}: not a hex quantity: {str(v)[:80]}")
    try:
        return int(v[2:], 16)
    except ValueError:
        raise Fault(f"{what}: not a hex quantity: {v[:80]}") from None


def word(v, what):
    """One ABI word from eth_call (uint256 or address)."""
    if not isinstance(v, str) or not v.startswith("0x") or len(v) != 66:
        raise Fault(f"{what}: not one 32-byte word (no contract at the address?): {str(v)[:80]}")
    try:
        return int(v[2:], 16)
    except ValueError:
        raise Fault(f"{what}: not hex: {v[:80]}") from None


# ---------------------------------------------------------------- results

def ok(check, value=None, limit=None, detail=""):
    return {"check": check, "fault": False, "value": value, "limit": limit, "detail": detail}


def bad(check, value=None, limit=None, detail=""):
    return {"check": check, "fault": True, "value": value, "limit": limit, "detail": detail}


def skip(check, detail):
    return {"check": check, "fault": False, "skip": True, "value": None, "limit": None, "detail": detail}


def at_most(check, value, limit, what):
    if value > limit:
        return bad(check, value, limit, f"{what} {value} above {limit}")
    return ok(check, value, limit)


def field(d, path, kinds, label="/v1/health"):
    cur = d
    for k in path:
        if not isinstance(cur, dict) or k not in cur:
            raise Fault(f"malformed {label}: missing {'.'.join(path)}")
        cur = cur[k]
    want = kinds if isinstance(kinds, tuple) else (kinds,)
    if isinstance(cur, bool) and bool not in want:
        raise Fault(f"malformed {label}: {'.'.join(path)} is {type(cur).__name__}")
    if not isinstance(cur, want):
        raise Fault(f"malformed {label}: {'.'.join(path)} is {type(cur).__name__}")
    if isinstance(cur, int) and not isinstance(cur, bool) and int in want and cur < 0 and path[-1] != "clockSkewSeconds":
        raise Fault(f"malformed {label}: {'.'.join(path)} is negative")
    return cur


def guarded(check, fn):
    """Run one check; a malformed answer is an alert, never a pass."""
    try:
        return fn()
    except Fault as e:
        return {**bad(check, detail=str(e)), "malformed": not e.down}


# ---------------------------------------------------------------- checks

def rpc_checks(cfg, rpc, now, out):
    """Appends to `out`; returns the chain facts, or None when the RPC is down or wrong."""
    c = cfg["rpc"]
    try:
        chain = hexint(rpc.call("eth_chainId", []), "eth_chainId")
    except Fault as e:
        out.extend([bad("rpc.up", detail=str(e))] if e.down else [ok("rpc.up"), bad("rpc.chain", detail=str(e))])
        return None
    out.append(ok("rpc.up", detail=origin(rpc.url)))
    if chain != cfg["chain_id"]:
        out.append(bad("rpc.chain", chain, cfg["chain_id"], f"chain id {chain}, expected {cfg['chain_id']}"))
        return None
    out.append(ok("rpc.chain", chain, cfg["chain_id"]))
    facts = {"chain": chain}

    def blocks():
        latest = rpc.call("eth_getBlockByNumber", ["latest", False])
        fin = rpc.call("eth_getBlockByNumber", ["finalized", False])
        if not isinstance(latest, dict) or not isinstance(fin, dict):
            raise Fault("eth_getBlockByNumber: no block object")
        return (hexint(latest.get("number"), "latest.number"), hexint(latest.get("timestamp"), "latest.timestamp"),
                hexint(fin.get("number"), "finalized.number"))

    try:
        head, ts, final = blocks()
    except Fault as e:
        if e.down:
            out[out.index(next(r for r in out if r["check"] == "rpc.up"))] = bad("rpc.up", detail=str(e))
            return None
        out.append(bad("rpc.head_age", detail=str(e)))
        out.append(bad("rpc.finalized_lag", detail=str(e)))
        return facts
    age = now - ts
    if age < -c["head_age_max_s"]:
        out.append(bad("rpc.head_age", age, c["head_age_max_s"], f"head block {-age} s in the future: this box's clock is behind"))
    else:
        out.append(at_most("rpc.head_age", age, c["head_age_max_s"], "head block age (s)"))
    lag = head - final
    if lag < 0:
        out.append(bad("rpc.finalized_lag", lag, c["finalized_lag_max_blocks"], f"finalized {final} above latest {head}"))
    else:
        out.append(at_most("rpc.finalized_lag", lag, c["finalized_lag_max_blocks"], "finalized lag (blocks)"))
    facts.update(head=head, finalized=final)
    return facts


def call_word(rpc, to, data, what):
    return word(rpc.call("eth_call", [{"to": to, "data": data}, "latest"]), what)


SEL_ALLOCATED = "0xb304b2e1"  # allocated()
SEL_PAID_OUT = "0x5c76ca2d"  # paidOut()
SEL_TOKEN = "0xfc0c546a"  # token()
SEL_BALANCE_OF = "0x70a08231"  # balanceOf(address)


def gas_check(cfg, rpc, who):
    """`who`: "deployer" or "operator": its ETH against balances.<who>_wei_min."""
    check, addr = f"balances.{who}_eth", cfg["addresses"][who]
    if addr is None:
        return skip(check, f"addresses.{who} not set")

    def eth():
        wei = hexint(rpc.call("eth_getBalance", [addr, "latest"]), "eth_getBalance")
        need = int(cfg["balances"][f"{who}_wei_min"])
        if wei < need:
            return bad(check, str(wei), str(need), f"{who} {addr} holds {wei} wei, below {need}")
        return ok(check, str(wei), str(need))
    return guarded(check, eth)


def balance_checks(cfg, rpc, out):
    a = cfg["addresses"]
    out.append(gas_check(cfg, rpc, "deployer"))
    # ROADMAP L19 (THREATS NS17-4): the operator pays gas for every self-settle and receipt post; an
    # empty operator turns paid queries into 503s. gather() makes this a skip line while the node
    # reports payments off (no key on the server, nothing to send).
    out.append(gas_check(cfg, rpc, "operator"))
    rev, usdc = a["revenue"], a["usdc"]
    if rev is None or usdc is None:
        out.append(skip("balances.revenue", "addresses.revenue or addresses.usdc not set"))
        return

    def revenue():
        token = call_word(rpc, rev, SEL_TOKEN, "revenue.token()")
        if token != int(usdc, 16):
            return bad("balances.revenue", detail=f"revenue.token() is 0x{token:040x}, config says {usdc.lower()}")
        allocated = call_word(rpc, rev, SEL_ALLOCATED, "revenue.allocated()")
        paid = call_word(rpc, rev, SEL_PAID_OUT, "revenue.paidOut()")
        held = call_word(rpc, usdc, SEL_BALANCE_OF + "0" * 24 + rev[2:].lower(), "usdc.balanceOf(revenue)")
        if paid > allocated:
            return bad("balances.revenue", str(held), None, f"paidOut {paid} above allocated {allocated}")
        owed = allocated - paid
        if held < owed:
            return bad("balances.revenue", str(held), str(owed), f"OnyxRevenue holds {held}, owes {owed} (allocated {allocated} - paidOut {paid})")
        return ok("balances.revenue", str(held), str(owed), f"allocated {allocated} - paidOut {paid}")
    out.append(guarded("balances.revenue", revenue))


def node_checks(cfg, h, now, facts, mem):
    """`h`: the parsed /v1/health body. `facts`: the RPC's chain facts (or None). `mem`: state."""
    n = cfg["node"]
    if not isinstance(h, dict):
        return [bad("node.health", detail=f"malformed /v1/health: a JSON {type(h).__name__}, not an object")]
    out = []
    status = h.get("status")
    out.append(ok("node.health") if status == "ok" else bad("node.health", status, "ok", f"status is {status!r}"))

    def chain():
        cid = h.get("chainId", "missing")
        if cid == "missing":
            raise Fault("malformed /v1/health: missing chainId")
        if cid != cfg["chain_id"]:
            return bad("node.chain", cid, cfg["chain_id"], f"node serves chain {cid}, expected {cfg['chain_id']}")
        return ok("node.chain", cid, cfg["chain_id"])
    out.append(guarded("node.chain", chain))

    def payments():
        pay = h.get("payments", "missing")
        if pay == "missing":
            raise Fault("malformed /v1/health: missing payments")
        accepting = field(h, ("acceptingPayments",), bool)
        if pay is not None and not accepting:
            return bad("node.payments", False, True, "payments are on but refused (a journal write failed or the receipt log is full)")
        return ok("node.payments", accepting if pay is not None else None, detail="payments off" if pay is None else "")
    out.append(guarded("node.payments", payments))

    # Indexer (ADR-0033).
    idx = h.get("indexer", "missing")
    if idx == "missing":
        out.append(bad("node.indexer", detail="malformed /v1/health: missing indexer"))
    elif idx is None:
        out.append(bad("node.indexer", detail="indexer off (ONYX_INDEXER)") if n["indexer_required"] else skip("node.indexer", "indexer off, not required"))
    elif not isinstance(idx, dict):
        out.append(bad("node.indexer", detail="malformed /v1/health: indexer is not an object"))
    else:
        out.append(ok("node.indexer"))

        def lag():
            nxt = field(h, ("indexer", "nextBlock"), int)
            head = field(h, ("indexer", "finalizedHead"), int)
            target = max(head, facts["finalized"]) if facts and "finalized" in facts else head
            ahead = (nxt - 1) - target
            if ahead > n["indexer_ahead_max_blocks"]:
                return bad("node.indexer.lag", -ahead, n["indexer_lag_max_blocks"],
                           f"indexer cursor {nxt - 1} is {ahead} blocks past the finalized head {target} "
                           f"(more than {n['indexer_ahead_max_blocks']}): a lying node or a state file from another chain")
            behind = max(0, target - (nxt - 1))
            return at_most("node.indexer.lag", behind, n["indexer_lag_max_blocks"], f"indexer behind the finalized head {target} by (blocks)")
        out.append(guarded("node.indexer.lag", lag))
        for check, key, limit in (("node.indexer.conflicts", "conflicts", "conflicts_max"),
                                  ("node.indexer.candidates_dropped", "candidatesDropped", "candidates_dropped_max"),
                                  ("node.indexer.candidates_evicted", "candidatesEvicted", "candidates_evicted_max")):
            out.append(guarded(check, lambda c=check, k=key, l=limit: at_most(c, field(h, ("indexer", k), int), n[l], k)))

        def failing():
            if "failing" in idx:
                f = field(h, ("indexer", "failing"), bool)
            elif "lastError" in idx:
                f = idx["lastError"] is not None
            else:
                raise Fault("malformed /v1/health: indexer has neither failing nor lastError")
            return bad("node.indexer.failing", True, False, "the indexer's last poll failed (node log has the error)") if f else ok("node.indexer.failing", False)
        out.append(guarded("node.indexer.failing", failing))

    # Settle intents (OPS-L2-F1).
    def intents_open():
        o = field(h, ("settles", "intentsOpen"), int)
        if o > 0:
            mem.setdefault("intents_open_since", now)
        else:
            mem.pop("intents_open_since", None)
        return at_most("node.intents.open", o, n["intents_open_max"], "open settle intents")
    out.append(guarded("node.intents.open", intents_open))

    def open_for():
        o = field(h, ("settles", "intentsOpen"), int)
        if o == 0:
            return ok("node.intents.open_for", 0, n["intents_open_for_max_s"])
        since = mem.get("intents_open_since", now)
        return at_most("node.intents.open_for", now - since, n["intents_open_for_max_s"], "intents open without a break for (s)")
    out.append(guarded("node.intents.open_for", open_for))

    def unknown():
        u = field(h, ("settles", "unknownOutcomes"), int)
        last = mem.get("unknown_outcomes")
        mem["unknown_outcomes"] = u
        new = 0 if last is None else (u - last if u >= last else u)
        return at_most("node.intents.unknown_outcomes", new, n["unknown_outcomes_new_max"], f"settles with unknown outcome since the last run (total {u}):")
    out.append(guarded("node.intents.unknown_outcomes", unknown))

    for check, path in WRITE_ERRORS:
        if path[0] == "reporter" and h.get("reporter", "missing") is None:
            out.append(ok(check, detail="no reporter"))
            continue
        # Indexer off: node.indexer already alerts (or skips) for it; one cause, one alert (NL20-2).
        if path[0] == "indexer" and h.get("indexer", "missing") is None:
            out.append(ok(check, detail="indexer off"))
            continue
        out.append(guarded(check, lambda c=check, p=path: at_most(c, field(h, p, int), n["write_errors_max"], ".".join(p))))
    out.append(guarded("node.receipts_dropped", lambda: at_most("node.receipts_dropped", field(h, ("receiptsDropped",), int), n["receipts_dropped_max"], "receiptsDropped")))

    def skew():
        s = field(h, ("settles", "clockSkewSeconds"), (int, type(None)))
        if s is None:
            return ok("node.clock_skew", None, n["clock_skew_max_s"], "not measured yet")
        return at_most("node.clock_skew", abs(s), n["clock_skew_max_s"], "clock skew to the chain (s)")
    out.append(guarded("node.clock_skew", skew))

    rep = h.get("reporter", "missing")
    if rep == "missing":
        out.append(bad("node.reporter.stuck", detail="malformed /v1/health: missing reporter"))
    elif rep is None:
        out.append(ok("node.reporter.stuck", detail="no reporter"))
        out.append(ok("node.reporter.last_error", detail="no reporter"))
    else:
        def stuck():
            s = field(h, ("reporter", "stuckSince"), (int, type(None)))
            if s is None:
                return ok("node.reporter.stuck", None, n["reporter_stuck_max_s"])
            if s - now > n["clock_skew_max_s"]:
                return bad("node.reporter.stuck", now - s, n["reporter_stuck_max_s"],
                           f"reporter stuckSince {s} is {s - now} s in the future: the node's clock or this box's is wrong")
            return at_most("node.reporter.stuck", max(0, now - s), n["reporter_stuck_max_s"], f"reporter credit unmined since {s}, for (s)")
        out.append(guarded("node.reporter.stuck", stuck))

        def last_error():
            e = field(h, ("reporter", "lastError"), (str, type(None)))
            return bad("node.reporter.last_error", e[:200], None, "reporter's last post failed") if e is not None else ok("node.reporter.last_error")
        out.append(guarded("node.reporter.last_error", last_error))

    # Level 3 (ROADMAP C62, THREATS C10-3): the post log the level-3 store serves from, as the node
    # last read it. Its seconds are the node's own clock, the one its breaker runs on.
    l3 = h.get("level3", "missing")
    if l3 == "missing" and n["level3_required"]:
        out.append(bad("node.level3", detail="malformed /v1/health: missing level3"))
    elif l3 == "missing":
        # Review C61-R1-4: a node from before ROADMAP C62 has no level3 key; not required, no page.
        out.append(skip("node.level3", "no level3 on /v1/health (a node before ROADMAP C62), not required"))
    elif l3 is None:
        out.append(bad("node.level3", detail="no level-3 store (ONYX_SIGNED_REPORTS)") if n["level3_required"]
                   else skip("node.level3", "no level-3 store, not required"))
    elif not isinstance(l3, dict):
        out.append(bad("node.level3", detail="malformed /v1/health: level3 is not an object"))
    else:
        out.append(ok("node.level3"))

        def stopped():
            st = field(h, ("level3", "stopped"), (str, type(None)))
            if st is not None:
                return bad("node.level3.stopped", st[:80], None, f"level 3 is stopped ({st[:80]}): no level-3 fact is served")
            return ok("node.level3.stopped")
        out.append(guarded("node.level3.stopped", stopped))

        def breaker():
            b = field(h, ("level3", "breakerSecs"), int)
            room, need = b - n["level3_pending_for_max_s"], level3_need(cfg["deadline_s"])
            if room < need:
                return bad("node.level3.breaker", room, need,
                           f"the node's breaker trips at {b} s: an alert at {n['level3_pending_for_max_s']} s "
                           f"leaves {room} s, less than the longest gap between two watch runs and one more "
                           f"run ({need} s)")
            return ok("node.level3.breaker", room, need)
        out.append(guarded("node.level3.breaker", breaker))

        try:
            readable = field(h, ("level3", "logReadable"), bool)
        except Fault as e:
            readable = None
            for c in LEVEL3_COUNT_CHECKS:
                out.append({**bad(c, detail=str(e)), "malformed": True})
        if readable is False:
            for c in LEVEL3_COUNT_CHECKS:
                out.append(bad(c, detail="the level-3 post log is unreadable: counts unknown"))
        elif readable:
            def pending_for():
                s = field(h, ("level3", "oldestPendingSecs"), (int, type(None)))
                p = field(h, ("level3", "postsPending"), int)
                if s is None:
                    if p:
                        raise Fault(f"malformed /v1/health: level3.postsPending is {p} with no oldestPendingSecs")
                    return ok("node.level3.pending_for", None, n["level3_pending_for_max_s"], "nothing pending")
                if not p:
                    raise Fault("malformed /v1/health: level3.oldestPendingSecs with postsPending 0")
                return at_most("node.level3.pending_for", s, n["level3_pending_for_max_s"],
                               f"oldest level-3 post pending (breaker at {LEVEL3_BREAKER_S} s) for (s)")
            out.append(guarded("node.level3.pending_for", pending_for))
            out.append(guarded("node.level3.pending", lambda: at_most(
                "node.level3.pending", field(h, ("level3", "postsPending"), int), n["level3_pending_max"], "level-3 posts pending")))

            def refused_new():
                # Review C61-R1-2: refusals that never clear only (postsRefusedFinal); one that may
                # clear is retried, can flip back to pending, and is node.level3.retrying's.
                r = field(h, ("level3", "postsRefusedFinal"), int)
                total = field(h, ("level3", "postsRefused"), int)
                if r > total:
                    raise Fault(f"malformed /v1/health: level3.postsRefusedFinal {r} above postsRefused {total}")
                last = mem.get("level3_refused")
                mem["level3_refused"] = r
                new = 0 if last is None else (r - last if r >= last else r)
                return at_most("node.level3.refused_new", new, n["level3_refused_new_max"],
                               f"level-3 posts the book refused for good since the last run (total {r}):")
            out.append(guarded("node.level3.refused_new", refused_new))

            def retrying():
                r = field(h, ("level3", "postsRefusedRetrying"), int)
                total = field(h, ("level3", "postsRefused"), int)
                if r > total:
                    raise Fault(f"malformed /v1/health: level3.postsRefusedRetrying {r} above postsRefused {total}")
                return at_most("node.level3.retrying", r, n["level3_retrying_max"],
                               "level-3 refusals that may clear, retried (each holds its fact):")
            out.append(guarded("node.level3.retrying", retrying))
    return out


def site_checks(cfg, site, timeout, out):
    base = site.rstrip("/") + "/"
    try:
        status, _ = http(base, timeout)
        out.append(ok("site.up", status, 200) if status == 200 else bad("site.up", status, 200, f"{origin(base)}/ answered HTTP {status}"))
    except Fault as e:
        out.append(bad("site.up", detail=str(e)))

    def public():
        d = get_json(base + "data/demo.json", timeout)
        if not isinstance(d, dict):
            raise Fault("malformed demo.json: not an object")
        if d.get("public") is not True:
            return bad("site.public", d.get("public"), True, "data/demo.json is not marked public: a private preview is served (LC-6)")
        runs = d.get("runs")
        if not isinstance(runs, list):
            raise Fault("malformed demo.json: runs is not a list")
        leaked = [i for i, r in enumerate(runs) if not isinstance(r, dict) or r.get("input", "missing") is not None]
        if leaked:
            return bad("site.public", True, True, f"public flag set but runs {leaked[:5]} carry an input (label rows)")
        return ok("site.public", True, True)
    out.append(guarded("site.public", public))


# ---------------------------------------------------------------- one run

RPC_NAMES = ["rpc.up", "rpc.chain", "rpc.head_age", "rpc.finalized_lag"]
BALANCE_NAMES = ["balances.deployer_eth", "balances.operator_eth", "balances.revenue"]
SITE_NAMES = ["site.up", "site.public"]


def immediate(r):
    """A result that alerts on its first run (no debounce)."""
    return {**r, "malformed": True}


def missing(cfg, what, names, flag):
    """Lines for a target nobody configured: the first alerts if `require.<what>` is set."""
    why = f"no {flag} configured"
    first = immediate(bad(names[0], detail=f"{why} and require.{what} is true")) if cfg["require"][what] else skip(names[0], why)
    return [first] + [skip(n, why) for n in names[1:]]


def gather(cfg, opts, now, mem):
    results = []
    late = f"deadline: the run's {cfg['deadline_s']} s budget (deadline_s) ran out before this check"

    def out_of_time(names):
        done = {r["check"] for r in results}
        results.extend(immediate(bad(n, detail=late)) for n in names if n not in done)

    facts = None
    if not opts.rpc:
        results += missing(cfg, "rpc", RPC_NAMES + BALANCE_NAMES, "RPC (--rpc, --rpc-file or ONYX_WATCH_RPC_URL)")
    else:
        try:
            rpc = Rpc(opts.rpc, cfg["rpc"]["timeout_s"], cfg["rpc"]["tries"], opts.backoff)
            facts = rpc_checks(cfg, rpc, now, results)
            if facts is not None:
                balance_checks(cfg, rpc, results)
        except DeadlineHit:
            facts = None
            out_of_time(RPC_NAMES + BALANCE_NAMES)
    if not opts.node:
        results += missing(cfg, "node", ["node.up"], "node (--node or ONYX_WATCH_NODE)")
    else:
        try:
            h = get_json(opts.node.rstrip("/") + "/v1/health", cfg["node"]["timeout_s"])
        except DeadlineHit:
            out_of_time(["node.up"])
        except Fault as e:
            results += [bad("node.up", detail=str(e))] if e.down else [ok("node.up"), bad("node.health", detail=str(e))]
        else:
            results.append(ok("node.up"))
            results += node_checks(cfg, h, now, facts, mem)
            if isinstance(h, dict) and "payments" in h and h["payments"] is None:
                # Payments off: the operator sends nothing, so its gas cannot block a sale. A node that
                # is down or malformed leaves the balance line as read (the safe side).
                why = "payments off on the node: the operator sends no transaction"
                results[:] = [skip(r["check"], why) if r["check"] == "balances.operator_eth" and not r.get("skip") else r
                              for r in results]
    if not opts.site:
        results += missing(cfg, "site", SITE_NAMES, "site (--site or ONYX_WATCH_SITE)")
    else:
        try:
            site_checks(cfg, opts.site, cfg["site"]["timeout_s"], results)
        except DeadlineHit:
            out_of_time(SITE_NAMES)
    return results


def apply(cfg, results, state, now):
    """Debounce and de-duplicate. Returns (lines, events)."""
    checks = state.setdefault("checks", {})
    after = {k: v for k, v in cfg["after"].items() if not k.startswith("_")}
    realert = cfg["notify"]["realert_after_s"]
    lines, events = [], []
    for r in results:
        name = r["check"]
        st = checks.setdefault(name, {"status": "ok", "consec": 0})
        prev = st.get("status", "ok")
        if r.get("skip"):
            status = "skip"
            st["consec"] = 0
        elif r["fault"]:
            st["consec"] = st.get("consec", 0) + 1
            # A malformed answer alerts at once: only a failure that can be transient waits.
            status = "alert" if r.get("malformed") or st["consec"] >= after.get(name, 1) else "warn"
        else:
            st["consec"] = 0
            status = "ok"
        notify = None
        if status == "alert":
            if prev != "alert":
                st["since"] = now
                st["notified"] = None
            st.pop("resolve_pending", None)
            if st.get("notified") is None:
                notify = "alert"
            elif now - st["notified"] >= realert:
                notify = "repeat"
        else:
            if prev == "alert":
                st["resolve_pending"] = True
                st.pop("notified", None)
                st.pop("since", None)
            if st.get("resolve_pending") and status in ("ok", "skip"):
                notify = "resolved"
        st["status"] = status
        line = {"ts": now, "check": name, "status": status, "value": r["value"], "limit": r["limit"],
                "detail": r["detail"], "notify": notify}
        lines.append(line)
        if notify:
            events.append({"event": "resolved" if notify == "resolved" else "alert", "repeat": notify == "repeat",
                           "host": socket.gethostname(), "since": st.get("since"), **{k: line[k] for k in ("ts", "check", "status", "value", "limit", "detail")}})
    return lines, events


def mark_sent(state, ev):
    st = state["checks"][ev["check"]]
    if ev["event"] == "resolved":
        st.pop("resolve_pending", None)
    else:
        st["notified"] = ev["ts"]


SECRET_ENV = re.compile(r"KEY|SECRET|TOKEN|PASSW|CREDENTIAL|PRIVATE|RPC_URL|MNEMONIC|SEED", re.I)


def hook_env():
    """The notify command's environment: ours without anything that may hold a secret (the RPC URL
    can carry a provider API key; the deployer key, CDP keys and the like never reach a hook)."""
    return {k: v for k, v in os.environ.items() if not SECRET_ENV.search(k)}


class Notifier:
    def __init__(self, alert_log, cmd, timeout):
        self.alert_log, self.cmd, self.timeout = alert_log, cmd, timeout
        # All commands of one run share 2 x command_timeout_s; events left over are sent next run.
        self.until = _clock() + 2 * timeout

    def send(self, ev):
        text = json.dumps(ev, sort_keys=True)
        print(f"health-watch: {ev['event']}{' (repeat)' if ev['repeat'] else ''}: {ev['check']}: {ev['detail']}", file=sys.stderr)
        delivered = True
        try:
            fd = os.open(self.alert_log, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
            with os.fdopen(fd, "a", encoding="utf-8") as f:
                f.write(text + "\n")
        except OSError as e:
            print(f"health-watch: alert log {self.alert_log}: {e.strerror}", file=sys.stderr)
            delivered = False
        if self.cmd:
            left = min(self.timeout, self.until - _clock())
            if left <= 0:
                print("health-watch: notify budget spent; this event is sent on the next run", file=sys.stderr)
                return False
            try:
                p = subprocess.run(shlex.split(self.cmd), input=text + "\n", text=True, timeout=left,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env=hook_env())
                if p.returncode != 0:
                    print(f"health-watch: notify command exit {p.returncode}: {p.stderr.strip()[:200]}", file=sys.stderr)
                    delivered = False
            except (OSError, subprocess.TimeoutExpired) as e:
                print(f"health-watch: notify command failed: {type(e).__name__}", file=sys.stderr)
                delivered = False
        return delivered


STATUSES = {"ok", "warn", "alert", "skip"}


def _int(v, minimum=None):
    return type(v) is int and (minimum is None or v >= minimum)


def validate_state(s):
    """Raise ValueError unless `s` has exactly the shape run_once writes (review L12-R1 #1: valid
    JSON with wrong inner types used to crash every run before the save, so alerts stopped)."""
    if not isinstance(s, dict) or s.get("version") != STATE_VERSION or set(s) - {"version", "checks", "mem"}:
        raise ValueError("not a version 1 state object")
    checks, mem = s.get("checks", {}), s.get("mem", {})
    if not isinstance(checks, dict):
        raise ValueError("checks is not an object")
    for name, st in checks.items():
        if not isinstance(st, dict) or set(st) - {"status", "consec", "since", "notified", "resolve_pending"}:
            raise ValueError(f"checks.{name} is not a check entry")
        if st.get("status") not in STATUSES or not _int(st.get("consec"), 0):
            raise ValueError(f"checks.{name}: bad status or consec")
        for k in ("since", "notified"):
            if st.get(k) is not None and not _int(st[k]):
                raise ValueError(f"checks.{name}.{k} is not an integer")
        if "resolve_pending" in st and type(st["resolve_pending"]) is not bool:
            raise ValueError(f"checks.{name}.resolve_pending is not a bool")
    if not isinstance(mem, dict) or set(mem) - {"intents_open_since", "unknown_outcomes", "level3_refused"}:
        raise ValueError("mem is not the expected object")
    if not all(_int(v, 0) for v in mem.values()):
        raise ValueError("mem holds a non-integer")


def fresh_state():
    return {"version": STATE_VERSION, "checks": {}, "mem": {}}


def move_aside(path):
    try:
        os.replace(path, str(path) + ".bad")
    except OSError:
        pass


def load_state(path):
    """(state, problem or None). A state file that cannot be read or has the wrong shape is moved
    aside and reported."""
    try:
        with open(path, encoding="utf-8") as f:
            s = json.load(f)
        validate_state(s)
        s.setdefault("checks", {})
        s.setdefault("mem", {})
        return s, None
    except FileNotFoundError:
        return fresh_state(), None
    except (OSError, ValueError) as e:
        move_aside(path)
        return fresh_state(), f"state file unreadable or invalid ({type(e).__name__}: {str(e)[:120]}), moved to {path}.bad; alerts sent before may repeat"


def save_state(path, state):
    tmp = f"{path}.tmp"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(state, f, sort_keys=True)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)


def run_once(cfg, opts, now=None, out=None):
    """One pass: gather, debounce, notify, save. Returns the exit code (0 or 1)."""
    now = int(time.time()) if now is None else now
    out = out or sys.stdout
    _DEADLINE[0] = _clock() + cfg["deadline_s"]
    try:
        state, problem = load_state(opts.state)

        def once(state, problem):
            results = gather(cfg, opts, now, state["mem"])
            if problem:
                results.insert(0, immediate(bad("watch.state", detail=problem)))
            elif "watch.state" in state["checks"]:
                results.insert(0, ok("watch.state"))
            return apply(cfg, results, state, now)
        try:
            lines, events = once(state, problem)
        except (TypeError, AttributeError, KeyError) as e:
            # Last line: a state shape validate_state missed. Start clean and say so.
            move_aside(opts.state)
            state = fresh_state()
            lines, events = once(state, f"state file broke the run ({type(e).__name__}: {str(e)[:120]}), "
                                        f"moved to {opts.state}.bad; alerts sent before may repeat")
    finally:
        _DEADLINE[0] = None
    for line in lines:
        out.write(json.dumps(line) + "\n")
    out.flush()
    notifier = Notifier(opts.alert_log, opts.notify_cmd, cfg["notify"]["command_timeout_s"])
    for ev in events:
        if notifier.send(ev):
            mark_sent(state, ev)
    save_state(opts.state, state)
    return 1 if any(line["status"] == "alert" for line in lines) else 0


def parse_args(argv):
    p = argparse.ArgumentParser(prog="health-watch.py", description="Onyx health watch (ROADMAP L12)")
    mode = p.add_mutually_exclusive_group()
    mode.add_argument("--once", action="store_true", help="one pass (cron, systemd timer)")
    mode.add_argument("--interval", type=int, metavar="SECS", help="loop, one pass every SECS")
    mode.add_argument("--print-config", action="store_true", help="print the effective thresholds and exit")
    p.add_argument("--config", action="append", default=[], metavar="FILE", help="JSON overlay on deploy/health-watch/thresholds.json (repeatable)")
    p.add_argument("--node", metavar="URL", default=os.environ.get("ONYX_WATCH_NODE") or None,
                   help="onyx-node base URL, e.g. http://127.0.0.1:8080 (default $ONYX_WATCH_NODE)")
    p.add_argument("--rpc", metavar="URL", help="chain JSON-RPC URL (prefer --rpc-file or ONYX_WATCH_RPC_URL if it holds an API key)")
    p.add_argument("--rpc-file", metavar="FILE", help="file holding the RPC URL (mode 0600)")
    p.add_argument("--site", metavar="URL", default=os.environ.get("ONYX_WATCH_SITE") or None,
                   help="public website base URL (default $ONYX_WATCH_SITE)")
    p.add_argument("--state", metavar="FILE", help="state file (default $XDG_STATE_HOME/onyx-health-watch/state.json)")
    p.add_argument("--alert-log", metavar="FILE", help="alert events, one JSON line each (default: alerts.jsonl next to the state file)")
    p.add_argument("--notify-cmd", metavar="CMD", default=os.environ.get("ONYX_WATCH_NOTIFY_CMD") or None,
                   help="command run per alert event, event JSON on stdin, split like a shell would but run without one "
                        "(default $ONYX_WATCH_NOTIFY_CMD); it gets no environment variable that may hold a secret")
    p.add_argument("--backoff", type=float, default=0.5, help=argparse.SUPPRESS)
    a = p.parse_args(argv)
    if a.rpc_file:
        if a.rpc:
            p.error("--rpc and --rpc-file are exclusive")
        try:
            a.rpc = Path(a.rpc_file).read_text(encoding="utf-8").strip()
        except OSError as e:
            p.error(f"--rpc-file: {e.strerror}")
    elif not a.rpc and os.environ.get("ONYX_WATCH_RPC_URL"):
        a.rpc = os.environ["ONYX_WATCH_RPC_URL"].strip()
    if a.print_config:
        return a
    if not (a.once or a.interval):
        p.error("pass --once or --interval SECS")
    if a.interval is not None and a.interval < 1:
        p.error("--interval must be at least 1")
    if not (a.node or a.rpc or a.site):
        p.error("nothing to watch: pass --node, --rpc/--rpc-file and/or --site")
    for name in ("node", "rpc", "site"):
        v = getattr(a, name)
        if v and urllib.parse.urlsplit(v).scheme not in ("http", "https"):
            p.error(f"--{name} must be an http(s) URL (got {origin(v)})")
    if not a.state:
        base = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local" / "state")
        a.state = str(base / "onyx-health-watch" / "state.json")
    Path(a.state).parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    if not a.alert_log:
        a.alert_log = str(Path(a.state).parent / "alerts.jsonl")
    return a


def main(argv=None):
    try:
        a = parse_args(sys.argv[1:] if argv is None else argv)
        cfg = load_config(a.config)
    except ConfigError as e:
        print(f"health-watch: config: {e}", file=sys.stderr)
        return 2
    if a.print_config:
        print(json.dumps(cfg, indent=2))
        return 0
    lock_path = a.state + ".lock"
    lock = open(lock_path, "a")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        print(f"health-watch: another run holds {lock_path}", file=sys.stderr)
        return 2
    if a.once:
        return run_once(cfg, a)
    try:
        while True:
            start = time.monotonic()
            run_once(cfg, a)
            time.sleep(max(0.0, a.interval - (time.monotonic() - start)))
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    sys.exit(main())
