#!/usr/bin/env python3
"""Record the SEC EDGAR responses behind the spot bitcoin ETF holdings feeds (ROADMAP P2c).

For every fund the Rust registry covers (adapters/source-edgar/src/etf.rs `FUNDS`), three
simulated observers GET the fund's pinned `companyconcept` document on data.sec.gov inside one
300-second window, and the bodies are written to adapters/source-edgar/fixtures/etf/ with a
manifest (fetch times, the server's Date header, SHA-256 of every body). Tests and the website
replay these files offline; `cargo test -p onyx-source-edgar` checks the manifest against the
registry (URL per fund) and every body against its SHA-256.

SEC fair access (https://www.sec.gov/os/accessing-edgar-data): a declared User-Agent and at most
10 requests per second. This script sends at most 2 per second, only to data.sec.gov, with the
User-Agent below. The SEC asks for a contact in it; the project has no role contact yet, so it
says "contact pending" (ROADMAP P2g; never a personal address).

Usage: python3 -I scripts/record-sec-etf-holdings.py [--out DIR]
       python3 -I scripts/record-sec-etf-holdings.py --self-test   (offline; run by check-all)
"""
import hashlib
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

USER_AGENT = "onyx-dev (contact pending)"
HOST = "https://data.sec.gov/"
OBSERVERS = 3
SEC_MIN_GAP_SECS = 0.1  # SEC fair access: at most 10 requests per second
GAP_SECS = 0.6  # at most 2 requests per second (SEC allows 10)
WINDOW_SECS = 300

# Must match adapters/source-edgar/src/etf.rs FUNDS (covered funds): `--self-test` compares them,
# and the Rust manifest test refuses a recording whose URLs differ from the registry.
FUNDS = [
    ("ibit", "0001980994", "CryptoAssetNumberOfUnits"),
    ("fbtc", "0001852317", "InvestmentOwnedBalanceShares"),
    ("gbtc", "0001588489", "InvestmentOwnedBalanceContracts"),
    ("btc_mini", "0002015034", "InvestmentOwnedBalanceContracts"),
    ("arkb", "0001869699", "InvestmentOwnedBalanceContracts"),
    ("ezbc", "0001992870", "CryptoAssetNumberOfUnits"),
    ("defi", "0001985840", "InvestmentOwnedBalanceShares"),
]


def url(cik, concept):
    return f"{HOST}api/xbrl/companyconcept/CIK{cik}/us-gaap/{concept}.json"


class Pacer:
    """Spaces request starts at least `gap` seconds apart on a monotonic clock (THREATS ETF-5):
    SEC fair access allows 10 requests per second; the recorder sends at most 2."""

    def __init__(self, gap=GAP_SECS, clock=time.monotonic, sleep=time.sleep):
        if not gap >= SEC_MIN_GAP_SECS:
            raise ValueError(f"gap {gap} s would exceed SEC's 10 requests per second")
        self.gap, self.clock, self.sleep, self.last = gap, clock, sleep, None

    def wait(self):
        if self.last is not None:
            left = self.gap - (self.clock() - self.last)
            if left > 0:
                self.sleep(left)
        self.last = self.clock()


class NoRedirect(urllib.request.HTTPRedirectHandler):
    """A redirect is never followed (it could leave data.sec.gov); the 3xx is answered as
    an error status, which main() refuses."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


OPENER = urllib.request.build_opener(NoRedirect)


def get(u):
    if not u.startswith(HOST):
        raise ValueError(f"only {HOST} is fetched, not {u}")
    req = urllib.request.Request(u, headers={"User-Agent": USER_AGENT, "Accept-Encoding": "identity"})
    try:
        with OPENER.open(req, timeout=60) as r:
            return r.status, r.headers.get("Date", ""), r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("Date", ""), e.read()


def self_test():
    """Offline checks of the network rules (no request is sent)."""
    def fail(msg):
        sys.exit(f"record-sec-etf-holdings self-test: {msg}")

    # 1. Pacing: 21 instant requests on a fake clock start at least GAP_SECS apart, so never
    #    more than 10 (here 2) in any one-second window.
    now = [1000.0]
    starts = []
    p = Pacer(clock=lambda: now[0], sleep=lambda s: now.__setitem__(0, now[0] + s))
    for _ in range(OBSERVERS * len(FUNDS)):
        p.wait()
        starts.append(now[0])
    gaps = [b - a for a, b in zip(starts, starts[1:])]
    if min(gaps) < GAP_SECS - 1e-9:
        fail(f"requests {min(gaps)} s apart, want at least {GAP_SECS}")
    if max(sum(1 for t in starts if s <= t < s + 1) for s in starts) > 10:
        fail("more than 10 requests in one second")
    # 2. A gap that would break SEC's limit is refused.
    for bad in (0.0, 0.05, 0.099, float("nan")):
        try:
            Pacer(gap=bad)
            fail(f"gap {bad} accepted")
        except ValueError:
            pass
    # 3. Only data.sec.gov is fetched; a redirect is never followed.
    for u in ("https://www.sec.gov/Archives/", "http://data.sec.gov/api/", "https://data.sec.gov.evil.example/"):
        try:
            get(u)
            fail(f"{u} fetched")
        except ValueError:
            pass
    if NoRedirect().redirect_request(None, None, 301, "", {}, "https://evil.example/") is not None:
        fail("a redirect is followed")
    # 4. The User-Agent names no person: no e-mail address, no @ at all (ETF-5, SEC-4).
    if "@" in USER_AGENT or re.search(r"[\w.+-]+@[\w-]+\.", USER_AGENT):
        fail("the User-Agent carries an address")
    # 5. The funds equal the Rust registry (key, CIK, concept, in order).
    rs = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "adapters", "source-edgar", "src", "etf.rs")
    text = open(rs, encoding="utf-8").read()
    block = text[text.index("pub const FUNDS"):text.index("pub struct NotCovered")]
    rust = re.findall(r'key: "([^"]+)",.*?cik: "([^"]+)",\s*concept: "([^"]+)"', block, re.S)
    if rust != FUNDS:
        fail(f"FUNDS differ from etf.rs: {rust} vs {FUNDS}")
    print(f"record-sec-etf-holdings self-test ok: {len(starts)} paced requests, {len(FUNDS)} funds")


def main():
    out = "adapters/source-edgar/fixtures/etf"
    if "--out" in sys.argv:
        out = sys.argv[sys.argv.index("--out") + 1]
    os.makedirs(out, exist_ok=True)
    window_start = int(time.time())
    funds = []
    pacer = Pacer()
    for key, cik, concept in FUNDS:
        fetches, bodies = [], []
        for observer in range(1, OBSERVERS + 1):
            pacer.wait()
            fetched = int(time.time())
            status, date, body = get(url(cik, concept))
            if status != 200:
                sys.exit(f"{key}: HTTP {status} from {url(cik, concept)}")
            if fetched >= window_start + WINDOW_SECS:
                sys.exit("the recording ran past its 300-second window; run it again")
            sha = hashlib.sha256(body).hexdigest()
            fetches.append({"observer": observer, "fetched_unix": fetched, "http_date": date, "sha256": sha})
            bodies.append(body)
        if len(set(bodies)) != 1:
            sys.exit(f"{key}: observers got different bodies; record again")
        with open(os.path.join(out, f"{key}.json"), "wb") as f:
            f.write(bodies[0])
        funds.append({
            "key": key,
            "url": url(cik, concept),
            "body": f"{key}.json",
            "bytes": len(bodies[0]),
            "sha256": hashlib.sha256(bodies[0]).hexdigest(),
            "fetches": fetches,
        })
        print(f"{key}: {len(bodies[0])} bytes, sha256 {funds[-1]['sha256']}")
    manifest = {
        "source": "SEC EDGAR XBRL companyconcept (data.sec.gov)",
        "user_agent": USER_AGENT,
        "window_start": window_start,
        "window_secs": WINDOW_SECS,
        "funds": funds,
    }
    with open(os.path.join(out, "manifest.json"), "w") as f:
        json.dump(manifest, f, indent=2)
        f.write("\n")


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        self_test()
    else:
        main()
