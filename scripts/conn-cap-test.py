#!/usr/bin/env python3
# Behaviour test of deploy/nftables/onyx-conn-cap.nft (ROADMAP L19, THREATS NS17-4).
# Runs INSIDE the throwaway container that scripts/conn-cap-test.sh starts (its own network
# namespace, no network): it loads the real file, listens on port 443 on 127.0.0.1 and ::1, and
# checks that one source gets exactly CAP open connections, the next is reset, a closed one frees
# a place, loading the file twice keeps one table, and a broken file is refused.
# Never run it on a host: it loads firewall rules into the namespace it runs in.
import os
import re
import socket
import subprocess
import sys
import time

HOST = "/host"
LOADER = [f"{HOST}/lib64/ld-linux-x86-64.so.2", "--library-path",
          f"{HOST}/lib/x86_64-linux-gnu:{HOST}/usr/lib/x86_64-linux-gnu", f"{HOST}/usr/sbin/nft"]
RULES = sys.argv[1]


def ip(*args):
    r = subprocess.run(LOADER[:-1] + [f"{HOST}/usr/sbin/ip"] + list(args), capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"ip {' '.join(args)} failed: {r.stderr.strip()}")


def nft(*args, check=True):
    r = subprocess.run(LOADER + list(args), capture_output=True, text=True)
    if check and r.returncode != 0:
        sys.exit(f"nft {' '.join(args)} failed: {r.stderr.strip()}")
    return r


def cap_of(text):
    caps = {int(n) for n in re.findall(r"ct count over (\d+)", text)}
    if len(caps) != 1:
        sys.exit(f"expected one cap in the file, found {sorted(caps)}")
    return caps.pop()


def listen(family, addr):
    s = socket.socket(family, socket.SOCK_STREAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind((addr, 443))
    s.listen(1024)
    return s


def connect(family, addr, source=None):
    c = socket.socket(family, socket.SOCK_STREAM)
    c.settimeout(5)
    if source:
        c.bind((source, 0))
    try:
        c.connect((addr, 443))
    except (ConnectionRefusedError, ConnectionResetError):
        c.close()
        return None
    return c


def fail(msg):
    print(f"FAIL {msg}")
    sys.exit(1)


def main():
    text = open(RULES).read()
    cap = cap_of(text)
    if os.environ.get("ONYX_CONN_CAP_IN_CONTAINER") != "1":
        sys.exit("refusing to run outside scripts/conn-cap-test.sh's container")
    bad = "/tmp/bad.nft"
    with open(bad, "w") as f:
        f.write(text.replace("@https_v4 {", "@no_such_set {", 1))
    if nft("-c", "-f", bad, check=False).returncode == 0:
        fail("a file naming a missing set passed the syntax check")
    print("ok   a broken file is refused")
    nft("-c", "-f", RULES)
    nft("-f", RULES)
    nft("-f", RULES)  # loading again replaces the table, never adds a second one
    tables = nft("list", "tables").stdout.split()
    if tables.count("onyx_conn_cap") != 1:
        fail(f"tables after two loads: {tables}")
    print("ok   loading twice keeps one onyx_conn_cap table")
    for family, addr in ((socket.AF_INET, "127.0.0.1"), (socket.AF_INET6, "::1")):
        server = listen(family, addr)
        held = []
        for i in range(cap):
            c = connect(family, addr)
            if c is None:
                fail(f"{addr}: connection {i + 1} of {cap} refused")
            held.append((c, server.accept()[0]))
        if connect(family, addr) is not None:
            fail(f"{addr}: connection {cap + 1} was accepted (cap {cap})")
        print(f"ok   {addr}: {cap} connections open, the next one is reset")
        # Closing one (both ends, as Caddy does) frees a place once the kernel sees it closed.
        for s in held.pop():
            s.close()
        for _ in range(50):
            c = connect(family, addr)
            if c is not None:
                held.append((c, server.accept()[0]))
                break
            time.sleep(0.1)
        else:
            fail(f"{addr}: a closed connection did not free a place")
        print(f"ok   {addr}: a closed connection frees a place")
        for pair in held:
            for s in pair:
                s.close()
        server.close()
    # IPv6 counts per /64: ::1 and ::2 share one prefix, so they share one cap.
    ip("-6", "addr", "add", "::2/128", "dev", "lo")
    server = listen(socket.AF_INET6, "::")
    held = []
    for i in range(cap):
        c = connect(socket.AF_INET6, "::1", source="::1" if i % 2 else "::2")
        if c is None:
            fail(f"/64: connection {i + 1} of {cap} refused")
        held.append((c, server.accept()[0]))
    if connect(socket.AF_INET6, "::1", source="::2") is not None:
        fail(f"/64: a second address in the same /64 got past the cap of {cap}")
    print(f"ok   ::1 and ::2 (one /64) share the cap of {cap}")
    for pair in held:
        for s in pair:
            s.close()
    server.close()
    counters = nft("list", "table", "inet", "onyx_conn_cap").stdout
    if not re.search(r"counter packets [1-9]", counters):
        fail("the reject rules counted nothing")
    print("CONN CAP TEST PASSED")


main()
