#!/usr/bin/env python3
"""The public record of publisher decisions (ADR-0066 item 9, THREATS PUBON-14, S3-R1-L4, S3-R1-SCAN): deploy/publishers/.

Usage: python3 -I scripts/check-publishers.py [--write] [--limits FILE [--max-gib N]]
                                              [--rpc URL] [--records DIR] [--status FILE] [--self-test]

Checks every folder of the record, offline:
- `<publisher id>/decision.json`: a registered publisher, a JSON list, oldest first. An `approved`
  entry names its application file; that file hashes to the entry's `application`, its signature
  recovers `signer`, and its id, origin, keys document, chain and book are the entry's.
- `application-<16 hex>/decision.json`: an application that is not approved (`received`, `refused`,
  `withdrawn`, `expired`), named by its Keccak. It holds only the fields its state allows: a refusal
  keeps the hash, the reason code, the date and, from the tool's report, the blocked signer with
  `blocks_signer_until`. No id, no origin, no data text, no sample.
- No e-mail-shaped text anywhere. One origin has one id; one signer has one id.
- An application `received` more than 5 days ago without a decision is a WARNING, never a failure:
  a clock alone must not turn the build red.
- The status list apps/web/static/publishers.json (the website and `onyx-publish doctor --publishers`
  read it) equals what the record gives; `--write` writes it. A publisher from before the record
  (`registered-before-record`) is listed without its origin. An approval is listed as
  "approved (record)" until a run with `--rpc` found its registration in the book; then "approved".
- The public answers of an approval are `yes` for the lines 7, 8, 9, 10, 11 and 13: no free text,
  and never line 12 (who runs a publisher is not public).
With `--limits FILE` (a node's ONYX_BULK_PUBLISHERS file): every entry is a recorded publisher with
the recorded numbers and state, every approved publisher has its entry, and with `--max-gib` the
grants plus the reserve fit. With `--rpc URL` (the reviewer, after each decision; read-only): the
book's KeyRegistered log in each recorded block equals the record.
- When gitleaks is installed, the repository's secret scanner (.gitleaks.toml) reads the record and
  the status list: a field name it takes for a credential fails here, before the public snapshot.
Exit 0 passed (warnings are printed), 1 a problem (each named), 2 usage. `--self-test` plants a
changed application, a refusal that keeps the data text, contact text, an approval the book does
not show, a late decision and wrong limits.
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RECORDS = os.path.join(ROOT, "deploy", "publishers")
STATUS = os.path.join(ROOT, "apps", "web", "static", "publishers.json")
DECIDE_DAYS = 5
# The checklist lines whose answers are public. Line 12 (the operator grouping) is the reviewer's
# private note: the record never says who runs a publisher (review S3-R1 L5).
ANSWER_LINES = ["7", "8", "9", "10", "11", "13"]
# An approval only this record shows; "approved" is written once a run with --rpc found the
# book's registration for it (review S3-R1 L4).
UNCONFIRMED = "approved (record)"
REASONS = ["name-not-neutral", "name-taken", "origin-unreachable", "origin-overlap", "sample-failed",
           "rights-unclear", "personal-data", "not-hostable", "duplicate", "no-capacity", "no-cosignature"]
TOOL_REASONS = ["malformed", "bad-signature", "wrong-network", "expired", "bad-time", "keys-document",
                "signer-blocked"]
ID_RE = re.compile(r"[a-z0-9_-]+(?:[.:][a-z0-9_-]+)*\Z")
HASH_FOLDER_RE = re.compile(r"application-[0-9a-f]{16}\Z")
ADDR_RE = re.compile(r"0x[0-9a-f]{40}\Z")
H32_RE = re.compile(r"0x[0-9a-f]{64}\Z")
DATE_RE = re.compile(r"\d{4}-\d{2}-\d{2}\Z")
CONTACT_RE = re.compile(r"[^\s@\"']+@[^\s@\"']+\.[^\s@\"']+|mailto:", re.I)

# ---------------------------------------------------------------- Keccak-256 and secp256k1

_RC = [0x0000000000000001, 0x0000000000008082, 0x800000000000808A, 0x8000000080008000, 0x000000000000808B,
       0x0000000080000001, 0x8000000080008081, 0x8000000000008009, 0x000000000000008A, 0x0000000000000088,
       0x0000000080008009, 0x000000008000000A, 0x000000008000808B, 0x800000000000008B, 0x8000000000008089,
       0x8000000000008003, 0x8000000000008002, 0x8000000000000080, 0x000000000000800A, 0x800000008000000A,
       0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008]
_ROT = [[0, 36, 3, 41, 18], [1, 44, 10, 45, 2], [62, 6, 43, 15, 61], [28, 55, 25, 21, 56], [27, 20, 39, 8, 14]]
_M = (1 << 64) - 1


def keccak(data: bytes) -> bytes:
    """Keccak-256 (ADR-0003), not SHA3-256."""
    rate = 136
    pad = rate - len(data) % rate
    msg = bytearray(data) + (b"\x81" if pad == 1 else b"\x01" + b"\x00" * (pad - 2) + b"\x80")
    a = [[0] * 5 for _ in range(5)]
    for off in range(0, len(msg), rate):
        for i in range(rate // 8):
            a[i % 5][i // 5] ^= int.from_bytes(msg[off + 8 * i:off + 8 * i + 8], "little")
        for rc in _RC:
            c = [a[x][0] ^ a[x][1] ^ a[x][2] ^ a[x][3] ^ a[x][4] for x in range(5)]
            d = [c[(x - 1) % 5] ^ (((c[(x + 1) % 5] << 1) | (c[(x + 1) % 5] >> 63)) & _M) for x in range(5)]
            a = [[a[x][y] ^ d[x] for y in range(5)] for x in range(5)]
            b = [[0] * 5 for _ in range(5)]
            for x in range(5):
                for y in range(5):
                    r = _ROT[x][y]
                    b[y][(2 * x + 3 * y) % 5] = ((a[x][y] << r) | (a[x][y] >> (64 - r))) & _M if r else a[x][y]
            a = [[b[x][y] ^ (~b[(x + 1) % 5][y] & _M & b[(x + 2) % 5][y]) for y in range(5)] for x in range(5)]
            a[0][0] ^= rc
    return b"".join(a[i % 5][i // 5].to_bytes(8, "little") for i in range(4))


def hx(b: bytes) -> str:
    return "0x" + b.hex()


_P = 2**256 - 2**32 - 977
_N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
_G = (0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
      0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8)


def _add(p, q):
    if p is None:
        return q
    if q is None:
        return p
    if p[0] == q[0] and (p[1] + q[1]) % _P == 0:
        return None
    if p == q:
        m = 3 * p[0] * p[0] * pow(2 * p[1], -1, _P) % _P
    else:
        m = (q[1] - p[1]) * pow(q[0] - p[0], -1, _P) % _P
    x = (m * m - p[0] - q[0]) % _P
    return x, (m * (p[0] - x) - p[1]) % _P


def _mul(k, p):
    out = None
    while k:
        if k & 1:
            out = _add(out, p)
        p = _add(p, p)
        k >>= 1
    return out


def _address(point) -> str:
    return hx(keccak(point[0].to_bytes(32, "big") + point[1].to_bytes(32, "big"))[12:])


def eip191(text: str) -> int:
    m = text.encode()
    return int.from_bytes(keccak(b"\x19Ethereum Signed Message:\n" + str(len(m)).encode() + m), "big")


def recover(text: str, sig: bytes):
    """The address that signed `text` (EIP-191; canonical form only: v 27 or 28, low s), or None."""
    if len(sig) != 65 or sig[64] not in (27, 28):
        return None
    r, s = int.from_bytes(sig[:32], "big"), int.from_bytes(sig[32:64], "big")
    if not (0 < r < _N and 0 < s <= _N // 2):
        return None
    y2 = (pow(r, 3, _P) + 7) % _P
    y = pow(y2, (_P + 1) // 4, _P)
    if y * y % _P != y2:
        return None
    if y % 2 != sig[64] - 27:
        y = _P - y
    z = eip191(text)
    ri = pow(r, -1, _N)
    q = _add(_mul(s * ri % _N, (r, y)), _mul(-z * ri % _N, _G))
    return _address(q) if q else None


def sign(text: str, secret: int) -> bytes:
    """A test signature with a fixed nonce (the self-test only: never for a key that matters)."""
    k = int.from_bytes(keccak(b"self-test nonce" + text.encode()), "big") % _N
    point = _mul(k, _G)
    r = point[0] % _N
    s = pow(k, -1, _N) * (eip191(text) + r * secret) % _N
    v = 27 + point[1] % 2
    if s > _N // 2:
        s, v = _N - s, 55 - v
    return r.to_bytes(32, "big") + s.to_bytes(32, "big") + bytes([v])


# ---------------------------------------------------------------- the application

APP_KEYS = ["version", "chain_id", "book", "publisher", "origin", "signer", "signers_document_keccak", "data",
            "sample", "asks", "created_unix", "expires_unix", "signature", "cosignature"]


def strict_load(raw: bytes):
    """JSON in which no field stands twice."""
    def pairs(items):
        keys = [k for k, _ in items]
        if len(keys) != len(set(keys)):
            raise ValueError("a field stands twice")
        return dict(items)
    return json.loads(raw.decode("utf-8"), object_pairs_hook=pairs)


def application_message(a: dict) -> str:
    """`onyx-publisher-application-v1:0x<digest>` (onyx_bulk::application::Fields::message)."""
    def s(t):
        b = t.encode()
        return len(b).to_bytes(4, "little") + b

    def raw(h, n):
        b = bytes.fromhex(h[2:])
        if len(b) != n:
            raise ValueError("a hex field of the wrong length")
        return b
    sample, asks = a["sample"], a["asks"]
    body = (bytes([a["version"]]) + a["chain_id"].to_bytes(8, "little") + raw(a["book"], 20) + s(a["publisher"])
            + s(a["origin"]) + raw(a["signer"], 20) + raw(a["signers_document_keccak"], 32) + s(a["data"])
            + s(sample["dataset"]) + raw(sample["manifest_root"], 32) + sample["bytes"].to_bytes(8, "little")
            + s(sample["path"]) + asks["quota_gib"].to_bytes(4, "little") + asks["datasets"].to_bytes(4, "little")
            + a["created_unix"].to_bytes(8, "little") + a["expires_unix"].to_bytes(8, "little"))
    return "onyx-publisher-application-v1:" + hx(keccak(b"onyx-publisher-application-v1" + body))


def read_application(raw: bytes) -> dict:
    """The application's fields, after its signature recovered its signer. Raises ValueError."""
    if len(raw) > 4096:
        raise ValueError("over 4 KiB")
    a = strict_load(raw)
    if not isinstance(a, dict) or sorted(a) != sorted(APP_KEYS):
        raise ValueError("not exactly the fields of an application")
    try:
        text = application_message(a)
        signer = recover(text, bytes.fromhex(a["signature"][2:]))
    except (KeyError, TypeError, AttributeError, OverflowError) as e:
        raise ValueError(f"malformed: {e}") from None
    if signer != a["signer"]:
        raise ValueError("the signature is not the signer's")
    if a["cosignature"] is not None:
        a["cosigner"] = recover(text, bytes.fromhex(a["cosignature"][2:]))
        if a["cosigner"] is None:
            raise ValueError("the cosignature recovers no key")
    return a


# ---------------------------------------------------------------- the record's rules

# state -> (required fields, optional fields); every other field is refused.
PENDING = {
    "received": ({"state", "date", "application", "received_unix"}, set()),
    "refused": ({"state", "date", "application", "reason"}, {"signer", "blocks_signer_until"}),
    "withdrawn": ({"state", "date", "application", "request", "request_keccak", "signer", "sequence"}, set()),
    "expired": ({"state", "date", "application", "signer", "sample_manifest_root"}, set()),
}
APPROVED = {"state", "date", "application", "application_file", "publisher", "origin", "signer",
            "signers_document_keccak", "standing", "cosigner", "chain_id", "book", "answers", "limits", "register",
            "relay"}
BEFORE = {"state", "date", "publisher", "origin", "signer", "signers_document_keccak", "chain_id", "book", "register"}
REQUEST = {"state", "date", "request", "request_keccak", "signer", "sequence"}
REQUEST_MORE = {"raise": {"limits"}, "retire-key": {"ended_signer", "at_unix", "tx"}, "key-stolen": {"ended_signer", "since_unix", "tx"}}
ACTION = ({"state", "date", "action"}, {"reason"})


def is_tx(v) -> bool:
    return isinstance(v, dict) and set(v) == {"tx", "block"} and isinstance(v["tx"], str) \
        and bool(H32_RE.match(v["tx"])) and isinstance(v["block"], int)


def is_limits(v) -> bool:
    return isinstance(v, dict) and set(v) == {"quota_gib", "datasets"} \
        and all(isinstance(x, int) and not isinstance(x, bool) and x >= 1 for x in v.values())


def fields(d: dict, required: set, optional: set, out: list, where: str) -> bool:
    missing, extra = required - set(d), set(d) - required - optional
    if missing:
        out.append(f"{where}: no {', '.join(sorted(missing))}")
    if extra:
        out.append(f"{where}: holds {', '.join(sorted(extra))}, which this state must not keep")
    return not missing and not extra


def common(d: dict, out: list, where: str):
    if not isinstance(d.get("date"), str) or not DATE_RE.match(d["date"]):
        out.append(f"{where}: date is not YYYY-MM-DD")
    for k, rx in (("application", H32_RE), ("signer", ADDR_RE), ("request_keccak", H32_RE), ("ended_signer", ADDR_RE),
                  ("signers_document_keccak", H32_RE), ("sample_manifest_root", H32_RE), ("book", ADDR_RE)):
        if k in d and not (isinstance(d[k], str) and rx.match(d[k])):
            out.append(f"{where}: {k} is not lowercase hex of its length")
    for k in ("sequence", "blocks_signer_until", "received_unix", "at_unix", "since_unix", "chain_id"):
        if k in d and (not isinstance(d[k], int) or isinstance(d[k], bool) or d[k] < 0):
            out.append(f"{where}: {k} is not a whole number")
    if "reason" in d and d["reason"] not in REASONS + TOOL_REASONS:
        out.append(f"{where}: reason {d['reason']!r} is not a reason code")


def check_pending(folder: str, entries: list, out: list):
    for i, d in enumerate(entries):
        where = f"{folder}/decision.json entry {i + 1}"
        state = d.get("state")
        if state not in PENDING:
            out.append(f"{where}: state {state!r} is not one of an application that is not approved")
            continue
        fields(d, *PENDING[state], out, where)
        common(d, out, where)
        if isinstance(d.get("application"), str) and d["application"][2:18] != folder[len("application-"):]:
            out.append(f"{where}: the folder is not named by this application's Keccak")
        if state == "refused" and ("signer" in d) != ("blocks_signer_until" in d):
            out.append(f"{where}: a refusal blocks a signer with both signer and blocks_signer_until, or with neither")
        if state == "withdrawn" and d.get("request") != "withdraw":
            out.append(f"{where}: a withdrawal is recorded from a withdraw request")
    states = [d.get("state") for d in entries]
    if any(s != "received" for s in states[:-1]):
        out.append(f"{folder}/decision.json: a refused, withdrawn or expired application is closed: no entry follows it")


def check_publisher(root: str, folder: str, entries: list, out: list):
    first = entries[0].get("state")
    if first not in ("approved", "registered-before-record"):
        out.append(f"{folder}/decision.json: a publisher's record starts with its approval or with registered-before-record")
    for i, d in enumerate(entries):
        where = f"{folder}/decision.json entry {i + 1}"
        state = d.get("state")
        common(d, out, where)
        if "request" in d:
            more = REQUEST_MORE.get(d["request"])
            if more is None:
                out.append(f"{where}: request {d['request']!r} is not raise, retire-key or key-stolen")
            elif fields(d, REQUEST | (more - {"tx"}), more & {"tx"}, out, where):
                if state not in ("approved", "paused", "retired"):
                    out.append(f"{where}: state {state!r} after a request")
                if "limits" in d and not is_limits(d["limits"]):
                    out.append(f"{where}: limits are whole GiB and datasets")
                if d.get("tx") is not None and not is_tx(d["tx"]):
                    out.append(f"{where}: tx is a transaction hash and a block, or null")
        elif "action" in d:
            if fields(d, *ACTION, out, where) and (d["action"], state) not in (("pause", "paused"), ("resume", "approved")):
                out.append(f"{where}: action {d['action']!r} with state {state!r}")
        elif state == "registered-before-record":
            if fields(d, BEFORE, set(), out, where):
                if d["publisher"] != folder:
                    out.append(f"{where}: publisher is not the folder's id")
                if not is_tx(d["register"]):
                    out.append(f"{where}: register is a transaction hash and a block")
        elif state == "approved":
            if not fields(d, APPROVED, set(), out, where):
                continue
            if d["publisher"] != folder:
                out.append(f"{where}: publisher is not the folder's id")
            if not is_limits(d["limits"]):
                out.append(f"{where}: limits are whole GiB and datasets")
            if not is_tx(d["register"]):
                out.append(f"{where}: register is a transaction hash and a block")
            relay = d["relay"]
            if not (is_tx(relay) or relay in ({"already_recorded": True}, {"not_relayed": "stale"})):
                out.append(f"{where}: relay is a transaction, already_recorded or not_relayed")
            answers = d["answers"]
            if not isinstance(answers, dict) or sorted(answers) != sorted(ANSWER_LINES) \
                    or any(v != "yes" for v in answers.values()):
                out.append(f"{where}: answers are yes for the lines 7, 8, 9, 10, 11 and 13 and nothing else: "
                           "no free text, and not line 12 (who runs a publisher is not public)")
            name = d["application_file"]
            if not isinstance(name, str) or not re.match(r"application(-\d+)?\.json\Z", name):
                out.append(f"{where}: application_file is not an application file's name")
                continue
            try:
                with open(os.path.join(root, folder, name), "rb") as fh:
                    raw = fh.read()
            except OSError:
                out.append(f"{where}: {folder}/{name} is missing")
                continue
            if hx(keccak(raw)) != d["application"]:
                out.append(f"{where}: {folder}/{name} does not hash to the recorded application (it was changed)")
                continue
            try:
                a = read_application(raw)
            except ValueError as e:
                out.append(f"{where}: {folder}/{name}: {e}")
                continue
            for k in ("publisher", "origin", "signer", "signers_document_keccak", "chain_id", "book"):
                if a[k] != d[k]:
                    out.append(f"{where}: {k} is not the application's")
            if is_limits(d["limits"]) and (d["limits"]["quota_gib"] > a["asks"]["quota_gib"]
                                          or d["limits"]["datasets"] > a["asks"]["datasets"]):
                out.append(f"{where}: the limits are over what the application asks")
            if d["standing"] == "second-key" and (a.get("cosigner") is None or a["cosigner"] != d["cosigner"]):
                out.append(f"{where}: a second key's application is cosigned by the recorded cosigner")
            if d["standing"] not in ("first", "reapply", "second-key"):
                out.append(f"{where}: standing {d['standing']!r}")
        else:
            out.append(f"{where}: state {state!r} is not a publisher's state")
    origins = {d["origin"] for d in entries if isinstance(d.get("origin"), str)}
    if len(origins) > 1:
        out.append(f"{folder}/decision.json: one id has one origin, the record names {len(origins)}")


def load(root: str, out: list) -> dict:
    """Every folder's entries, after the checks of each. A broken file is a problem, not a crash."""
    record = {}
    if not os.path.isdir(root):
        out.append(f"{root}: no record folder")
        return record
    for folder in sorted(os.listdir(root)):
        path = os.path.join(root, folder)
        if not os.path.isdir(path):
            if folder != "README.md":
                out.append(f"{folder}: the record holds folders only")
            continue
        pending = bool(HASH_FOLDER_RE.match(folder))
        if not pending and not (ID_RE.match(folder) and len(folder) <= 128):
            out.append(f"{folder}: neither a publisher id nor application-<16 hex>")
            continue
        try:
            with open(os.path.join(path, "decision.json"), "rb") as fh:
                raw = fh.read()
            entries = strict_load(raw)
        except (OSError, ValueError) as e:
            out.append(f"{folder}/decision.json: {e}")
            continue
        if isinstance(entries, dict):
            entries = [entries]
        if not isinstance(entries, list) or not entries or not all(isinstance(d, dict) for d in entries):
            out.append(f"{folder}/decision.json: not a list of decisions")
            continue
        for name in sorted(os.listdir(path)):
            with open(os.path.join(path, name), "rb") as fh:
                text = fh.read().decode("utf-8", "replace")
            if CONTACT_RE.search(text):
                out.append(f"{folder}/{name}: e-mail-shaped text or a mailto link: the record has no contact data")
            named = {d.get("application_file") for d in entries}
            if name != "decision.json" and (pending or name not in named):
                out.append(f"{folder}/{name}: a file no decision names (of an application that is not approved only the decision is kept)")
        if pending:
            check_pending(folder, entries, out)
        else:
            check_publisher(root, folder, entries, out)
        record[folder] = entries
    by_origin, by_signer = {}, {}
    for folder, entries in record.items():
        if HASH_FOLDER_RE.match(folder):
            continue
        for d in entries:
            if isinstance(d.get("origin"), str):
                by_origin.setdefault(d["origin"], set()).add(folder)
            if d.get("state") in ("approved", "registered-before-record") and "request" not in d and "action" not in d \
                    and isinstance(d.get("signer"), str):
                by_signer.setdefault(d["signer"], set()).add(folder)
    for what, table in (("origin", by_origin), ("signer", by_signer)):
        for k, ids in sorted(table.items()):
            if len(ids) > 1:
                out.append(f"the {what} {k} stands under {len(ids)} ids ({', '.join(sorted(ids))}): one {what}, one id")
    return record


def warnings(record: dict, now: int) -> list:
    out = []
    for folder, entries in sorted(record.items()):
        last = entries[-1]
        if last.get("state") == "received" and isinstance(last.get("received_unix"), int):
            days = (now - last["received_unix"]) // 86400
            if days >= DECIDE_DAYS:
                out.append(f"{folder}: received {days} days ago and not decided (the reviewer decides within {DECIDE_DAYS} days)")
    return out


def limits_of(entries: list):
    """The limits the record grants now: the newest entry that names them."""
    for d in reversed(entries):
        if is_limits(d.get("limits")):
            return d["limits"]
    return None


def status(record: dict, confirmed: bool) -> dict:
    """The status list: publishers by id, and applications that are not approved by their Keccak.
    Without `confirmed` (no run against the book) an approval is listed as "approved (record)"."""
    publishers, applications, net = [], [], {}
    for folder, entries in sorted(record.items()):
        last = entries[-1]
        if HASH_FOLDER_RE.match(folder):
            row = {"application": last.get("application"), "state": last.get("state"), "date": last.get("date")}
            for k in ("reason", "blocks_signer_until"):
                if k in last:
                    row[k] = last[k]
            applications.append(row)
            continue
        keys = [d for d in entries if d.get("state") in ("approved", "registered-before-record")
                and "request" not in d and "action" not in d]
        gone = {d["ended_signer"] for d in entries if d.get("request") in ("retire-key", "key-stolen") and "ended_signer" in d}
        state = last.get("state")
        if state == "approved" and not confirmed:
            state = UNCONFIRMED
        row = {"publisher": folder, "state": state, "date": last.get("date"),
               "origin": keys[0].get("origin") if keys else None,
               "signers": [d["signer"] for d in keys if d.get("signer") not in gone],
               "registered": [d["register"] for d in keys if is_tx(d.get("register"))]}
        if state == "registered-before-record":
            # Review S4-R1 M2: the website shows this list. A publisher from before the record has
            # no application whose origin a reader could check here, so the list carries none for
            # it; the record's own file and the book keep it.
            del row["origin"]
        if "reason" in last:
            row["reason"] = last["reason"]
        if keys:
            net = {"chain_id": keys[0].get("chain_id"), "book": keys[0].get("book")}
        publishers.append(row)
    return {"version": 1, **net, "publishers": publishers, "applications": applications}


def check_limits(record: dict, path: str, max_gib, out: list):
    try:
        with open(path, "rb") as fh:
            doc = strict_load(fh.read())
        entries = {e["id"]: e for e in doc["publishers"]}
        if len(entries) != len(doc["publishers"]):
            raise ValueError("an id has two entries")
        reserve = doc["reserve_gib"]
    except (OSError, ValueError, KeyError, TypeError) as e:
        out.append(f"--limits {path}: not a limits file ({e})")
        return
    for pid, e in sorted(entries.items()):
        rec = record.get(pid)
        if rec is None or HASH_FOLDER_RE.match(pid):
            out.append(f"--limits: {pid} has an entry and no record: the node takes data from a publisher nobody recorded")
            continue
        state = rec[-1].get("state")
        if state == "retired":
            if e.get("state") != "paused":
                out.append(f"--limits: {pid} is retired in the record and its entry is not paused")
        elif (state == "paused") != (e.get("state") == "paused"):
            out.append(f"--limits: {pid} is {e.get('state')} in the file and {state} in the record")
        want = limits_of(rec)
        if want and (e.get("quota_gib"), e.get("datasets")) != (want["quota_gib"], want["datasets"]):
            out.append(f"--limits: {pid} has {e.get('quota_gib')} GiB and {e.get('datasets')} datasets, the record grants {want['quota_gib']} and {want['datasets']}")
    for pid, rec in sorted(record.items()):
        if not HASH_FOLDER_RE.match(pid) and limits_of(rec) and pid not in entries:
            out.append(f"--limits: {pid} is approved with limits and has no entry: its key would meet a node without its limit")
    if max_gib is not None:
        total = sum(e.get("quota_gib", 0) for e in entries.values()) + reserve
        if total > max_gib:
            out.append(f"--limits: the grants plus the reserve are {total} GiB, over the node's {max_gib} GiB")


# ---------------------------------------------------------------- the book (read-only)

TOPIC = hx(keccak(b"KeyRegistered(bytes32,address,string,string,bytes32,address,string)"))


def http_rpc(url: str):
    def call(method, params):
        req = urllib.request.Request(url, data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(),
                                     headers={"content-type": "application/json", "user-agent": "onyx-check-publishers/1"})
        with urllib.request.urlopen(req, timeout=20) as r:
            answer = json.load(r)
        if "error" in answer:
            raise OSError(str(answer["error"])[:200])
        return answer["result"]
    return call


def abi_string(data: bytes, i: int) -> str:
    at = int.from_bytes(data[32 * i:32 * i + 32], "big")
    n = int.from_bytes(data[at:at + 32], "big")
    return data[at + 32:at + 32 + n].decode("utf-8")


def check_book(record: dict, call, out: list):
    """Each recorded registration is the book's KeyRegistered log in its block."""
    for folder, entries in sorted(record.items()):
        for i, d in enumerate(entries):
            if d.get("state") not in ("approved", "registered-before-record") or not is_tx(d.get("register")) \
                    or "request" in d or "action" in d:
                continue
            where = f"{folder}/decision.json entry {i + 1}"
            source = d["publisher"].encode()
            key = hx(keccak(b"onyx-source-key-v1" + len(source).to_bytes(4, "little") + source + bytes.fromhex(d["signer"][2:])))
            block = hex(d["register"]["block"])
            try:
                logs = call("eth_getLogs", [{"address": d["book"], "fromBlock": block, "toBlock": block, "topics": [TOPIC, key]}])
            except (OSError, ValueError) as e:
                out.append(f"{where}: the book was not read ({e})")
                continue
            hit = [x for x in logs if x.get("transactionHash") == d["register"]["tx"]]
            if not hit:
                out.append(f"{where}: the book shows no registration of this key in the recorded transaction")
                continue
            data = bytes.fromhex(hit[0]["data"][2:])
            if (abi_string(data, 0), abi_string(data, 1), hx(data[64:96])) != (d["publisher"], d["origin"], d["signers_document_keccak"]):
                out.append(f"{where}: the book's registration names another id, origin or keys document")


# ---------------------------------------------------------------- the secret scanner

def leaks(paths: list, out: list) -> bool:
    """Run the repository's secret scanner (gitleaks, .gitleaks.toml) over `paths`: the public
    snapshot runs it over the whole tree and refuses a finding, and its allowlist takes only exact
    path-and-value entries, so a field name the scanner reads as a credential (a name with "key" in
    it and a hex value) would need an entry per record. A finding is a problem here already.
    False when gitleaks is not installed (nothing was scanned)."""
    tool = shutil.which("gitleaks")
    if tool is None:
        return False
    for path in paths:
        if not os.path.exists(path):
            continue
        with tempfile.TemporaryDirectory() as t:
            report = os.path.join(t, "report.json")
            done = subprocess.run([tool, "dir", path, "--config", os.path.join(ROOT, ".gitleaks.toml"), "--redact",
                                   "--no-banner", "--exit-code", "1", "--report-format", "json", "--report-path", report],
                                  capture_output=True, text=True, timeout=300, check=False)
            if done.returncode == 0:
                continue
            try:
                with open(report, encoding="utf-8") as fh:
                    found = json.load(fh)
            except (OSError, ValueError):
                found = []
            if not found:
                out.append(f"gitleaks failed on {path} (exit {done.returncode}): {done.stderr.strip()[-200:]}")
            for f in found:
                out.append(f"{f.get('File')}:{f.get('StartLine')}: the secret scanner reads this line as a credential "
                           f"(rule {f.get('RuleID')}): a field name must not read as one (no key, secret, token, "
                           "password, auth or api in it), or the public snapshot refuses the tree")
    return True


# ---------------------------------------------------------------- run

def run(root: str, status_path: str, write: bool, limits, max_gib, call, now: int, scan: bool = False):
    """(problems, warnings). With `scan`, the secret scanner reads the record and the status list."""
    out = []
    record = load(root, out)
    if scan and not leaks([root], out):
        print("NOTE: gitleaks is not installed: the record was not read by the secret scanner")
    if limits:
        check_limits(record, limits, max_gib, out)
    if call:
        check_book(record, call, out)
    def text_of(confirmed):
        return json.dumps(status(record, confirmed), indent=1, sort_keys=True) + "\n"
    # The book confirmed every recorded registration in this run, or it was not asked.
    confirmed = call is not None and not out
    text = text_of(confirmed)
    if write and not out:
        with open(status_path, "w", encoding="utf-8") as fh:
            fh.write(text)
    else:
        try:
            with open(status_path, encoding="utf-8") as fh:
                have = fh.read()
        except OSError:
            have = None
        # A list a run with --rpc wrote stays valid for a run without the book.
        if have not in (text, text_of(True)):
            out.append(f"{status_path} is not what the record gives: run scripts/check-publishers.py --write")
    if scan:
        leaks([status_path], out)
    return out, warnings(record, now)


def self_test() -> int:
    bad = []

    def check(name, ok):
        print(f"  {'ok' if ok else 'FAIL'}: {name}")
        if not ok:
            bad.append(name)

    assert hx(keccak(b"")) == "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
    secret = 0xA11CE5
    signer = _address(_mul(secret, _G))
    cosign_secret = 0xB0B5
    book = "0x" + "b0" * 20
    app = {"version": 1, "chain_id": 31337, "book": book, "publisher": "river-levels.open",
           "origin": "data.example.org/river-levels", "signer": signer, "signers_document_keccak": "0x" + "11" * 32,
           "data": "Daily water levels of 40 public gauges.",
           "sample": {"dataset": "river-levels.daily", "manifest_root": "0x" + "22" * 32, "bytes": 106, "path": "sample/"},
           "asks": {"quota_gib": 1, "datasets": 16}, "created_unix": 1794873600, "expires_unix": 1795392000}
    text = application_message(app)
    app["signature"] = hx(sign(text, secret))
    app["cosignature"] = None
    raw = (json.dumps(app) + "\n").encode()
    check("a signed application recovers its signer", read_application(raw)["signer"] == signer)
    tx = {"tx": "0x" + "7a" * 32, "block": 42}
    approved = {"state": "approved", "date": "2026-11-17", "application": hx(keccak(raw)), "application_file": "application.json",
                "publisher": app["publisher"], "origin": app["origin"], "signer": signer,
                "signers_document_keccak": app["signers_document_keccak"], "standing": "first", "cosigner": None,
                "chain_id": 31337, "book": book, "answers": {n: "yes" for n in ANSWER_LINES},
                "limits": {"quota_gib": 1, "datasets": 16}, "register": tx, "relay": {"tx": "0x" + "7b" * 32, "block": 43}}
    refused = {"state": "refused", "date": "2026-11-17", "application": "0x" + "ab" * 32, "reason": "rights-unclear",
               "signer": "0x" + "cd" * 20, "blocks_signer_until": 1795478700}
    before = {"state": "registered-before-record", "date": "2026-10-30", "publisher": "tide-tables.open",
              "origin": "tides.example.net", "signer": "0x" + "ee" * 20, "signers_document_keccak": "0x" + "33" * 32,
              "chain_id": 31337, "book": book, "register": {"tx": "0x" + "7c" * 32, "block": 7}}
    now = 1794873600 + 86400

    def case(change=None, limits=None, max_gib=None, call=None, at=now):
        with tempfile.TemporaryDirectory() as t:
            root, st = os.path.join(t, "publishers"), os.path.join(t, "publishers.json")
            fresh = lambda d: json.loads(json.dumps(d))  # noqa: E731
            files = {"river-levels.open/decision.json": [fresh(approved)], "river-levels.open/application.json": raw,
                     "application-" + "ab" * 8 + "/decision.json": [fresh(refused)],
                     "tide-tables.open/decision.json": [fresh(before)]}
            if change:
                change(files)
            for rel, v in files.items():
                os.makedirs(os.path.dirname(os.path.join(root, rel)), exist_ok=True)
                with open(os.path.join(root, rel), "wb") as fh:
                    fh.write(v if isinstance(v, bytes) else json.dumps(v, indent=1).encode())
            lim = None
            if limits is not None:
                lim = os.path.join(t, "limits.json")
                with open(lim, "w", encoding="utf-8") as fh:
                    json.dump(limits, fh)
            first = run(root, st, True, lim, max_gib, call, at)
            again = run(root, st, False, lim, max_gib, call, at) if not first[0] else first
            return first[0] + [p for p in again[0] if p not in first[0]], first[1]

    def has(result, text):
        return any(text in p for p in result[0])

    check("a valid record passes, and its status list is the written one", case() == ([], []))

    def changed(files):
        files["river-levels.open/application.json"] = raw.replace(b"40 public", b"41 public")
    check("a changed application is refused", has(case(changed), "does not hash to the recorded application"))

    def resigned(files):
        other = dict(app, data="Other text.")
        b = (json.dumps(other) + "\n").encode()
        files["river-levels.open/application.json"] = b
        files["river-levels.open/decision.json"][0]["application"] = hx(keccak(b))
    check("an application whose signature is not the signer's is refused", has(case(resigned), "the signature is not the signer's"))

    def keeps_data(files):
        files["application-" + "ab" * 8 + "/decision.json"][0]["data"] = "Daily water levels."
    check("a refused record that keeps the data text is refused", has(case(keeps_data), "holds data, which this state must not keep"))

    def keeps_id(files):
        files["application-" + "ab" * 8 + "/decision.json"][0]["publisher"] = "river-levels.open"
    check("a refused record that keeps the id is refused", has(case(keeps_id), "holds publisher"))

    def keeps_file(files):
        files["application-" + "ab" * 8 + "/application.json"] = raw
    check("a refused record that keeps the application file is refused", has(case(keeps_file), "a file no decision names"))

    def half_block(files):
        del files["application-" + "ab" * 8 + "/decision.json"][0]["blocks_signer_until"]
    check("a refusal that names a signer without its block is refused", has(case(half_block), "or with neither"))

    def contact(files):
        files["river-levels.open/decision.json"][0]["register"]["note"] = "ask someone" + "@" + "example.org"
    check("e-mail-shaped text is refused", has(case(contact), "e-mail-shaped text"))

    def free_text(files):
        files["river-levels.open/decision.json"][0]["answers"]["8"] = "reads as nobody else"
    check("an answer in free text is refused", has(case(free_text), "no free text"))

    def line_twelve(files):
        files["river-levels.open/decision.json"][0]["answers"]["12"] = "yes"
    check("the operator grouping (line 12) in the public record is refused", has(case(line_twelve), "not line 12"))

    def two_ids(files):
        files["tide-tables.open/decision.json"][0]["signer"] = signer
    check("one signer under two ids is refused", has(case(two_ids), "one signer, one id"))

    def over_ask(files):
        files["river-levels.open/decision.json"][0]["limits"] = {"quota_gib": 2, "datasets": 16}
    check("limits over what the application asks are refused", has(case(over_ask), "over what the application asks"))

    def late(files):
        files["application-" + "cd" * 8 + "/decision.json"] = [{"state": "received", "date": "2026-11-17",
                                                               "application": "0x" + "cd" * 32, "received_unix": 1794873600}]
    r = case(late, at=1794873600 + 6 * 86400)
    check("a late decision only warns", r[0] == [] and len(r[1]) == 1 and "not decided" in r[1][0])
    check("a decision in time does not warn", case(late, at=1794873600 + 4 * 86400) == ([], []))

    def log(d, **over):
        def s(t):
            b = t.encode()
            return len(b).to_bytes(32, "big") + b + b"\x00" * (-len(b) % 32)
        a, o = s(over.get("publisher", d["publisher"])), s(over.get("origin", d["origin"]))
        head = (160).to_bytes(32, "big") + (160 + len(a)).to_bytes(32, "big") + bytes.fromhex(d["signers_document_keccak"][2:]) \
            + bytes(32) + (160 + len(a) + len(o)).to_bytes(32, "big")
        return {"transactionHash": d["register"]["tx"], "data": hx(head + a + o + s("r"))}

    def book_with(**over):
        def call(method, params):
            f = params[0]
            for d in (approved, before):
                if int(f["fromBlock"], 16) == d["register"]["block"] and f["address"] == book and f["topics"][0] == TOPIC:
                    if d is approved and over.get("missing"):
                        return []
                    return [log(d, **(over if d is approved else {}))]
            return []
        return call
    check("the record equals the book's registrations", case(call=book_with()) == ([], []))

    def listed_state(call):
        with tempfile.TemporaryDirectory() as t:
            root, st = os.path.join(t, "p"), os.path.join(t, "publishers.json")
            os.makedirs(os.path.join(root, "river-levels.open"))
            with open(os.path.join(root, "river-levels.open", "decision.json"), "w", encoding="utf-8") as fh:
                json.dump([approved], fh)
            with open(os.path.join(root, "river-levels.open", "application.json"), "wb") as fh:
                fh.write(raw)
            first = run(root, st, True, None, None, call, now)
            with open(st, encoding="utf-8") as fh:
                state = json.load(fh)["publishers"][0]["state"]
            return state, first[0], run(root, st, False, None, None, None, now)[0]
    check("an approval the book did not confirm is listed as approved (record)", listed_state(None) == (UNCONFIRMED, [], []))
    check("an approval the book confirmed is listed as approved, and stays valid offline",
          listed_state(book_with()) == ("approved", [], []))
    check("an approval the book does not show is refused", has(case(call=book_with(missing=True)), "the book shows no registration"))
    check("an approval for another origin than the book's is refused",
          has(case(call=book_with(origin="other.example.org")), "names another id, origin or keys document"))

    good = {"reserve_gib": 2, "publishers": [{"id": "river-levels.open", "quota_gib": 1, "datasets": 16, "state": "open"},
                                             {"id": "tide-tables.open", "quota_gib": 16, "datasets": 50000, "state": "open"}]}
    check("a limits file that matches the record passes", case(limits=good, max_gib=24) == ([], []))
    check("grants over the node's total are refused", has(case(limits=good, max_gib=18), "over the node's 18 GiB"))
    more = json.loads(json.dumps(good))
    more["publishers"][0]["quota_gib"] = 8
    check("an entry with other numbers than the record is refused", has(case(limits=more), "the record grants 1 and 16"))
    stray = json.loads(json.dumps(good))
    stray["publishers"].append({"id": "unknown.open", "quota_gib": 1, "datasets": 16, "state": "open"})
    check("an entry without a record is refused", has(case(limits=stray), "has an entry and no record"))
    none = {"reserve_gib": 2, "publishers": good["publishers"][1:]}
    check("an approved publisher without an entry is refused", has(case(limits=none), "has no entry"))
    paused = json.loads(json.dumps(good))
    paused["publishers"][0]["state"] = "paused"
    check("a pause the record does not show is refused", has(case(limits=paused), "is paused in the file and approved in the record"))

    def pause(files):
        files["river-levels.open/decision.json"].append({"state": "paused", "date": "2026-11-18", "action": "pause"})
    check("a recorded pause matches a paused entry", case(pause, limits=paused) == ([], []))

    with tempfile.TemporaryDirectory() as t:
        os.makedirs(os.path.join(t, "p", "tide-tables.open"))
        with open(os.path.join(t, "p", "tide-tables.open", "decision.json"), "w", encoding="utf-8") as fh:
            json.dump([before], fh)
        st = os.path.join(t, "publishers.json")
        check("a missing status list is refused without --write", any("--write" in p for p in run(os.path.join(t, "p"), st, False, None, None, None, now)[0]))
        run(os.path.join(t, "p"), st, True, None, None, None, now)
        with open(st, encoding="utf-8") as fh:
            listed = json.load(fh)
        check("the status list names the publisher, its state and key, and no origin before the record",
              listed["publishers"] == [{"publisher": "tide-tables.open", "state": "registered-before-record", "date": "2026-10-30",
                                        "signers": [before["signer"]],
                                        "registered": [before["register"]]}] and listed["applications"] == [])
    # The secret scanner: the record as written passes; the field name the scanner once took for a
    # credential is refused (its name is built here so this file holds no such line).
    with tempfile.TemporaryDirectory() as t:
        folder = os.path.join(t, "p", "tide-tables.open")
        os.makedirs(folder)

        def planted(name):
            entry = dict(before)
            entry[name] = entry.pop("signers_document_keccak")
            entry[name] = hx(keccak(b"a keys document"))
            with open(os.path.join(folder, "decision.json"), "w", encoding="utf-8") as fh:
                json.dump([entry], fh, indent=2)
            found = []
            return leaks([os.path.join(t, "p")], found), found
        scanned, found = planted("signers_document_keccak")
        if not scanned:
            print("  skipped: gitleaks is not installed (the secret scanner's two cases)")
        else:
            check("the secret scanner passes the record's field names", found == [])
            _, found = planted("keys_" + "document_keccak")
            check("a field name the secret scanner reads as a credential is refused",
                  len(found) == 1 and "generic-api-key" in found[0] and "decision.json:" in found[0])
    if bad:
        print(f"check-publishers self-test FAILED: {', '.join(bad)}")
        return 1
    print("check-publishers self-test passed")
    return 0


def main(argv: list) -> int:
    opts, args = {}, list(argv)
    while args:
        a = args.pop(0)
        if a in ("--write", "--self-test"):
            opts[a] = True
        elif a in ("--limits", "--max-gib", "--rpc", "--records", "--status") and args:
            opts[a] = args.pop(0)
        else:
            print(__doc__.split("\n\n")[1], file=sys.stderr)
            return 2
    if "--self-test" in opts:
        return self_test()
    try:
        max_gib = int(opts["--max-gib"]) if "--max-gib" in opts else None
    except ValueError:
        print("--max-gib: a whole number", file=sys.stderr)
        return 2
    call = http_rpc(opts["--rpc"]) if "--rpc" in opts else None
    problems, warned = run(opts.get("--records", RECORDS), opts.get("--status", STATUS), "--write" in opts,
                           opts.get("--limits"), max_gib, call, int(time.time()), scan=True)
    for w in warned:
        print(f"WARNING: {w}")
    for p in problems:
        print(f"check-publishers: {p}")
    if problems:
        print(f"check-publishers FAILED: {len(problems)} problem(s)")
        return 1
    print(f"check-publishers passed ({len(warned)} warning(s))")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
