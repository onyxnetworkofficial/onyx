#!/usr/bin/env bash
# Restore drill for a backup-node.sh snapshot (THREATS OPS-BACKUP-1, BAK-R1; docs/ops/deployment.md "Restore the
# API node"): start a local onyx-node from the snapshot and check it serves what the live node served.
#
# Usage: scripts/restore-drill.sh [--full] SNAPSHOT_DIR
#        scripts/restore-drill.sh --self-test   (offline: the config filter, the launch, the journal
#                                               check, the --full shard check, the live node's reads and
#                                               the lock, with stand-ins on 127.0.0.1; no node, no
#                                               outside network)
#   --full  also read back every shard tiers.json names through the node (each must hash to its name)
#           and require every dataset's newest manifest to have all its shards; backup-node.sh runs
#           the drill with it on every copy before the copy becomes a snapshot.
# Env:   ONYX_NODE_BIN (default the snapshot's own server/onyx-node, else the release build in
#        ~/.cache/onyx-api-target/release), ONYX_CHECK_BIN (default the release onyx-check there),
#        ONYX_DRILL_LIVE (https://onyx.cryptagraph.com), ONYX_DRILL_RPC (https://sepolia.base.org),
#        ONYX_DRILL_MEMORY_MAX (8G), ONYX_DRILL_START_SECS (900, the cap on the node's start),
#        ONYX_DRILL_MAX_SECS (10800, the node scope's RuntimeMaxSec), ONYX_DRILL_SSH_HOST (onyx-web;
#        backup-node.sh passes its ONYX_BACKUP_HOST: the server, reached over ssh when the public
#        URL fails).
#
# 1. The lock: the drill holds the backup folder's .lock shared, so no backup-node.sh run prunes
#    the snapshot under it (a run started by backup-node.sh itself, ONYX_DRILL_UNDER_BACKUP=1, is
#    under that run's lock). A drill killed before its trap leaves its .drill.* folder; the next
#    backup run deletes it after a day, and stops its node's scope (below).
# 2. A scratch folder next to the snapshot (same disk, 0700): the bulk store as hard links to the
#    snapshot's files, state, cas and etc-onyx as real copies, the binary copied and made
#    executable. Hard links are safe because the node never writes a bulk file in place
#    (crates/onyx-api/src/bulk_serve.rs, bulk_erase.rs, bulk_learn.rs; crates/onyx-coordinator/src/
#    cas.rs, tiers.rs): a shard goes through FsCas::put (a new uniquely named temp file, synced,
#    renamed in); manifests, catalogs, fork marks, erase and learned records through write_atomic or
#    write_durable, and tiers.json through TieredStore::save (each `<name>.tmp`, then a rename); a
#    tier move copies, then unlinks; an erase unlinks. The one exception is a leftover `*.tmp`
#    (those writers truncate it), so no `*.tmp` is linked. The state journals are appended to, so
#    state/ is a real copy. The drill checks afterwards that no file of the snapshot's bulk store
#    changed (size, mtime, mode, inode).
# 3. A local config from the snapshot's node.env, never printed: only the settings on an allow-list
#    are taken (the store, the catalog, the deployment, the indexer, limits), every /etc/onyx and
#    /var/lib/onyx path moved to the copy. An allow-listed setting naming a file no snapshot holds (a
#    path outside /etc/onyx and /var/lib/onyx's bulk, state and cas, or a /etc/onyx file missing from
#    the snapshot) is a FAIL naming the setting, never its value; a setting holding an address, or
#    not on the list, is left out and named in a NOTE line. Then 127.0.0.1
#    on a free port and an http://127.0.0.1 public URL (no TLS, no proxy), payments off, erasure off,
#    no request limit, the public chain node https://sepolia.base.org, the indexer's held-shard
#    rescan at 10 calls a poll. Intake, salted labels, keys, storage-node and paid settings are not on
#    the list. The settings reach the node through its environment only (never a command line:
#    systemd-run gets a fixed --description), and only counts and setting names are printed.
# 4. Before the start: state/wants.salt must be there, 32 bytes (a missing salt or an empty state/
#    is a FAIL), and every checksummed state journal (*.journal and *.jsonl, state/self-settle/
#    too) is read like the node's journal reader does (each line's keccak-256 sum, the header, the
#    batches; a torn last line allowed), so the journals the drill's node does not open are checked
#    too: intake (submissions.journal, bound to no chain) and, once payments are on, the money
#    journals (receipts.jsonl, settle-intents.jsonl, credits-in-flight.jsonl, pending-settles.jsonl),
#    each bound to the snapshot's deployment.json chain and OnyxRevenue, as the node binds them.
#    Contents are never printed.
# 5. `onyx-node --check-config`, then the node under systemd-run --user --scope -p MemoryMax=8G
#    -p RuntimeMaxSec=10800, without the lock descriptors 8 and 9 (a node outliving a killed drill
#    never holds the backup folder's lock; the scope is named after the drill's pid, and the next
#    backup run stops it once the drill is gone).
# 6. Checks, each printed PASS or FAIL: the binary is the server's build (SNAPSHOT.json), /v1/health
#    ok with the live chain, contract and rights, each publisher's newest catalog number and root
#    equal to the live node's catalog of that number, and for at least 5 sampled datasets (both
#    publishers, one widened format-2 dataset) the local GET /v1/bulk/{p}/{d}/rows passes onyx-check
#    --bulk on Base Sepolia against the snapshot's signedBook with the live node's digest and
#    manifest root for that version. The live node may have published since the snapshot (a daily
#    publish): then the drill compares against the live node's copy of the catalog number and
#    dataset version the snapshot holds, and says so. With --full, the two store-wide checks above.
#    After the stop: wants.salt (read by the node at every start) unchanged. Every shard of --full is
#    counted, whatever fails on it: the check passes only when all were read and their bytes add up.
# 7. The node is stopped and the scratch folder deleted, on success, failure or Ctrl-C (a trap).
# Exit 0 only when every check passes, 2 for a refused start (arguments, the lock). Reads the live
# node over HTTPS (about 30 requests), or, when its public URL fails (Caddy, TLS or DNS down), over
# ssh to ONYX_DRILL_SSH_HOST with `curl http://127.0.0.1:8080/...` there (read only, said in a NOTE
# line); writes nothing outside the scratch folder.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
self=$repo/scripts/$(basename "$0")
live=${ONYX_DRILL_LIVE:-https://onyx.cryptagraph.com}
rpc=${ONYX_DRILL_RPC:-https://sepolia.base.org}
mem=${ONYX_DRILL_MEMORY_MAX:-8G}
start_cap=${ONYX_DRILL_START_SECS:-900}
check_bin=${ONYX_CHECK_BIN:-$HOME/.cache/onyx-api-target/release/onyx-check}
max_secs=${ONYX_DRILL_MAX_SECS:-10800}
ssh_host=${ONYX_DRILL_SSH_HOST:-onyx-web}
# Test hooks (the self-test only): stand-ins for systemd-run and ssh.
systemd_run=${ONYX_DRILL_SYSTEMD_RUN:-systemd-run}
ssh_cmd=${ONYX_DRILL_SSH:-ssh}
ua=onyx-restore-drill
umask 077

# keccak-256 (the node's hash) and the journal reader of crates/onyx-api/src/journal.rs, in Python
# for files of kilobytes: `<sum> <json>` lines, the sum the first 16 bytes of keccak-256 of the JSON
# in hex, the first line the header, every later line a batch of {"add": id, "v": value} or
# {"del": id}; a last line without its newline is a torn write and cut off. Modes: check STATE_DIR
# (one PASS or FAIL line per journal, counts only), frame (JSON lines on stdin to framed lines),
# vectors (keccak-256 of "" and "abc").
PYJ=$(cat <<'PY'
import json, os, sys
RC = [0x0000000000000001, 0x0000000000008082, 0x800000000000808A, 0x8000000080008000,
      0x000000000000808B, 0x0000000080000001, 0x8000000080008081, 0x8000000000008009,
      0x000000000000008A, 0x0000000000000088, 0x0000000080008009, 0x000000008000000A,
      0x000000008000808B, 0x800000000000008B, 0x8000000000008089, 0x8000000000008003,
      0x8000000000008002, 0x8000000000000080, 0x000000000000800A, 0x800000008000000A,
      0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008]
ROT = [[0, 36, 3, 41, 18], [1, 44, 10, 45, 2], [62, 6, 43, 15, 61], [28, 55, 25, 21, 56], [27, 20, 39, 8, 14]]
M64 = (1 << 64) - 1
def rol(v, n):
    return ((v << n) | (v >> (64 - n))) & M64 if n else v
def permute(a):
    for rc in RC:
        c = [a[x][0] ^ a[x][1] ^ a[x][2] ^ a[x][3] ^ a[x][4] for x in range(5)]
        d = [c[(x - 1) % 5] ^ rol(c[(x + 1) % 5], 1) for x in range(5)]
        a = [[a[x][y] ^ d[x] for y in range(5)] for x in range(5)]
        b = [[0] * 5 for _ in range(5)]
        for x in range(5):
            for y in range(5):
                b[y][(2 * x + 3 * y) % 5] = rol(a[x][y], ROT[x][y])
        a = [[b[x][y] ^ ((~b[(x + 1) % 5][y]) & b[(x + 2) % 5][y]) for y in range(5)] for x in range(5)]
        a[0][0] ^= rc
    return a
def keccak256(data):
    rate = 136
    p = bytearray(data) + b"\x01"
    p += b"\x00" * (-len(p) % rate)
    p[-1] |= 0x80
    a = [[0] * 5 for _ in range(5)]
    for off in range(0, len(p), rate):
        for i in range(rate // 8):
            a[i % 5][i // 5] ^= int.from_bytes(p[off + 8 * i:off + 8 * i + 8], "little")
        a = permute(a)
    return b"".join(a[i % 5][i // 5].to_bytes(8, "little") for i in range(4))
def framed(v):
    body = json.dumps(v, separators=(",", ":")).encode()
    return keccak256(body)[:16].hex().encode() + b" " + body + b"\n"
# The journals in this format, by file name, with their kind and what the node binds them to
# (Journal::open refuses another header): intake (crates/onyx-api/src/submit.rs) to no chain; the
# money journals, kept once payments are on (receipts.rs, intents.rs, reporter.rs credits in
# flight, self_settle.rs pending settles under state/self-settle/), to the deployment's chain and
# its OnyxRevenue (`revenue` in deployment.json), in lowercase hex as the node writes it.
FRAMED = {"submissions.journal": ("onyx-submissions", False), "receipts.jsonl": ("onyx-receipts", True),
          "settle-intents.jsonl": ("onyx-settle-intents", True),
          "credits-in-flight.jsonl": ("onyx-credits-in-flight", True),
          "pending-settles.jsonl": ("onyx-pending-settles", True)}
NO_CHAIN = (0, "0x" + "00" * 20)
def binding(dep):
    """The deployment's (chain, OnyxRevenue), or None when it names no revenue contract."""
    rev, chain = (dep.get("revenue"), dep.get("chainId")) if isinstance(dep, dict) else (None, None)
    if isinstance(rev, str) and len(rev) == 42 and rev[:2] == "0x" and all(c in "0123456789abcdefABCDEF" for c in rev[2:]) \
            and isinstance(chain, int) and not isinstance(chain, bool):
        return (chain, rev.lower())
    return None
def header_ok(v, kind, binds):
    """None when header v is a journal header of `kind` (any kind when None) bound to one of `binds`."""
    if not isinstance(v, dict) or set(v) != {"onyxJournal", "kind", "chainId", "contract"} or v["onyxJournal"] != 2:
        return "not the header this node writes"
    if kind is not None and v["kind"] != kind:
        return "another kind of journal"
    if (v["chainId"], v["contract"]) not in binds:
        return "bound to another chain or contract than the deployment's"
    return None
def check(path, kind=None, binds=(NO_CHAIN,)):
    data = open(path, "rb").read()
    lines = data.split(b"\n")
    torn = lines.pop() != b""
    live, nxt, batches = set(), 0, 0
    for n, line in enumerate(lines, 1):
        sp = line.find(b" ")
        if sp < 0 or line[:sp] != keccak256(line[sp + 1:])[:16].hex().encode():
            return f"line {n}: the checksum does not match"
        try:
            v = json.loads(line[sp + 1:])
        except ValueError:
            return f"line {n}: not JSON"
        if n == 1:
            why = header_ok(v, kind, binds)
            if why:
                return f"line 1: {why}"
            continue
        ops = v.get("ops") if isinstance(v, dict) else None
        if not isinstance(ops, list):
            return f"line {n}: no ops"
        for op in ops:
            if isinstance(op, dict) and isinstance(op.get("add"), int) and "v" in op:
                if op["add"] < nxt or op["add"] in live:
                    return f"line {n}: an id used twice"
                live.add(op["add"]); nxt = op["add"] + 1
            elif isinstance(op, dict) and isinstance(op.get("del"), int):
                if op["del"] not in live:
                    return f"line {n}: removes an unknown id"
                live.discard(op["del"])
            else:
                return f"line {n}: neither add nor del"
        batches += 1
    return (len(lines), batches, len(live), torn)
mode = sys.argv[1]
if mode == "vectors":
    print(keccak256(b"").hex(), keccak256(b"abc").hex())
elif mode == "frame":
    for line in sys.stdin:
        sys.stdout.buffer.write(framed(json.loads(line)))
elif mode == "check":
    # check STATE_DIR [DEPLOYMENT_JSON]: the salt, then every *.journal and *.jsonl below STATE_DIR.
    d = sys.argv[2]
    dep = None
    if len(sys.argv) > 3:
        try:
            with open(sys.argv[3]) as f:
                dep = json.load(f)
        except (OSError, ValueError):
            print("FAIL\tthe deployment manifest does not read as JSON: no journal binding can be checked")
    money = binding(dep)
    # wants.salt: made at the node's first start, read at every start (crates/onyx-api/src/wants.rs:
    # 32 bytes, else the start fails); a new one would change every salted account hash.
    salt = os.path.join(d, "wants.salt")
    if not os.listdir(d):
        print("FAIL\tstate/ is empty: a node that ever started holds at least wants.salt (a lost state folder?)")
    elif not os.path.isfile(salt):
        print("FAIL\tstate/wants.salt is missing: the node would make a new salt, and every salted account hash would change")
    elif os.path.getsize(salt) != 32:
        print("FAIL\tstate/wants.salt is not 32 bytes: the node refuses to start")
    else:
        print("PASS\tstate/wants.salt: 32 bytes, as the node reads it")
    names = sorted(os.path.relpath(os.path.join(r, f), d) for r, _, fs in os.walk(d) for f in fs
                   if f.endswith((".journal", ".jsonl")))
    for f in names:
        base = os.path.basename(f)
        if base in FRAMED:
            kind, is_money = FRAMED[base]
            if is_money and money is None:
                print(f"FAIL\tstate/{f}: a payments journal, but the deployment names no OnyxRevenue it could belong to")
                continue
            r = check(os.path.join(d, f), kind, (money,) if is_money else (NO_CHAIN,))
        else:
            # Another journal of this format (its first line a framed header): bound to no chain or to
            # the deployment's. Others (the want board's wants.journal, the prepaid books) are plain
            # JSON lines the node reads itself.
            with open(os.path.join(d, f), "rb") as h:
                first = h.readline().rstrip(b"\n")
            sp = first.find(b" ")
            if sp < 0 or first[:sp] != keccak256(first[sp + 1:])[:16].hex().encode():
                print(f"NOTE\tstate/{f}: not a checksummed journal (read by the node itself), not checked here")
                continue
            r = check(os.path.join(d, f), None, tuple(b for b in (NO_CHAIN, money) if b))
        if isinstance(r, str):
            print(f"FAIL\tstate/{f}: {r} (the node would refuse to open it)")
        else:
            print(f"PASS\tstate/{f}: {r[0]} line{'' if r[0] == 1 else 's'} ({'a header and ' if r[0] else 'empty, '}{r[1]} batches, {r[2]} live entries), "
                  f"every checksum good, bound as the node binds it{', a torn last line the node cuts off' if r[3] else ''}")
PY
)

# --full, the shards: PORT BULK_DIR. Every hot and warm shard tiers.json names is read back with
# GET /v1/bulk/shards/0x{root} on 127.0.0.1:PORT by 3 threads; each must answer 200 with as many
# bytes as its file. Every shard's outcome is caught and counted, whatever goes wrong with it (a
# missing file, a dropped connection, anything else: a thread that died on one used to leave its
# shards unread while the check passed, review BAK-R2-L2), and the check passes only when every
# shard was read (done == total), none failed and the bytes read equal the files' bytes. One
# PASS or FAIL line.
PYFULL=$(cat <<'PY'
import http.client, json, os, sys, threading, time
port, bulk = int(sys.argv[1]), sys.argv[2]
tiers = json.load(open(os.path.join(bulk, "tiers.json")))
todo = [(r, t.split(":", 1)[1]) for r, t in tiers.items() if t.startswith("bulk:") and t != "bulk:cold"]
total = len(todo)
lock = threading.Lock()
bad, done, nbytes, want = [], [0], [0], [0]
def one(c, root, where):
    """(connection, why it failed or None, bytes read, bytes of the file)."""
    size = os.path.getsize(os.path.join(bulk, where, "bulk", root))
    for _ in range(3):
        try:
            if c is None:
                c = http.client.HTTPConnection("127.0.0.1", port, timeout=300)
            c.request("GET", f"/v1/bulk/shards/0x{root}")
            r = c.getresponse()
            body = r.read()
        except (OSError, http.client.HTTPException):
            if c is not None:
                c.close()
            c = None
            time.sleep(1)
            continue
        if r.status != 200:
            return c, str(r.status), len(body), size
        return c, None if len(body) == size else f"{len(body)} of {size} bytes", len(body), size
    return c, "no answer", 0, size
def work():
    c = None
    while True:
        with lock:
            if not todo:
                return
            root, where = todo.pop()
        try:
            c, why, got, size = one(c, root, where)
        except Exception as e:  # noqa: BLE001 - every shard's failure is counted, never lost
            c, why, got, size = None, type(e).__name__, 0, 0
        with lock:
            done[0] += 1
            nbytes[0] += got
            want[0] += size
            if why:
                bad.append(f"0x{root[:12]} ({why})")
t0 = time.time()
ts = [threading.Thread(target=work) for _ in range(3)]
for t in ts:
    t.start()
for t in ts:
    t.join()
secs = time.time() - t0
if bad or done[0] != total or nbytes[0] != want[0]:
    print(f"FAIL\tfull: {len(bad)} of {total} shards did not read back whole through the node, {done[0]} read, "
          f"{nbytes[0]} of {want[0]} bytes: {', '.join(bad[:5])}")
else:
    print(f"PASS\tfull: all {total} shards tiers.json names read back through the node, each hashing to its name "
          f"({nbytes[0]} bytes, every file's size, in {secs:.0f} s)")
PY
)

# make_config ETC SCRATCH PORT RPC: the local node.env (allow-list, see the header) as
# SCRATCH/node.env (0600) and the deployment's book, chain and OnyxRevenue in
# SCRATCH/deployment-pick.json. Prints NOTE and FAIL lines (setting names, never a value); after a
# FAIL (a file the node needs that no snapshot holds, review BAK-R2-L4) it writes no config.
make_config() {
  python3 -I - "$@" <<'PY'
import json, os, sys
etc, scratch, port, rpc = sys.argv[1:5]
env = {}
with open(os.path.join(etc, "node.env"), encoding="utf-8") as f:
    for raw in f:
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        k, v = k.strip(), v.strip()
        if len(v) >= 2 and v[0] == v[-1] and v[0] in "\"'":
            v = v[1:-1]
        env[k] = v
# Taken from the snapshot: what the store, the catalog and the indexer need to load as on the
# server, and limits. Everything else (addresses, the chain node, payments, intake, keys, salted
# labels, storage-node, relay and paid settings, any setting this list does not know) is left out.
ALLOW = {"ONYX_ENV", "ONYX_DEPLOYMENT", "ONYX_CATALOG", "ONYX_RIGHTS", "ONYX_ALLOW_TEST_LABELS",
         "ONYX_OBSERVATIONS", "ONYX_OBSERVATION_TEMPLATES", "ONYX_SIGNED_REPORTS", "ONYX_TRACK_PUBLICATIONS",
         "ONYX_BULK_DIR", "ONYX_BULK_WARM_DIR", "ONYX_BULK_QUOTA_GIB", "ONYX_BULK_MAX_DATASETS",
         "ONYX_BULK_MAX_GIB", "ONYX_BULK_WARM_AFTER_DAYS", "ONYX_BULK_FORMAT2",
         "ONYX_INDEXER", "ONYX_INDEX_SPAN", "ONYX_INDEX_OVERLAP", "ONYX_INDEX_EVERY_SECS",
         "ONYX_INDEX_RESCAN_POLLS", "ONYX_INDEX_RESCAN_SECS", "ONYX_INDEX_MAX_JUMP", "ONYX_INDEX_STATE",
         "ONYX_INDEX_HEALTH", "ONYX_GATE_REFRESH_SECS", "ONYX_HEADER_READ_TIMEOUT_SECS",
         "ONYX_WRITE_STALL_SECS", "ONYX_MAX_CONNECTIONS", "ONYX_MAX_CONNS_PER_IP",
         "ONYX_USDC_EIP712_NAME", "ONYX_USDC_EIP712_VERSION", "ONYX_MAX_FEE_GWEI", "ONYX_MAX_GAS"}
COPIED = ("bulk", "state", "cas")  # what of /var/lib/onyx a snapshot holds
def mapped(k, v):
    """(value in the copy, None) or (None, why it is left out). Names only, never a value."""
    out = []
    for item in v.split(","):
        item = item.strip()
        if item.startswith("/etc/onyx/"):
            rel = os.path.normpath(item[len("/etc/onyx/"):])
            p = os.path.join(etc, rel)
            if rel.startswith("..") or not os.path.exists(p):
                return None, f"FAIL\t{k} names a file under /etc/onyx that is not in the snapshot"
            out.append(p)
        elif item.startswith("/var/lib/onyx/"):
            rel = os.path.normpath(item[len("/var/lib/onyx/"):])
            if rel.split("/")[0] not in COPIED:
                return None, f"FAIL\t{k} names a path under /var/lib/onyx outside bulk/, state/ and cas/, which no snapshot holds"
            out.append(os.path.join(scratch, rel))
        elif "://" in item:
            return None, "address"  # an address (a node URL may carry a key): left out
        elif item.startswith("/"):
            # A file the server needs that is in no snapshot (review BAK-R2-L4).
            return None, f"FAIL\t{k} names a path outside /etc/onyx and /var/lib/onyx, which no snapshot holds"
        else:
            out.append(item)
    return ",".join(out), None
kept, moved, left, fails = {}, 0, [], []
for k, v in env.items():
    if k not in ALLOW or "\n" in v:
        left.append(k)
        continue
    m, why = mapped(k, v)
    if m is None:
        left.append(k)
        if why.startswith("FAIL"):
            fails.append(why)
        continue
    moved += m != v
    kept[k] = m
for k in ("ONYX_DEPLOYMENT", "ONYX_CATALOG", "ONYX_BULK_DIR"):
    if k in kept and (not kept[k].startswith(scratch + "/") or not os.path.exists(kept[k])):
        kept.pop(k)
    if k not in kept and not any(f"\t{k} " in x for x in fails):
        fails.append(f"FAIL\tthe snapshot's node.env names no {k} inside /etc/onyx or /var/lib/onyx")
print(f"NOTE\tlocal config: {len(kept)} settings from the snapshot's node.env on the allow-list ({moved} paths moved "
      f"to the copy), {len(left)} left out" + (f" ({', '.join(sorted(left))})" if left else ""))
for x in fails:
    print(x)
if fails:
    sys.exit(0)
local = {
    "ONYX_LISTEN": f"127.0.0.1:{port}",
    "ONYX_PUBLIC_URL": f"http://127.0.0.1:{port}",
    "ONYX_RPC_URL": rpc,
    "ONYX_PAYMENTS": "off",
    "ONYX_STATE_DIR": os.path.join(scratch, "state"),
    "ONYX_CAS_DIR": os.path.join(scratch, "cas"),
    "ONYX_BULK_ERASE": "off",
    "ONYX_BULK_FREE_PER_MINUTE": "1000000",
    "ONYX_READS_PER_MINUTE": "0",
}
if kept.get("ONYX_INDEXER") == "on":
    local["ONYX_INDEX_RESCAN_CALLS"] = "10"
fd = os.open(os.path.join(scratch, "node.env"), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, "w") as f:
    for k, v in {**kept, **local}.items():
        f.write(f"{k}={v}\n")
# The deployment manifest is public (chain, contract addresses): the drill reads its book from it,
# and the payments journals' binding (chain and OnyxRevenue).
dep = json.load(open(kept["ONYX_DEPLOYMENT"]))
with open(os.path.join(scratch, "deployment-pick.json"), "w") as f:
    json.dump({k: dep.get(k) for k in ("signedBook", "chainId", "revenue")}, f)
print(f"NOTE\tlocal config: {len(local)} settings set for the drill")
PY
}

# node_run COMMAND...: run COMMAND with the drill's environment and nothing else of ours: the
# settings of SCRATCH/node.env go in as environment, never as arguments (a command line can reach
# the journal and `ps`). PATH, and what systemd-run needs to reach the user manager, are kept.
# node_exec replaces the current (sub)shell; node_run runs it in a subshell and waits.
node_exec() {
  while IFS= read -r v; do
    case $v in PATH|XDG_RUNTIME_DIR|DBUS_SESSION_BUS_ADDRESS) ;; *) unset "$v" 2>/dev/null || true;; esac
  done < <(compgen -e)
  export PATH=/usr/bin:/bin
  while IFS= read -r line; do export "${line%%=*}=${line#*=}"; done <"$scratch/node.env"
  exec "$@"
}
node_run() { ( node_exec "$@" ) 8>&- 9>&-; }
# The node in its own scope, under a fixed description and a unit name the trap can stop. The
# backgrounded function is one forked shell that execs systemd-run, which execs the node: $! is the
# node itself (a subshell inside it would make $! a wrapper that SIGTERM kills, the node left
# running). The scope is outside the drill's cgroup, so a SIGKILLed drill leaves it running: it
# gets neither lock descriptor (8, the drill's shared lock; 9, a calling backup's), so it never
# holds the backup folder's lock, and it is stopped after RuntimeMaxSec; the scope's name ends in
# the drill's pid, and the next backup stops a scope whose drill is gone (review BAK-R2-L1).
start_node() {
  scope=onyx-restore-drill-$port-$$
  node_exec "$systemd_run" --user --scope -q --unit="$scope" --description="onyx restore drill node" \
    -p MemoryMax="$mem" -p RuntimeMaxSec="$max_secs" -- "$node_bin" >"$scratch/node.log" 2>&1 8>&- 9>&- &
  pid=$!
}

# live_get PATH OUT SECS: GET PATH from the live node into OUT. Its public URL first; when that
# fails (Caddy, TLS or DNS down while the server runs), the server's own http://127.0.0.1:8080 over
# ssh (a read-only curl, review BAK-R2-L5), and every later request that way too, said once in a
# NOTE line. PATH is /v1/... with URL-escaped parts only, so it is quoted safely for the far shell.
live_via=https
live_get() {
  local path=$1 out=$2 secs=$3 safe='^/v1/[A-Za-z0-9._~%/?=&-]*$'
  [[ $path =~ $safe ]] && [[ $secs =~ ^[0-9]{1,4}$ ]] || { echo "restore-drill: refused a live path" >&2; return 2; }
  if [ "$live_via" = https ] && curl -fsS --connect-timeout 20 -m "$secs" -A "$ua" -o "$out" "$live$path" 2>/dev/null; then
    return 0
  fi
  if "$ssh_cmd" -o BatchMode=yes -o ConnectTimeout=20 "$ssh_host" \
      "curl -fsS -m $secs -A $ua 'http://127.0.0.1:8080$path'" >"$out" 2>/dev/null; then
    if [ "$live_via" = https ]; then
      live_via=ssh
      echo "NOTE the live node's public URL did not answer: comparing with it over ssh $ssh_host (its own 127.0.0.1:8080, read only)"
    fi
    return 0
  fi
  return 1
}

self_test() {
  local t pass=0 failed=0 out rc mark
  t=$(mktemp -d "${TMPDIR:-/tmp}/restore-drill-test.XXXXXX")
  # shellcheck disable=SC2064
  trap "rm -rf '$t'" EXIT
  ok() { echo "ok: $1"; pass=$((pass + 1)); }
  bad() { echo "FAIL: $1"; failed=$((failed + 1)); }
  check() { if eval "$2"; then ok "$1"; else bad "$1"; fi; }
  mark="planted-$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')"

  out=$(python3 -I -c "$PYJ" vectors)
  check "keccak-256 of \"\" and \"abc\" are the known values (the node's hash)" '[ "$out" = "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470 4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45" ]'

  # BAK-R1-L3: the journals the drill's node does not open are read as the node would read them.
  local st=$t/state
  mkdir -p "$st"
  head -c 32 /dev/urandom >"$st/wants.salt"
  printf '%s\n' '{"onyxJournal":2,"kind":"onyx-submissions","chainId":0,"contract":"0x0000000000000000000000000000000000000000"}' \
    "{\"ops\":[{\"add\":0,\"v\":{\"note\":\"$mark\"}}]}" "{\"ops\":[{\"add\":1,\"v\":{\"note\":\"$mark\"}},{\"del\":0}]}" \
    | python3 -I -c "$PYJ" frame >"$st/submissions.journal"
  printf '0123 {"ops":[{"add":' >>"$st/submissions.journal"
  out=$(python3 -I -c "$PYJ" check "$st")
  check "a good intake journal passes, a torn last line allowed, no content printed (BAK-R1-L3)" '[[ $out == *"PASS"*"state/submissions.journal: 3 lines (a header and 2 batches, 1 live entries)"*"torn last line"* ]] && [[ $out != *FAIL* ]] && [[ $out != *"$mark"* ]]'
  python3 -I - "$st/submissions.journal" <<'PY'
import sys
p = sys.argv[1]
b = bytearray(open(p, "rb").read())
i = b.index(b'"note"')
b[i + 2] ^= 1
open(p, "wb").write(b)
PY
  out=$(python3 -I -c "$PYJ" check "$st")
  check "a damaged line in it fails the drill, no content printed (BAK-R1-L3)" '[[ $out == *"FAIL"*"line 2: the checksum does not match"* ]] && [[ $out != *"$mark"* ]]'
  printf '%s\n' '{"onyxJournal":2,"kind":"onyx-receipts","chainId":84532,"contract":"0x0000000000000000000000000000000000000001"}' \
    | python3 -I -c "$PYJ" frame >"$st/submissions.journal"
  out=$(python3 -I -c "$PYJ" check "$st")
  check "an intake journal with another header fails (BAK-R1-L3)" '[[ $out == *"FAIL"*"state/submissions.journal: line 1: another kind of journal"* ]]'

  # BAK-R2-M2: the salt is required; the money journals (*.jsonl, state/self-settle/ too) are read
  # and each must be bound to the snapshot's deployment: its chain and OnyxRevenue, lowercase.
  local st2=$t/state2 dep=$t/dep.json rev=0x00000000000000000000000000000000000000Ab
  mkdir -p "$st2/self-settle"
  head -c 32 /dev/urandom >"$st2/wants.salt"
  echo "{\"chainId\":84532,\"revenue\":\"$rev\"}" >"$dep"
  frame_journal() { # FILE KIND CHAIN CONTRACT: a header and two batches holding the marker
    printf '%s\n' "{\"onyxJournal\":2,\"kind\":\"$2\",\"chainId\":$3,\"contract\":\"$4\"}" \
      "{\"ops\":[{\"add\":0,\"v\":{\"note\":\"$mark\"}}]}" "{\"ops\":[{\"add\":1,\"v\":{\"note\":\"$mark\"}}]}" \
      | python3 -I -c "$PYJ" frame >"$1"
  }
  frame_journal "$st2/receipts.jsonl" onyx-receipts 84532 "${rev,,}"
  frame_journal "$st2/settle-intents.jsonl" onyx-settle-intents 84532 "${rev,,}"
  frame_journal "$st2/self-settle/pending-settles.jsonl" onyx-pending-settles 84532 "${rev,,}"
  echo "{\"account\":\"0x00\",\"want\":\"$mark\",\"at\":1}" >"$st2/wants.journal"
  out=$(python3 -I -c "$PYJ" check "$st2" "$dep")
  check "receipts.jsonl, settle-intents.jsonl and self-settle/pending-settles.jsonl are read, each bound to the deployment's chain and OnyxRevenue, no content printed (BAK-R2-M2)" '[[ $out == *"PASS"*"state/receipts.jsonl: 3 lines (a header and 2 batches, 2 live entries), every checksum good, bound as the node binds it"* ]] && [[ $out == *"PASS"*"state/settle-intents.jsonl: 3 lines"* ]] && [[ $out == *"PASS"*"state/self-settle/pending-settles.jsonl: 3 lines"* ]] && [[ $out == *"NOTE"*"state/wants.journal: not a checksummed journal"* ]] && [[ $out != *FAIL* ]] && [[ $out != *"$mark"* ]]'
  frame_journal "$st2/settle-intents.jsonl" onyx-settle-intents 84532 0x0000000000000000000000000000000000000002
  out=$(python3 -I -c "$PYJ" check "$st2" "$dep")
  check "a settle-intents.jsonl bound to another contract fails (BAK-R2-M2)" '[[ $out == *"FAIL"*"state/settle-intents.jsonl: line 1: bound to another chain or contract than the deployment"* ]]'
  frame_journal "$st2/settle-intents.jsonl" onyx-settle-intents 8453 "${rev,,}"
  out=$(python3 -I -c "$PYJ" check "$st2" "$dep")
  check "a settle-intents.jsonl bound to another chain fails (BAK-R2-M2)" '[[ $out == *"FAIL"*"state/settle-intents.jsonl: line 1: bound to another chain"* ]]'
  frame_journal "$st2/settle-intents.jsonl" onyx-settle-intents 84532 "${rev,,}"
  sed -i '2s/"add":0/"add":7/' "$st2/receipts.jsonl"
  out=$(python3 -I -c "$PYJ" check "$st2" "$dep")
  check "a damaged receipts.jsonl fails, no content printed (BAK-R2-M2)" '[[ $out == *"FAIL"*"state/receipts.jsonl: line 2: the checksum does not match"* ]] && [[ $out == *"PASS"*"state/settle-intents.jsonl"* ]] && [[ $out != *"$mark"* ]]'
  echo '{"chainId":84532}' >"$t/dep-norev.json"
  out=$(python3 -I -c "$PYJ" check "$st2" "$t/dep-norev.json")
  check "a money journal with a deployment that names no OnyxRevenue fails (BAK-R2-M2)" '[[ $out == *"FAIL"*"state/settle-intents.jsonl: a payments journal, but the deployment names no OnyxRevenue"* ]]'
  rm -f "$st2/wants.salt"
  out=$(python3 -I -c "$PYJ" check "$st2" "$dep")
  check "a state/ without wants.salt fails (it was a NOTE) (BAK-R2-M2)" '[[ $out == "FAIL"*"state/wants.salt is missing"* ]]'
  head -c 31 /dev/urandom >"$st2/wants.salt"
  out=$(python3 -I -c "$PYJ" check "$st2" "$dep")
  check "a wants.salt that is not 32 bytes fails (BAK-R2-M2)" '[[ $out == "FAIL"*"state/wants.salt is not 32 bytes"* ]]'
  mkdir "$t/state-empty"
  out=$(python3 -I -c "$PYJ" check "$t/state-empty" "$dep")
  check "an empty state/ fails (it was a NOTE) (BAK-R2-M2)" '[[ $out == "FAIL"*"state/ is empty"* ]]'

  # BAK-R1-M4: only allow-listed settings reach the node, through its environment only.
  local snap=$t/backups/2026-01-01T000000Z
  mkdir -p "$snap/etc-onyx" "$snap/bulk" "$t/scratch/etc"
  cat >"$t/scratch/etc/node.env" <<EOF
ONYX_ENV=testnet
ONYX_RIGHTS=fallback
ONYX_BULK_DIR=/var/lib/onyx/bulk
ONYX_BULK_FORMAT2=on
ONYX_DEPLOYMENT=/etc/onyx/deployment.json
ONYX_CATALOG=/etc/onyx/catalog.json
ONYX_INDEXER=on
ONYX_INDEX_HEALTH=summary
ONYX_RPC_URL=https://node.invalid/$mark
ONYX_RPC_URL_FILE=/etc/onyx/rpc-$mark
ONYX_OPERATOR_KEY_FILE=/etc/onyx/operator.key
ONYX_STORAGE_KEY_FILE=/etc/onyx/storage/n1.key
ONYX_SUBMISSIONS=/var/lib/onyx/state/submissions.journal
ONYX_SALTED_LABELS=/etc/onyx/labels-$mark.json
ONYX_X402_FACILITATOR=https://pay.invalid/$mark
ONYX_PAYMENTS=self-settle
ONYX_SOMETHING_NEW=$mark
ONYX_OBSERVATION_TEMPLATES=m=https://x.invalid/$mark
EOF
  echo '{"signedBook":"0x0000000000000000000000000000000000000001","chainId":84532}' >"$t/scratch/etc/deployment.json"
  echo '{}' >"$t/scratch/etc/catalog.json"
  mkdir -p "$t/scratch/bulk" "$t/scratch/state" "$t/scratch/cas"
  out=$(make_config "$t/scratch/etc" "$t/scratch" 4242 https://sepolia.base.org 2>&1) && rc=0 || rc=$?
  local keys
  keys=$(cut -d= -f1 "$t/scratch/node.env" | LC_ALL=C sort | paste -sd' ')
  check "the local config takes allow-listed settings only, paths moved into the copy (BAK-R1-M4)" '[ "$rc" = 0 ] && [ "$keys" = "ONYX_BULK_DIR ONYX_BULK_ERASE ONYX_BULK_FORMAT2 ONYX_BULK_FREE_PER_MINUTE ONYX_CAS_DIR ONYX_CATALOG ONYX_DEPLOYMENT ONYX_ENV ONYX_INDEXER ONYX_INDEX_HEALTH ONYX_INDEX_RESCAN_CALLS ONYX_LISTEN ONYX_PAYMENTS ONYX_PUBLIC_URL ONYX_READS_PER_MINUTE ONYX_RIGHTS ONYX_RPC_URL ONYX_STATE_DIR" ] && grep -qx "ONYX_BULK_DIR=$t/scratch/bulk" "$t/scratch/node.env" && grep -qx "ONYX_PAYMENTS=off" "$t/scratch/node.env" && grep -qx "ONYX_RPC_URL=https://sepolia.base.org" "$t/scratch/node.env"'
  check "no left-out value is in the local config or the output (BAK-R1-M4)" '! grep -qF "$mark" "$t/scratch/node.env" && [[ $out != *"$mark"* ]] && [[ $out == *"8 settings from the snapshot"*"10 left out (ONYX_OBSERVATION_TEMPLATES, ONYX_OPERATOR_KEY_FILE, ONYX_PAYMENTS, ONYX_RPC_URL, ONYX_RPC_URL_FILE, ONYX_SALTED_LABELS, ONYX_SOMETHING_NEW, ONYX_STORAGE_KEY_FILE, ONYX_SUBMISSIONS, ONYX_X402_FACILITATOR)"* ]] && [[ $out != *FAIL* ]]'

  # BAK-R2-L4: an allow-listed setting naming a file no snapshot holds fails, naming the setting only.
  mkdir -p "$t/scratch2/bulk" "$t/scratch2/state" "$t/scratch2/cas"
  cp -a "$t/scratch/etc" "$t/scratch2/etc"
  printf '%s\n' "ONYX_TRACK_PUBLICATIONS=/opt/elsewhere/$mark.borsh" "ONYX_OBSERVATIONS=/etc/onyx/observations-$mark" \
    "ONYX_INDEX_STATE=/var/lib/onyx/other-$mark/index.json" >>"$t/scratch2/etc/node.env"
  out=$(make_config "$t/scratch2/etc" "$t/scratch2" 4243 https://sepolia.base.org 2>&1) && rc=0 || rc=$?
  check "settings naming a path outside the backup, a /etc/onyx file not in it, or /var/lib/onyx outside bulk, state and cas each FAIL by name, never value, and no config is written (BAK-R2-L4)" '[ "$rc" = 0 ] && grep -qx "FAIL.ONYX_TRACK_PUBLICATIONS names a path outside /etc/onyx and /var/lib/onyx, which no snapshot holds" <<<"$out" && grep -qx "FAIL.ONYX_OBSERVATIONS names a file under /etc/onyx that is not in the snapshot" <<<"$out" && grep -qx "FAIL.ONYX_INDEX_STATE names a path under /var/lib/onyx outside bulk/, state/ and cas/, which no snapshot holds" <<<"$out" && [[ $out != *"$mark"* ]] && [ ! -e "$t/scratch2/node.env" ]'

  # A systemd-run stand-in that records its arguments and runs the command after "--"; a node
  # stand-in that records its arguments and environment.
  cat >"$t/systemd-run" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$@" >"$SR_LOG"
while [ "$1" != -- ]; do shift; done
shift
exec "$@"
SH
  cat >"$t/node" <<'SH'
#!/usr/bin/env bash
echo "$#" >"$NODE_ARGS"
echo "$$" >"$NODE_PID"
env >"$NODE_ENV"
ls /proc/$$/fd | sort -n | paste -sd' ' >"$NODE_FDS"
SH
  chmod 0755 "$t/systemd-run" "$t/node"
  local scratch=$t/scratch node_bin=$t/node pid port=4242 scope
  # The recorders' paths ride in node.env here only for the test, like any setting would.
  printf 'SR_LOG=%s\nNODE_ARGS=%s\nNODE_ENV=%s\nNODE_PID=%s\nNODE_FDS=%s\n' "$t/sr.log" "$t/node.args" "$t/node.envout" "$t/node.pid" "$t/node.fds" >>"$t/scratch/node.env"
  local systemd_run=$t/systemd-run max_secs=10800
  export DRILL_CANARY=$mark
  # The drill's shared lock is fd 8, a calling backup's lock fd 9: the node must get neither.
  exec 8>>"$t/lock8" 9>>"$t/lock9"
  start_node
  exec 8>&- 9>&-
  unset DRILL_CANARY
  wait "$pid" || true
  check "systemd-run gets a fixed description and no setting on its command line (BAK-R1-M4)" 'grep -qx -- "--description=onyx restore drill node" "$t/sr.log" && ! grep -q "ONYX_\|=/" "$t/sr.log" && [ "$(cat "$t/node.args")" = 0 ]'
  check "the pid the drill stops is the node's own process, not a wrapper shell" '[ "$(cat "$t/node.pid")" = "$pid" ]'
  check "the node holds neither lock descriptor (8, 9) and its scope stops after RuntimeMaxSec (BAK-R2-L1)" '[ -s "$t/node.fds" ] && [[ " $(cat "$t/node.fds") " != *" 8 "* ]] && [[ " $(cat "$t/node.fds") " != *" 9 "* ]] && grep -qx "RuntimeMaxSec=10800" "$t/sr.log" && grep -qx -- "--unit=onyx-restore-drill-4242-$$" "$t/sr.log"'
  check "the node gets the settings as environment and nothing else of the caller's (BAK-R1-M4)" 'grep -qx "ONYX_BULK_DIR=$t/scratch/bulk" "$t/node.envout" && ! grep -qF "$mark" "$t/node.envout" && ! grep -q "^HOME=\|^DRILL_CANARY=" "$t/node.envout"'

  # A stand-in for the node's and the live node's HTTP routes on 127.0.0.1: /v1/health and
  # /v1/bulk/shards/0x{root} (the shard file under BULK/hot/bulk, one root answered a byte short).
  cat >"$t/server.py" <<'PY'
import http.server, os, sys
portfile, bulk, short = sys.argv[1:4]
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass
    def answer(self, code, body=b""):
        self.send_response(code)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def do_GET(self):
        if self.path == "/v1/health":
            return self.answer(200, b'{"status":"ok","via":"public"}')
        if self.path.startswith("/v1/bulk/shards/0x"):
            root = self.path[len("/v1/bulk/shards/0x"):]
            p = os.path.join(bulk, "hot", "bulk", root)
            if os.path.isfile(p):
                body = open(p, "rb").read()
                return self.answer(200, body[:-1] if root == short else body)
        self.answer(404)
s = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
with open(portfile + ".tmp", "w") as f:
    f.write(str(s.server_address[1]))
os.rename(portfile + ".tmp", portfile)
s.serve_forever()
PY
  local fb=$t/fullbulk srv sport r1 r2 r3 i
  r1=$(printf '1%.0s' {1..64}) r2=$(printf '2%.0s' {1..64}) r3=$(printf '3%.0s' {1..64})
  mkdir -p "$fb/hot/bulk"
  echo "{\"$r1\":\"bulk:hot\",\"$r2\":\"bulk:hot\",\"$r3\":\"bulk:hot\"}" >"$fb/tiers.json"
  python3 -I "$t/server.py" "$t/srv.port" "$fb" "$r3" & srv=$!
  # The server's start: the port file appears (a cap of 30 s, far above any start).
  for i in $(seq 1 300); do [ -s "$t/srv.port" ] && break; sleep 0.1; done
  sport=$(cat "$t/srv.port")

  # BAK-R2-L2: every shard of the --full check is counted, whatever goes wrong with it.
  out=$(python3 -I -c "$PYFULL" "$sport" "$fb" 2>&1) && rc=0 || rc=$?
  check "--full with all 3 shard files missing fails: each shard's error is counted, no thread dies quietly (BAK-R2-L2)" '[[ $out == "FAIL"*"full: 3 of 3 shards did not read back whole through the node, 3 read, 0 of 0 bytes"*"(FileNotFoundError)"* ]] && [[ $out != *Traceback* ]]'
  echo "rows of shard one" >"$fb/hot/bulk/$r1" && echo "rows of shard two" >"$fb/hot/bulk/$r2"
  out=$(python3 -I -c "$PYFULL" "$sport" "$fb" 2>&1) && rc=0 || rc=$?
  check "--full with one shard file missing fails (BAK-R2-L2)" '[[ $out == "FAIL"*"full: 1 of 3 shards did not read back whole through the node, 3 read, 36 of 36 bytes: 0x333333333333 (FileNotFoundError)"* ]]'
  echo "rows of shard three" >"$fb/hot/bulk/$r3"
  out=$(python3 -I -c "$PYFULL" "$sport" "$fb" 2>&1) && rc=0 || rc=$?
  check "--full with a shard answered a byte short fails (BAK-R2-L2)" '[[ $out == "FAIL"*"full: 1 of 3 shards did not read back whole through the node, 3 read, 55 of 56 bytes: 0x333333333333 (19 of 20 bytes)"* ]]'
  kill "$srv" 2>/dev/null; wait "$srv" 2>/dev/null || true
  rm -f "$t/srv.port"
  python3 -I "$t/server.py" "$t/srv.port" "$fb" none & srv=$!
  for i in $(seq 1 300); do [ -s "$t/srv.port" ] && break; sleep 0.1; done
  sport=$(cat "$t/srv.port")
  out=$(python3 -I -c "$PYFULL" "$sport" "$fb" 2>&1) && rc=0 || rc=$?
  check "--full passes only with every shard read whole and the bytes adding up (BAK-R2-L2)" '[[ $out == "PASS"*"full: all 3 shards tiers.json names read back through the node, each hashing to its name (56 bytes, every file"* ]]'

  # BAK-R2-L5: the live node's reads go over ssh to its own 127.0.0.1:8080 when its public URL fails.
  cat >"$t/ssh" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$SSH_LOG"
echo '{"status":"ok","via":"ssh"}'
SH
  chmod 0755 "$t/ssh"
  local live=http://127.0.0.1:$sport ssh_cmd=$t/ssh ssh_host=test-host live_via=https deadport
  export SSH_LOG=$t/ssh.log
  live_get /v1/health "$t/lg1" 5 >"$t/lg1.out" && rc=0 || rc=$?
  check "a live read goes to the public URL while it answers, no ssh (BAK-R2-L5)" '[ "$rc" = 0 ] && grep -q "\"via\":\"public\"" "$t/lg1" && [ ! -e "$t/ssh.log" ] && [ "$live_via" = https ] && [ ! -s "$t/lg1.out" ]'
  deadport=$(python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
  live=http://127.0.0.1:$deadport
  live_get "/v1/bulk?limit=1" "$t/lg2" 5 >"$t/lg2.out" && rc=0 || rc=$?
  check "when the public URL fails, the read goes over ssh to the server's own 127.0.0.1:8080 with a read-only curl, said once (BAK-R2-L5)" '[ "$rc" = 0 ] && grep -q "\"via\":\"ssh\"" "$t/lg2" && grep -qxF -- "-o BatchMode=yes -o ConnectTimeout=20 test-host curl -fsS -m 5 -A onyx-restore-drill '"'"'http://127.0.0.1:8080/v1/bulk?limit=1'"'"'" "$t/ssh.log" && [ "$live_via" = ssh ] && grep -q "^NOTE the live node.s public URL did not answer: comparing with it over ssh test-host" "$t/lg2.out"'
  live=http://127.0.0.1:$sport
  live_get /v1/health "$t/lg3" 5 >"$t/lg3.out" && rc=0 || rc=$?
  check "once on ssh, later reads stay on ssh and say nothing more (BAK-R2-L5)" '[ "$rc" = 0 ] && grep -q "\"via\":\"ssh\"" "$t/lg3" && [ ! -s "$t/lg3.out" ] && [ "$(wc -l <"$t/ssh.log")" = 2 ]'
  live_get "/v1/x'y;id" "$t/lg4" 5 >"$t/lg4.out" 2>&1 && rc=0 || rc=$?
  check "a live path with a character the far shell would read is refused, no ssh (BAK-R2-L5)" '[ "$rc" = 2 ] && [ "$(wc -l <"$t/ssh.log")" = 2 ]'
  unset SSH_LOG
  kill "$srv" 2>/dev/null; wait "$srv" 2>/dev/null || true

  # BAK-R1-L5: a drill holds the backup folder's lock shared; a backup run holding it refuses the drill.
  mkdir -m 0700 -p "$snap/state" "$snap/cas" && : >"$snap/SNAPSHOT.json" && : >"$snap/etc-onyx/node.env"
  out=$(flock "$t/backups/.lock" "$self" "$snap" 2>&1) && rc=0 || rc=$?
  check "a drill while a backup run holds the lock is refused before it copies anything (BAK-R1-L5)" '[ "$rc" = 2 ] && [[ $out == *"holds $t/backups/.lock"* ]] && [ -z "$(find "$t/backups" -maxdepth 1 -name ".drill.*")" ]'
  out=$(flock -s "$t/backups/.lock" sh -c 'flock -n -x "$1" true' x "$t/backups/.lock" 2>&1) && rc=0 || rc=$?
  check "and a drill's shared hold keeps a backup run's exclusive lock out (BAK-R1-L5)" '[ "$rc" != 0 ]'

  echo "restore-drill self-test: $pass passed, $failed failed"
  [ "$failed" = 0 ]
}

full=
case ${1:-} in
  --self-test) [ $# = 1 ] || { echo "usage: $0 --self-test" >&2; exit 2; }; self_test; exit;;
  --full) full=1; shift;;
esac
[ $# = 1 ] || { echo "usage: $0 [--full] SNAPSHOT_DIR | --self-test" >&2; exit 2; }
snap=$(realpath -e -- "$1")
for p in bulk state cas etc-onyx SNAPSHOT.json etc-onyx/node.env; do
  [ -e "$snap/$p" ] || { echo "restore-drill: $snap is not a backup-node.sh snapshot (no $p)" >&2; exit 2; }
done
# The backup folder's lock, shared: no backup run prunes this snapshot while the drill reads it.
lockf=$(dirname "$snap")/.lock
if [ "${ONYX_DRILL_UNDER_BACKUP:-}" != 1 ]; then
  exec 8>>"$lockf"
  flock -s -n 8 || { echo "restore-drill: a backup-node run holds $lockf: run the drill after it" >&2; exit 2; }
fi
if [ -n "${ONYX_NODE_BIN:-}" ]; then node_src=$ONYX_NODE_BIN; node_from="ONYX_NODE_BIN"
elif [ -f "$snap/server/onyx-node" ]; then node_src=$snap/server/onyx-node; node_from="the snapshot's own binary"
else node_src=$HOME/.cache/onyx-api-target/release/onyx-node; node_from="the local release build"; fi
for b in "$node_src" "$check_bin"; do [ -f "$b" ] || { echo "restore-drill: no such file: $b" >&2; exit 2; }; done
[ -x "$check_bin" ] || { echo "restore-drill: not an executable: $check_bin" >&2; exit 2; }

passed=0
failed=0
pass() { echo "PASS $1"; passed=$((passed + 1)); }
fail() { echo "FAIL $1"; failed=$((failed + 1)); }
verdicts() { # FILE of "PASS|FAIL|NOTE<tab>text" lines
  while IFS=$'\t' read -r verdict what; do
    case $verdict in PASS) pass "$what";; NOTE) echo "NOTE $what";; *) fail "$what";; esac
  done <"$1"
}
pid=
scope=
scratch=
completed=
finish() {
  local st=$?
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill -KILL "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  # Whatever is left in the node's scope goes with it.
  [ -z "$scope" ] || systemctl --user stop "$scope.scope" >/dev/null 2>&1 || true
  [ -z "$scratch" ] || rm -rf -- "$scratch"
  if [ -n "$completed" ] && [ "$failed" = 0 ]; then
    echo "DRILL PASS: $passed checks passed ($(basename "$snap")${full:+, --full})"
  else
    echo "DRILL FAIL: $failed of $((passed + failed)) checks failed$([ -n "$completed" ] || echo ", stopped early (exit $st)") ($(basename "$snap"))"
    [ "$st" != 0 ] || st=1
  fi
  exit "$st"
}
trap finish EXIT
trap 'exit 130' INT TERM

scratch=$(mktemp -d "$(dirname "$snap")/.drill.XXXXXX")
meta() { find "$snap/bulk" -printf '%P\t%y\t%i\t%s\t%T@\t%m\n' | LC_ALL=C sort | sha256sum | cut -d' ' -f1; }
meta_before=$(meta)
cp -al -- "$snap/bulk" "$scratch/bulk"
find "$scratch/bulk" -type f -name '*.tmp' -delete
cp -a -- "$snap/state" "$scratch/state"
cp -a -- "$snap/cas" "$scratch/cas"
cp -a -- "$snap/etc-onyx" "$scratch/etc"
install -m 0700 -- "$node_src" "$scratch/onyx-node"
node_bin=$scratch/onyx-node
mkdir -p "$scratch/rows"

# A free loopback port.
port=$(python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
local_url=http://127.0.0.1:$port
make_config "$scratch/etc" "$scratch" "$port" "$rpc" >"$scratch/config.txt"
verdicts "$scratch/config.txt"
# A setting naming a file no snapshot holds: the node would not run as the server's (BAK-R2-L4).
[ "$failed" = 0 ] || exit 1

# The book the anchors are checked against, from the snapshot's deployment manifest.
book=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["signedBook"] or "")' "$scratch/deployment-pick.json")
[[ $book =~ ^0x[0-9a-fA-F]{40}$ ]] || { fail "the snapshot's deployment manifest names no signedBook"; exit 1; }
chain_id=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["chainId"])' "$scratch/deployment-pick.json")
[ "$chain_id" = 84532 ] || { fail "the snapshot's deployment is chain $chain_id, not Base Sepolia (84532)"; exit 1; }

# The server's build.
want_sha=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1])).get("serverBinarySha256") or "")' "$snap/SNAPSHOT.json")
have_sha=$(sha256sum "$node_bin" | cut -d' ' -f1)
if [ -z "$want_sha" ]; then
  fail "SNAPSHOT.json names no server binary hash"
elif [ "$want_sha" = "$have_sha" ]; then
  pass "onyx-node ($node_from) is the server's build (sha256 ${have_sha:0:12})"
else
  fail "onyx-node ($node_from) ${have_sha:0:12} is not the server's build ${want_sha:0:12}"
fi

# The salt and the state journals, read before the node opens anything; the payments journals'
# binding against the snapshot's deployment (chain and OnyxRevenue).
python3 -I -c "$PYJ" check "$scratch/state" "$scratch/deployment-pick.json" >"$scratch/journals.txt"
verdicts "$scratch/journals.txt"

if out=$(node_run "$node_bin" --check-config 2>&1); then
  pass "onyx-node --check-config: $(printf '%s' "$out" | tail -n 1 | cut -c1-200)"
else
  fail "onyx-node --check-config: $(printf '%s' "$out" | tail -n 3 | paste -sd' ' | cut -c1-400)"
  exit 1
fi

t_start=$SECONDS
start_node
up=
while [ $((SECONDS - t_start)) -lt "$start_cap" ]; do
  if ! kill -0 "$pid" 2>/dev/null; then break; fi
  if curl -fsS -m 5 -o "$scratch/health.json" "$local_url/v1/health" 2>/dev/null; then up=1; break; fi
  sleep 2
done
if [ -z "$up" ]; then
  fail "the local node did not answer /v1/health within ${start_cap} s; its last log lines:"
  tail -n 15 "$scratch/node.log" | cut -c1-300 | sed 's/^/  /'
  exit 1
fi
live_get /v1/health "$scratch/live-health.json" 30 || { fail "the live node did not answer /v1/health (its public URL, nor over ssh $ssh_host)"; exit 1; }
if hmsg=$(python3 -I - "$scratch/health.json" "$scratch/live-health.json" <<'PY'
import json, sys
a, b = (json.load(open(p)) for p in sys.argv[1:3])
same = all(a.get(k) == b.get(k) for k in ("chainId", "contract", "rights"))
print(f"status {a.get('status')}, chain {a.get('chainId')}, contract {str(a.get('contract'))[:12]}, rights {a.get('rights')}")
sys.exit(0 if same and a.get("status") == "ok" else 1)
PY
); then pass "/v1/health answers after $((SECONDS - t_start)) s ($hmsg), same chain, contract and rights as live"
else fail "/v1/health is not ok or differs from live ($hmsg)"; fi

# Catalogs: each publisher's newest catalog in the snapshot against the live node's catalog of the
# same number (the live node may hold newer ones by now).
curl -fsS -m 120 -o "$scratch/local-list.json" "$local_url/v1/bulk"
live_get "/v1/bulk?limit=1" "$scratch/live-list.json" 30 || { fail "the live node did not answer /v1/bulk"; exit 1; }
# The live node's copy of each catalog number the snapshot holds, numbered as the local list.
python3 -I - "$scratch/local-list.json" >"$scratch/catalog-paths.txt" <<'PY'
import json, sys, urllib.parse
for c in json.load(open(sys.argv[1])).get("catalogs", []):
    print(f"{urllib.parse.quote(c['publisher'], safe='')}/{int(c['number'])}")
PY
i=0
while read -r -u 3 cpath; do
  i=$((i + 1))
  live_get "/v1/bulk/catalogs/$cpath" "$scratch/live-catalog-$i.json" 60 || : >"$scratch/live-catalog-$i.json"
done 3<"$scratch/catalog-paths.txt"
python3 -I - "$scratch/local-list.json" "$scratch/live-list.json" "$scratch" >"$scratch/catalogs.txt" <<'PY'
import json, os, sys
a, b = (json.load(open(p)) for p in sys.argv[1:3])
fetched = {}
for i, c in enumerate(a.get("catalogs", []), 1):
    try:
        with open(os.path.join(sys.argv[3], f"live-catalog-{i}.json")) as f:
            fetched[c["publisher"]] = json.load(f).get("root")
    except (OSError, ValueError, AttributeError):
        fetched[c["publisher"]] = "unreadable"
ca = {c["publisher"]: (c["number"], c["root"]) for c in a.get("catalogs", [])}
cb = {c["publisher"]: (c["number"], c["root"]) for c in b.get("catalogs", [])}
moved = False
for p in sorted(set(ca) | set(cb)):
    if p not in ca:
        print(f"FAIL\tcatalog {p}: the live node has catalog {cb[p][0]}, the snapshot none (published since?)")
        moved = True
        continue
    n, root = ca[p]
    ln = cb.get(p, (None, None))[0]
    if ln == n:
        live_root, note = cb[p][1], "the live node's newest too"
    else:
        moved = True
        live_root = fetched.get(p, "unreadable")
        note = f"the live node is at {ln} now: compared with its catalog {n}"
    ok = live_root == root
    print(f"{'PASS' if ok else 'FAIL'}\tcatalog {p}: number {n}, root {root[:14]}, {'equal to' if ok else 'not'} the live node's ({note})")
ta, tb = a.get("total"), b.get("total")
if ta == tb:
    print(f"PASS\tdatasets: {ta} in the snapshot, {tb} on the live node")
elif moved:
    print(f"NOTE\tdatasets: {ta} in the snapshot, {tb} on the live node (published since the snapshot)")
else:
    print(f"FAIL\tdatasets: {ta} in the snapshot, {tb} on the live node, with the same catalogs")
PY
verdicts "$scratch/catalogs.txt"

# Samples: every early-record dataset, a widened (format-2) onchain-archive dataset, the largest
# onchain-archive dataset and three picked by position. The format is read from each newest
# manifest file (byte 0 of the stored manifest, after the 4-byte Borsh length) and confirmed by the
# node's answer.
python3 -I - "$scratch/local-list.json" "$scratch/bulk/manifests" >"$scratch/samples.txt" <<'PY'
import json, os, re, sys
lst = json.load(open(sys.argv[1]))["datasets"]
mdir = sys.argv[2]
by = {}
for d in lst:
    by.setdefault(d["publisher"], []).append(d)
picked = [("early-record", d["dataset"]) for d in sorted(by.get("early-record", []), key=lambda d: d["dataset"])]
oa = sorted(by.get("onchain-archive", []), key=lambda d: d["dataset"])
def fmt(ds):
    p = os.path.join(mdir, "onchain-archive", ds)
    vs = [int(m.group(1)) for f in os.listdir(p) if (m := re.fullmatch(r"v(\d+)\.bin", f))] if os.path.isdir(p) else []
    if not vs:
        return None
    with open(os.path.join(p, f"v{max(vs)}.bin"), "rb") as f:
        b = f.read(5)
    return b[4] if len(b) == 5 else None
wide = next((d["dataset"] for d in oa if fmt(d["dataset"]) == 2), None)
chosen = []
if wide:
    chosen.append(wide)
if oa:
    chosen.append(max(oa, key=lambda d: d["bytes"])["dataset"])
    for i in (len(oa) // 4, len(oa) // 2, 3 * len(oa) // 4):
        chosen.append(oa[i]["dataset"])
for d in dict.fromkeys(chosen):
    picked.append(("onchain-archive", d))
for p, d in picked:
    print(f"{p}\t{d}\t{'2' if d == wide else ''}")
PY
n_samples=$(wc -l <"$scratch/samples.txt")
# pipefail-ok: samples.txt is a dozen short lines at most
if cut -f3 "$scratch/samples.txt" | grep -qx 2; then pass "a widened format-2 dataset found in the snapshot"; else fail "no format-2 dataset found in the snapshot's manifests"; fi
if [ "$(cut -f1 "$scratch/samples.txt" | sort -u | wc -l)" = 2 ] && [ "$n_samples" -ge 5 ]; then
  pass "$n_samples sampled datasets over both publishers"
else
  fail "only $n_samples samples, publishers: $(cut -f1 "$scratch/samples.txt" | sort -u | paste -sd,)"
fi

i=0
while IFS=$'\t' read -r -u 3 p d want_fmt; do
  i=$((i + 1))
  q=$(python3 -I -c 'import sys, urllib.parse as u; print(u.quote(sys.argv[1], safe="") + "/" + u.quote(sys.argv[2], safe=""))' "$p" "$d")
  la=$scratch/rows/local-$i.json
  lb=$scratch/rows/live-$i.json
  li=$scratch/rows/live-info-$i.json
  if ! curl -fsS -m 300 -o "$la" "$local_url/v1/bulk/$q/rows?limit=1000"; then fail "$p/$d: the local node did not answer rows"; continue; fi
  if ! live_get "/v1/bulk/$q" "$li" 120; then fail "$p/$d: the live node did not answer the dataset"; continue; fi
  if ! live_get "/v1/bulk/$q/rows?limit=1000" "$lb" 300; then fail "$p/$d: the live node did not answer rows"; continue; fi
  ca=$("$check_bin" --bulk "$la" --publisher "$p" --dataset "$d" --rpc "$rpc" --chain-id 84532 --book "$book" 2>&1) && ra=0 || ra=$?
  cb=$("$check_bin" --bulk "$lb" --publisher "$p" --dataset "$d" --rpc "$rpc" --chain-id 84532 --book "$book" 2>&1) && rb=0 || rb=$?
  if verdict=$(CA=$ca CB=$cb python3 -I - "$la" "$lb" "$li" "$ra" "$rb" "$want_fmt" <<'PY'
import hashlib, json, os, sys
la, lb, li, ra, rb, want_fmt = sys.argv[1:7]
try:
    a, b = json.loads(os.environ["CA"]), json.loads(os.environ["CB"])
except ValueError:
    print(f"onyx-check gave no JSON (exit local {ra}, live {rb})"); sys.exit(1)
local = json.load(open(la))
fmt = local.get("manifest", {}).get("format")
info = json.load(open(li))
live_v = {v["version"]: v for v in info.get("versions", [])}
newest = max(live_v) if live_v else None
v = a.get("version")
checked = ra == "0" and a.get("ok") is True and a.get("chainChecked") is True
problems = []
if not checked:
    problems.append(f"local onyx-check exit {ra} ok={a.get('ok')} chainChecked={a.get('chainChecked')} {str(a.get('error') or '')[:120]}")
lv = live_v.get(v)
if lv is None:
    problems.append(f"the live node holds no version {v}")
elif lv.get("digest") != a.get("digest") or lv.get("root") != a.get("manifestRoot"):
    problems.append("digest or manifest root differs from the live node's for this version")
if newest == v:
    # The same version on both: the live answer must check too and carry the same rows.
    keys = ("digest", "manifestRoot", "version", "rows", "firstKey", "lastKey", "signer")
    diff = [k for k in keys if a.get(k) != b.get(k)]
    if rb != "0" or diff:
        problems.append(f"live onyx-check exit {rb}, differs: {diff}")
    def rows_sha(p):
        return hashlib.sha256(json.dumps(json.load(open(p))["rows"], sort_keys=True).encode()).hexdigest()
    if rows_sha(la) != rows_sha(lb):
        problems.append("rows differ")
    note = "same version on the live node"
else:
    note = f"the live node is at v{newest} now: compared with its v{v}"
if want_fmt and fmt != int(want_fmt):
    problems.append(f"format {fmt}, not {want_fmt}")
print(f"format {fmt}, v{v}, {a.get('rows')} rows, manifest root {str(a.get('manifestRoot'))[:14]}, "
      f"digest {str(a.get('digest'))[:14]} ({note})"
      + ("" if not problems else "; " + "; ".join(problems)))
sys.exit(1 if problems else 0)
PY
  ); then
    pass "$p/$d: onyx-check --bulk passed on chain, digest and root equal to the live node's ($verdict)"
  else
    fail "$p/$d: $verdict"
  fi
done 3<"$scratch/samples.txt"

# --full: the whole store, through the node. Each shard tiers.json names is read back with
# GET /v1/bulk/shards/{root}, which answers only when the file's rows hash to its name (the node's
# FsCas check: the Merkle root of its rows, crates/onyx-coordinator/src/cas.rs key_of); and every
# dataset's newest manifest must have all its shards stored, else the live node must lack them too.
if [ -n "$full" ]; then
  python3 -I -c "$PYFULL" "$port" "$scratch/bulk" >"$scratch/full-shards.txt"
  verdicts "$scratch/full-shards.txt"
  live_get /v1/bulk "$scratch/live-list-all.json" 120 || { fail "full: the live node did not answer /v1/bulk"; exit 1; }
  python3 -I - "$scratch/local-list.json" "$scratch/live-list-all.json" <<'PY' >"$scratch/full-manifests.txt"
import json, sys
a, b = (json.load(open(p))["datasets"] for p in sys.argv[1:3])
live = {(d["publisher"], d["dataset"]): d for d in b}
short, explained, shards = [], 0, 0
for d in a:
    shards += d["shards"]
    if d["stored_shards"] >= d["shards"]:
        continue
    l = live.get((d["publisher"], d["dataset"]))
    if l and l["version"] == d["version"] and l["stored_shards"] < l["shards"]:
        explained += 1  # the live node lacks them too: an upload not finished
    else:
        short.append(f"{d['publisher']}/{d['dataset']} v{d['version']} {d['stored_shards']}/{d['shards']}")
if short:
    print(f"FAIL\tfull: {len(short)} datasets' newest manifests miss shards the live node holds: {', '.join(short[:5])}")
else:
    note = f"; {explained} short on the live node too (an upload not finished)" if explained else ""
    print(f"PASS\tfull: every one of {len(a)} datasets' newest manifests has all its shards ({shards} shards){note}")
PY
  verdicts "$scratch/full-manifests.txt"
fi

# Stop the node (SIGTERM; the unit allows 75 s) and check the snapshot was not changed.
kill -TERM "$pid" 2>/dev/null || true
t_stop=$SECONDS
while kill -0 "$pid" 2>/dev/null && [ $((SECONDS - t_stop)) -lt 90 ]; do sleep 1; done
if kill -0 "$pid" 2>/dev/null; then
  fail "the local node did not stop within 90 s of SIGTERM (killed)"
else
  st=0; wait "$pid" 2>/dev/null || st=$?
  pass "the local node stopped on SIGTERM in $((SECONDS - t_stop)) s (exit $st)"
  pid=
fi
# wants.salt: read by the node at every start, made anew only when missing (crates/onyx-api/src/wants.rs).
want_salt=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["stateSha256"].get("wants.salt") or "")' "$snap/SNAPSHOT.json")
if [ -z "$want_salt" ]; then
  fail "SNAPSHOT.json names no state/wants.salt (the node made a new one: every salted account hash changed)"
elif [ "$(sha256sum "$scratch/state/wants.salt" | cut -d' ' -f1)" = "$want_salt" ]; then
  pass "state/wants.salt was read by the node and is the snapshot's (sha256 equal to SNAPSHOT.json)"
else
  fail "state/wants.salt differs from the snapshot's after the node ran"
fi
if [ "$(meta)" = "$meta_before" ]; then
  pass "the snapshot's bulk store is unchanged (size, mtime, mode, inode of every file)"
else
  fail "a file of the snapshot's bulk store changed during the drill"
fi
completed=1
