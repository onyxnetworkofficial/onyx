#!/usr/bin/env python3
"""Fault-injecting JSON-RPC proxy for the soak test (ROADMAP L6, scripts/soak.sh).

Sits between onyx-node and a local anvil and, per request, may answer with a
fault instead of (or after) forwarding. Local only: it listens on 127.0.0.1
and forwards to one upstream URL on 127.0.0.1.

    rpc_fault_proxy.py --listen 18751 --upstream http://127.0.0.1:18750 \
        --control faults.json --stats proxy-stats.json

The control file is JSON, re-read when it changes:
    {"profile": "mixed", "rates": {"error": 0.05, ...}, "lag_blocks": 40}
`rates` maps a fault to its probability per request (at most one fault per
request, drawn in the order of FAULTS). Faults:
    error      JSON-RPC error object (code -32000)
    http429    HTTP 429 with a JSON-RPC error body
    http5xx    HTTP 500, 502 or 503
    timeout    hold the connection 15-25 s without an answer, then close
    slow       forward after 0.5-4 s
    stale      an older answer to the same call (a lagging backend); for
               eth_getLogs an empty list; for head reads `lag_blocks` behind
    future     head reads (eth_blockNumber, eth_getBlockByNumber
               latest/safe/finalized) 10,000 blocks ahead of the real head
    drop       close the connection without an answer
    lost       forward (the upstream acts on it), then close without the
               answer (a send that landed but whose answer was lost)
    wrong_id   the real answer with another request's id
    garbage    a truncated JSON body
Stats (counts per fault and method, upstream errors) go to --stats every 5 s.
Python standard library only.
"""

import argparse
import asyncio
import json
import os
import random
import time
import urllib.parse

FAULTS = [
    "error",
    "http429",
    "http5xx",
    "timeout",
    "slow",
    "stale",
    "future",
    "drop",
    "lost",
    "wrong_id",
    "garbage",
]
HEAD_TAGS = ("latest", "safe", "finalized", "pending")
MAX_BODY = 32 << 20


class Control:
    def __init__(self, path):
        self.path = path
        self.mtime = None
        self.profile = "clean"
        self.rates = {}
        self.lag_blocks = 40
        self.methods = None  # None: every method

    def reload(self):
        if not self.path:
            return
        try:
            m = os.stat(self.path).st_mtime_ns
        except FileNotFoundError:
            return
        if m == self.mtime:
            return
        try:
            with open(self.path) as f:
                c = json.load(f)
        except (OSError, ValueError):
            return
        self.mtime = m
        self.profile = c.get("profile", "clean")
        self.rates = {k: float(v) for k, v in c.get("rates", {}).items() if k in FAULTS}
        self.lag_blocks = int(c.get("lag_blocks", 40))
        self.methods = c.get("methods")

    def pick(self, method):
        if self.methods is not None and method not in self.methods:
            return None
        r = random.random()
        acc = 0.0
        for f in FAULTS:
            acc += self.rates.get(f, 0.0)
            if r < acc:
                return f
        return None


class Stats:
    def __init__(self, path):
        self.path = path
        self.started = time.time()
        self.requests = 0
        self.faults = {}
        self.methods = {}
        self.upstream_errors = 0
        self.profile_seconds = {}
        self.last_profile = None
        self.last_t = time.time()

    def count(self, method, fault):
        self.requests += 1
        self.methods[method] = self.methods.get(method, 0) + 1
        if fault:
            k = f"{fault}"
            self.faults[k] = self.faults.get(k, 0) + 1
            km = f"{fault}:{method}"
            self.faults[km] = self.faults.get(km, 0) + 1

    def tick(self, profile):
        now = time.time()
        if self.last_profile is not None:
            self.profile_seconds[self.last_profile] = (
                self.profile_seconds.get(self.last_profile, 0.0) + now - self.last_t
            )
        self.last_profile = profile
        self.last_t = now

    def write(self, profile):
        self.tick(profile)
        if not self.path:
            return
        tmp = self.path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(
                {
                    "since": self.started,
                    "at": time.time(),
                    "profile": profile,
                    "requests": self.requests,
                    "faults": self.faults,
                    "methods": self.methods,
                    "upstreamErrors": self.upstream_errors,
                    "profileSeconds": {k: round(v) for k, v in self.profile_seconds.items()},
                },
                f,
                indent=1,
                sort_keys=True,
            )
        os.replace(tmp, self.path)


class Proxy:
    def __init__(self, upstream, control, stats):
        u = urllib.parse.urlsplit(upstream)
        if u.hostname not in ("127.0.0.1", "localhost"):
            raise SystemExit("upstream must be on 127.0.0.1")
        self.up_host = u.hostname
        self.up_port = u.port or 80
        self.control = control
        self.stats = stats
        # (method, params) -> list of (time, result), oldest first.
        self.history = {}
        self.ids = 0

    async def upstream(self, body):
        r, w = await asyncio.wait_for(
            asyncio.open_connection(self.up_host, self.up_port), timeout=10
        )
        try:
            w.write(
                b"POST / HTTP/1.1\r\nHost: upstream\r\nContent-Type: application/json\r\n"
                + f"Content-Length: {len(body)}\r\nConnection: close\r\n\r\n".encode()
                + body
            )
            await w.drain()
            head = await asyncio.wait_for(r.readuntil(b"\r\n\r\n"), timeout=60)
            status = int(head.split(b" ", 2)[1])
            length = None
            chunked = False
            for line in head.split(b"\r\n")[1:]:
                k, _, v = line.partition(b":")
                k = k.strip().lower()
                if k == b"content-length":
                    length = int(v.strip())
                elif k == b"transfer-encoding" and b"chunked" in v.lower():
                    chunked = True
            if chunked:
                data = b""
                while True:
                    size = int((await r.readuntil(b"\r\n")).strip().split(b";")[0], 16)
                    if size == 0:
                        await r.readuntil(b"\r\n")
                        break
                    data += await r.readexactly(size)
                    await r.readexactly(2)
            elif length is not None:
                data = await asyncio.wait_for(r.readexactly(length), timeout=60)
            else:
                data = await asyncio.wait_for(r.read(MAX_BODY), timeout=60)
            return status, data
        finally:
            w.close()

    def remember(self, key, result):
        h = self.history.setdefault(key, [])
        h.append((time.time(), result))
        if len(h) > 8:
            del h[0]
        if len(self.history) > 20000:
            self.history.clear()

    def stale(self, key, min_age):
        now = time.time()
        for t, res in self.history.get(key, []):
            if now - t >= min_age:
                return res
        return None

    async def handle(self, reader, writer):
        try:
            while True:
                try:
                    head = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), timeout=120)
                except (asyncio.IncompleteReadError, asyncio.TimeoutError, asyncio.LimitOverrunError):
                    return
                length = 0
                close = False
                for line in head.split(b"\r\n")[1:]:
                    k, _, v = line.partition(b":")
                    k = k.strip().lower()
                    if k == b"content-length":
                        length = int(v.strip())
                    elif k == b"connection" and v.strip().lower() == b"close":
                        close = True
                if length > MAX_BODY:
                    return
                body = await reader.readexactly(length)
                keep = await self.one(body, writer)
                if not keep or close:
                    return
        except (ConnectionError, asyncio.IncompleteReadError):
            return
        finally:
            try:
                writer.close()
            except Exception:
                pass

    async def reply(self, writer, status, payload, reason=b"OK"):
        writer.write(
            b"HTTP/1.1 %d %s\r\nContent-Type: application/json\r\nContent-Length: %d\r\n\r\n"
            % (status, reason, len(payload))
            + payload
        )
        await writer.drain()

    async def one(self, body, writer):
        """Answer one request; False closes the connection."""
        self.control.reload()
        try:
            req = json.loads(body)
        except ValueError:
            req = None
        if not isinstance(req, dict):
            # Batches and junk pass through untouched.
            status, data = await self.upstream(body)
            await self.reply(writer, status, data)
            return True
        method = req.get("method", "?")
        rid = req.get("id")
        key = (method, json.dumps(req.get("params"), sort_keys=True))
        fault = self.control.pick(method)
        is_head = method == "eth_blockNumber" or (
            method == "eth_getBlockByNumber"
            and isinstance(req.get("params"), list)
            and req["params"]
            and req["params"][0] in HEAD_TAGS
        )
        if fault == "future" and not is_head:
            fault = None
        self.stats.count(method, fault)

        def rpc_answer(result=None, error=None, id_=rid):
            v = {"jsonrpc": "2.0", "id": id_}
            if error is not None:
                v["error"] = error
            else:
                v["result"] = result
            return json.dumps(v).encode()

        if fault == "error":
            await self.reply(writer, 200, rpc_answer(error={"code": -32000, "message": "injected fault"}))
            return True
        if fault == "http429":
            await self.reply(
                writer, 429, rpc_answer(error={"code": 429, "message": "rate limited (injected)"}), b"Too Many Requests"
            )
            return True
        if fault == "http5xx":
            code = random.choice([500, 502, 503])
            await self.reply(writer, code, b'{"error":"injected"}', b"Error")
            return True
        if fault == "timeout":
            await asyncio.sleep(random.uniform(15, 25))
            return False
        if fault == "drop":
            return False
        if fault == "slow":
            await asyncio.sleep(random.uniform(0.5, 4.0))
        try:
            status, data = await self.upstream(body)
        except Exception:
            self.stats.upstream_errors += 1
            return False
        if fault == "lost":
            return False
        if status != 200:
            await self.reply(writer, status, data)
            return True
        try:
            ans = json.loads(data)
        except ValueError:
            await self.reply(writer, status, data)
            return True
        if isinstance(ans, dict) and "result" in ans and ans.get("error") is None:
            self.remember(key, ans["result"])
            res = ans["result"]
            if fault == "stale":
                if is_head:
                    res = lag(res, self.control.lag_blocks, method)
                elif method == "eth_getLogs":
                    res = []
                else:
                    old = self.stale(key, 5.0)
                    if old is not None:
                        res = old
                await self.reply(writer, 200, rpc_answer(res))
                return True
            if fault == "future":
                await self.reply(writer, 200, rpc_answer(lag(res, -10_000, method)))
                return True
        if fault == "wrong_id":
            other = rid + 7919 if isinstance(rid, int) else "other"
            if isinstance(ans, dict):
                ans["id"] = other
            await self.reply(writer, 200, json.dumps(ans).encode())
            return True
        if fault == "garbage":
            await self.reply(writer, 200, data[: max(1, len(data) // 2)])
            return True
        await self.reply(writer, 200, data)
        return True


def lag(res, blocks, method):
    """A head answer moved `blocks` back (negative: ahead)."""
    if method == "eth_blockNumber" and isinstance(res, str):
        return hex(max(0, int(res, 16) - blocks))
    if isinstance(res, dict) and isinstance(res.get("number"), str):
        out = dict(res)
        out["number"] = hex(max(0, int(res["number"], 16) - blocks))
        if blocks < 0:
            # A block that does not exist: its hash cannot be real either.
            out["hash"] = "0x" + os.urandom(32).hex()
        return out
    return res


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--listen", type=int, required=True)
    ap.add_argument("--upstream", required=True)
    ap.add_argument("--control")
    ap.add_argument("--stats")
    ap.add_argument("--seed", type=int)
    a = ap.parse_args()
    if a.seed is not None:
        random.seed(a.seed)
    control = Control(a.control)
    stats = Stats(a.stats)
    proxy = Proxy(a.upstream, control, stats)
    server = await asyncio.start_server(proxy.handle, "127.0.0.1", a.listen, limit=1 << 20)

    async def dump():
        while True:
            await asyncio.sleep(5)
            control.reload()
            try:
                stats.write(control.profile)
            except OSError:
                pass

    asyncio.get_running_loop().create_task(dump())
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
