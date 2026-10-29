#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Issuer rosters for label set v3 (ROADMAP P2a, ADR-0037 Amendment 1).

Turns the issuers' own dated proof-of-reserve snapshots into one normalized listings file that
`cargo run -p onyx-source-btc --example build-label-set-v3` turns into the dated label set
`fixtures/labels/test.onyx.entities.btc.v3.json`. Standard library only.

Two stages, each byte-for-byte reproducible:

  import <key> <dir> [<dir2>]   read an indexer run (the raw snapshots the indexer fetched from the
                                issuer and its height mapping) and write fixtures/rosters/<key>/:
                                the raw snapshot files where they fit in git (re-gzipped with
                                mtime 0), otherwise a deterministic extract, plus snapshots.json
                                (url, issuer date, fetch date, height and how it was found,
                                sha256 and keccak-256 of every raw snapshot). Run by a maintainer;
                                not part of check-all. `--out <dir>` writes elsewhere (to re-check
                                an import from the raw files: `import ... --out tmp` then diff).
  deny <filings.csv>            fixtures/rosters/deny.txt: roster addresses that a government
                                filing names (addresses only; the filing's names are never read
                                past the address column). Such an address is dropped (LBL-V2R-F1).
                                deny.basis.json records the roster addresses it was computed
                                against; `listings` refuses to run once they change, so every new
                                snapshot is checked against the filings (review P2a-R2-1).
  listings <out.json> [--rosters <dir>] [--check <committed.json>]
                                parse the committed files only and write the normalized listings
                                (one entry per snapshot: record fields and the listed addresses,
                                each with its published proof, if any). Grades are not decided
                                here: the Rust builder verifies every signature itself.
  fetch <key> <cache>           re-download a source's raw snapshots from the issuer (User-Agent
                                "onyx-dev"; never any personal identifier) and check each sha256.
  self-test                     address, keccak and parser checks with planted failures.

Sources never imported (EXCLUDED; the import refuses them): wBTC (wbtc.network's terms forbid
redistribution and automated access, IDX-R1-F1) and Coinbase cbBTC (terms unread; rows via a
Wayback capture and a person's relay, IDX-R1-F2, D4). Nothing third-party or inferred is read:
only files the issuer itself served.
"""
import csv
import gzip
import hashlib
import io
import json
import re
import subprocess
import unicodedata
import sys
import urllib.request
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ROSTERS = ROOT / "fixtures" / "rosters"
USER_AGENT = "onyx-dev"

# key: (source id, entity, kind, display name). Committed form per source: the raw snapshot
# (Binance, HTX, MEXC: gzip; Bitfinex: as served) or a deterministic extract that keeps no
# personal data (OKX: Bitcoin rows; BitMEX: height and address section; CoinEx: worksheet cells;
# Bybit, Deribit, Crypto.com, BITB: addresses), always with the raw snapshot's hashes.
SOURCES = {
    "binance": ("binance.por", "entity:binance", "Exchange", "Binance"),
    "bitb": ("bitwise.bitb", "entity:bitwise-bitb", "Etf", "Bitwise BITB"),
    "bitfinex": ("bitfinex.wallets", "entity:bitfinex", "Exchange", "Bitfinex"),
    "bitmex": ("bitmex.por", "entity:bitmex", "Exchange", "BitMEX"),
    "bybit": ("bybit.por", "entity:bybit", "Exchange", "Bybit"),
    "coinex": ("coinex.por", "entity:coinex", "Exchange", "CoinEx"),
    "cryptocom": ("cryptocom.wallets", "entity:crypto-com", "Exchange", "Crypto.com"),
    "deribit": ("deribit.por", "entity:deribit", "Exchange", "Deribit"),
    "htx": ("htx.por", "entity:htx", "Exchange", "HTX"),
    "mexc": ("mexc.por", "entity:mexc", "Exchange", "MEXC"),
    "okx": ("okx.por", "entity:okx", "Exchange", "OKX"),
}
EXCLUDED = {
    "wbtc": "wbtc.network's terms forbid reproducing or distributing its materials and automated access (IDX-R1-F1)",
    "cbbtc": "Coinbase's terms for the cbBTC page are unread; rows came via a Wayback capture and a person's relay (IDX-R1-F2, D4)",
}
# Snapshots before this date are not imported for the sources whose early snapshots list tens to
# hundreds of thousands of addresses each (OKX 2023-2025: up to 110,775; BitMEX 2022-2024: up to
# 336,705 deposit addresses; MEXC 2025-11/12: up to 28,848). They are listed in snapshots.json
# with their hashes ("skipped"), for a later full-history set kept outside git.
CUTOFF = {"okx": "2026-01-01", "bitmex": "2026-01-01", "mexc": "2026-01-01"}

# ---------------------------------------------------------------------------------------------
# Hashes and deterministic files
# ---------------------------------------------------------------------------------------------


def sha256(b):
    return hashlib.sha256(b).hexdigest()


_RC = [
    0x0000000000000001, 0x0000000000008082, 0x800000000000808A, 0x8000000080008000,
    0x000000000000808B, 0x0000000080000001, 0x8000000080008081, 0x8000000000008009,
    0x000000000000008A, 0x0000000000000088, 0x0000000080008009, 0x000000008000000A,
    0x000000008000808B, 0x800000000000008B, 0x8000000000008089, 0x8000000000008003,
    0x8000000000008002, 0x8000000000000080, 0x000000000000800A, 0x800000008000000A,
    0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
]
_ROT = [[0, 36, 3, 41, 18], [1, 44, 10, 45, 2], [62, 6, 43, 15, 61], [28, 55, 25, 21, 56],
        [27, 20, 39, 8, 14]]
_M = (1 << 64) - 1


def _keccak_f(a):
    for rc in _RC:
        c = [a[x][0] ^ a[x][1] ^ a[x][2] ^ a[x][3] ^ a[x][4] for x in range(5)]
        d = [c[(x - 1) % 5] ^ (((c[(x + 1) % 5] << 1) | (c[(x + 1) % 5] >> 63)) & _M) for x in range(5)]
        a = [[a[x][y] ^ d[x] for y in range(5)] for x in range(5)]
        b = [[0] * 5 for _ in range(5)]
        for x in range(5):
            for y in range(5):
                r = _ROT[x][y]
                b[y][(2 * x + 3 * y) % 5] = ((a[x][y] << r) | (a[x][y] >> (64 - r))) & _M if r else a[x][y]
        a = [[b[x][y] ^ ((~b[(x + 1) % 5][y]) & b[(x + 2) % 5][y]) for y in range(5)] for x in range(5)]
        a[0][0] ^= rc
    return a


def keccak(b):
    """Keccak-256 (ADR-0003), not SHA3-256. OpenSSL's when present, else pure Python."""
    try:
        h = hashlib.new("keccak-256")
    except ValueError:
        return _keccak_py(b)
    h.update(b)
    return h.hexdigest()


def _keccak_py(data):
    """Pure-Python Keccak-256 (pad10*1 with the 0x01 domain byte), used without OpenSSL's."""
    rate = 136
    pad = rate - (len(data) % rate)
    msg = bytearray(data) + (b"\x81" if pad == 1 else b"\x01" + b"\x00" * (pad - 2) + b"\x80")
    a = [[0] * 5 for _ in range(5)]
    for off in range(0, len(msg), rate):
        block = msg[off:off + rate]
        for i in range(rate // 8):
            a[i % 5][i // 5] ^= int.from_bytes(block[8 * i:8 * i + 8], "little")
        a = _keccak_f(a)
    return b"".join(a[i % 5][i // 5].to_bytes(8, "little") for i in range(4)).hex()


def gz(b):
    """Deterministic gzip: no file name, mtime 0, level 9."""
    out = io.BytesIO()
    with gzip.GzipFile(filename="", mode="wb", fileobj=out, compresslevel=9, mtime=0) as fh:
        fh.write(b)
    return out.getvalue()


def write_if_changed(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    if not path.exists() or path.read_bytes() != data:
        path.write_bytes(data)


def dumps(v):
    return json.dumps(v, indent=1, sort_keys=True, ensure_ascii=True) + "\n"


# ---------------------------------------------------------------------------------------------
# Bitcoin mainnet addresses (checksums), canonical spelling `btc:<address>`
# ---------------------------------------------------------------------------------------------

B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
BECH32 = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"


def b58check(s):
    n = 0
    for c in s:
        i = B58.find(c)
        if i < 0:
            return None
        n = n * 58 + i
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big") if n else b""
    raw = b"\x00" * (len(s) - len(s.lstrip("1"))) + raw
    if len(raw) < 5:
        return None
    body, check = raw[:-4], raw[-4:]
    if hashlib.sha256(hashlib.sha256(body).digest()).digest()[:4] != check:
        return None
    return body


def _polymod(values):
    gen = [0x3B6A57B2, 0x26508E6D, 0x1EA119FA, 0x3D4233DD, 0x2A1462B3]
    chk = 1
    for v in values:
        top = chk >> 25
        chk = (chk & 0x1FFFFFF) << 5 ^ v
        for i in range(5):
            chk ^= gen[i] if (top >> i) & 1 else 0
    return chk


def _convert(data, frm, to):
    acc, bits, out = 0, 0, []
    for v in data:
        acc = (acc << frm) | v
        bits += frm
        while bits >= to:
            bits -= to
            out.append((acc >> bits) & ((1 << to) - 1))
    if bits >= frm or (acc << (to - bits)) & ((1 << to) - 1):
        return None
    return out


def segwit(s):
    """(version, program) of a lowercase mainnet bech32/bech32m address, else None."""
    if s != s.lower() or not s.startswith("bc1") or len(s) > 90:
        return None
    data = [BECH32.find(c) for c in s[3:]]
    if len(data) < 7 or -1 in data:
        return None
    hrp = [ord(c) >> 5 for c in "bc"] + [0] + [ord(c) & 31 for c in "bc"]
    const = _polymod(hrp + data)
    ver = data[0]
    if const != (1 if ver == 0 else 0x2BC830A3):
        return None
    prog = _convert(data[1:-6], 5, 8)
    if prog is None or not 2 <= len(prog) <= 40 or ver > 16:
        return None
    if ver == 0 and len(prog) not in (20, 32):
        return None
    return ver, bytes(prog)


def canon(addr):
    """`btc:<addr>` for a valid mainnet address in its one canonical spelling, else None."""
    if addr.startswith("bc1"):
        return "btc:" + addr if segwit(addr) else None
    if addr[:1] in ("1", "3"):
        body = b58check(addr)
        if body is not None and len(body) == 21 and body[0] == (0 if addr[0] == "1" else 5):
            return "btc:" + addr
    return None


ADDRESS_TOKEN = re.compile(
    r"(?<![A-Za-z0-9])(?:bc1[ac-hj-np-z02-9]{6,87}|[13][a-km-zA-HJ-NP-Z1-9]{25,34})(?![A-Za-z0-9])"
)


def addresses_in_text(text, repair=False):
    """Every valid mainnet address in `text`. With `repair` (PDF text), a candidate that fails its
    checksum is joined with the next short lowercase bech32 token within the next three
    non-empty lines when that, and only that, makes a valid address (a wrapped table cell)."""
    if repair:  # PDF text: typographic ligatures (U+FB02 for "fl") back to letters
        text = unicodedata.normalize("NFKC", text)
    clean = text.replace("\u200b", " ").replace("\ufeff", " ")
    found, repairs = set(), 0
    lines = [ln for ln in clean.splitlines() if ln.strip()]
    for i, line in enumerate(lines):
        for tok in ADDRESS_TOKEN.findall(line):
            if canon(tok):
                found.add(canon(tok))
                continue
            if not (repair and tok.startswith("bc1")):
                continue
            for nxt in lines[i + 1:i + 4]:
                hit = None
                for piece in nxt.split():
                    if re.fullmatch(r"[ac-hj-np-z02-9]{1,30}", piece) and canon(tok + piece):
                        hit = canon(tok + piece)
                        break
                if hit:
                    found.add(hit)
                    repairs += 1
                    break
    return sorted(found), repairs


# ---------------------------------------------------------------------------------------------
# Parsers of the committed files (shared by import and listings)
# ---------------------------------------------------------------------------------------------


class Malformed(Exception):
    pass


def parse_okx_proofs(data):
    """okx/btc-proofs.csv.gz -> {snapshot id: [(address, proof)]}."""
    rows = list(csv.reader(io.StringIO(gzip.decompress(data).decode("utf-8"))))
    if rows[0] != ["address", "message", "signature1", "signature2", "script", "snapshots"]:
        raise Malformed("okx proofs header")
    out = {}
    for address, message, s1, s2, script, snaps in rows[1:]:
        c = canon(address)
        if c is None:
            raise Malformed(f"okx address {address!r}")
        proof = (address, message, tuple(s for s in (s1, s2) if s), script)
        for sid in snaps.split():
            out.setdefault(sid, []).append((c, proof))
    return out


def bitmex_addresses(text):
    """The `address:` section of a BitMEX reserves YAML: [(addr, addr_type, script)]. The
    `custodial:` section (customer deposit custody) is not read."""
    out, cur, inside = [], None, False
    for line in text.splitlines():
        if line and not line.startswith((" ", "-")):
            if cur:
                out.append(cur)
                cur = None
            inside = line == "address:"
            continue
        if not inside:
            continue
        m = re.fullmatch(r"  - \{(.*)\}", line)  # older files: one flow mapping per address
        if m:
            if cur:
                out.append(cur)
                cur = None
            pairs = re.findall(r"'(\w+)': '([^']*)'", m.group(1))
            item = dict(pairs)
            if len(pairs) != 4 or set(item) != {"addr", "addr_type", "script", "balance"}:
                raise Malformed(f"bitmex yaml item {line[:60]!r}")
            out.append(item)
            continue
        m = re.fullmatch(r"- addr: (\S+)", line)
        if m:
            if cur:
                out.append(cur)
            cur = {"addr": m.group(1)}
            continue
        m = re.fullmatch(r"  (addr_type|balance|script): (.*)", line)
        if m and cur is not None:
            cur[m.group(1)] = m.group(2).strip("'")
            continue
        if line.strip():
            raise Malformed(f"bitmex yaml line {line[:60]!r}")
    if cur:
        out.append(cur)
    return [(r["addr"], r.get("addr_type", ""), r.get("script", "")) for r in out]


def bitmex_extract(text):
    """The committed BitMEX extract: the snapshot height, then the `address:` section only (the
    `custodial:` section holds customer deposit custody and is never committed, review P2a-R1-4)."""
    buf = io.StringIO()
    w = csv.writer(buf, lineterminator="\n")
    w.writerow(["height", str(bitmex_height(text))])
    w.writerow(["addr", "addr_type", "script"])
    for row in bitmex_addresses(text):
        w.writerow(row)
    return buf.getvalue().encode("utf-8")


def parse_bitmex_extract(data):
    rows = list(csv.reader(io.StringIO(data.decode("utf-8"))))
    if rows[0][0] != "height" or rows[1] != ["addr", "addr_type", "script"]:
        raise Malformed("bitmex extract header")
    return int(rows[0][1]), [tuple(r) for r in rows[2:]]


def bitmex_height(text):
    m = re.search(r"^height:(?: |\n  )(\d+)$", text, re.M)
    if not m:
        raise Malformed("bitmex yaml has no height")
    return int(m.group(1))


def parse_binance(data):
    """Binance HotCold member: native BTC rows held by Binance (a third-party custodian's rows
    are custody, not exchange addresses, and are skipped). -> (addresses, heights, custodian rows)."""
    rows = list(csv.reader(io.StringIO(data.decode("utf-8-sig"))))
    head = [h.strip().lower().replace("_", " ") for h in rows[0]]
    want = ["coin", "network", "address", "balance", "height", "third party custodian name"]
    if head != want:
        raise Malformed(f"binance header {rows[0]}")
    addrs, heights, custody = set(), set(), 0
    for r in rows[1:]:
        if len(r) < 6 or r[0] != "BTC" or r[1] != "BTC":
            continue
        if r[5].strip():
            custody += 1
            continue
        c = canon(r[2].strip())
        if c is None:
            raise Malformed(f"binance address {r[2]!r}")
        addrs.add(c)
        heights.add(r[4].strip())
    return sorted(addrs), heights, custody


def parse_htx(data):
    """HTX tagged reserve CSV: (native BTC height, [(address, proof or None)])."""
    rows = list(csv.reader(io.StringIO(data.decode("utf-8-sig"))))
    native, every, listed, mode = None, None, [], None
    height = None
    for r in rows:
        cells = [c.strip() for c in r]
        low = [c.lower() for c in cells]
        if low[:3] == ["coin", "snapshot height", "balance"]:
            mode = "summary"
            continue
        if low[:6] == ["coin", "address", "snapshot height", "balance", "message", "signature"]:
            mode = "detail"
            height = native if native is not None else every
            continue
        if not cells or not cells[0]:
            continue
        coin = re.sub(r"\s+", "", cells[0]).upper()
        if mode == "summary" and coin in ("BTC", "BTC(ALL)"):
            if coin == "BTC":
                if not cells[1].isdigit():
                    raise Malformed(f"htx summary height {cells[1]!r}")
                native = int(cells[1])
            elif cells[1].isdigit():
                every = int(cells[1])
        elif mode == "detail" and coin == "BTC":
            address, h, _bal, message, sig = cells[1:6]
            if height is None or h != str(height):
                raise Malformed(f"htx height {h!r} vs {height!r}")
            c = canon(address)
            if c is None:
                raise Malformed(f"htx address {address!r}")
            proof = (address, message, (sig,), "") if sig not in ("", "-") else None
            listed.append((c, proof))
    height = height if height is not None else (native if native is not None else every)
    if height is None:
        raise Malformed("htx csv has no native BTC height")
    return height, listed


XML_NS = "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}"


def xlsx_rows(data):
    z = zipfile.ZipFile(io.BytesIO(data))
    sheets = sorted(n for n in z.namelist() if re.fullmatch(r"xl/worksheets/sheet\d+\.xml", n))
    if len(sheets) != 1:
        raise Malformed("coinex workbook sheets")
    shared = []
    if "xl/sharedStrings.xml" in z.namelist():
        for si in ET.fromstring(z.read("xl/sharedStrings.xml")).findall(XML_NS + "si"):
            shared.append("".join(t.text or "" for t in si.iter(XML_NS + "t")))
    rows = []
    for row in ET.fromstring(z.read(sheets[0])).findall(".//" + XML_NS + "row"):
        cells = {}
        for c in row.findall(XML_NS + "c"):
            ref = re.match(r"[A-Z]+", c.get("r", "")).group()
            col = 0
            for ch in ref:
                col = col * 26 + ord(ch) - 64
            v, inline = c.find(XML_NS + "v"), c.find(XML_NS + "is")
            text = ""
            if v is not None:
                text = v.text or ""
                if c.get("t") == "s":
                    text = shared[int(text)]
            elif inline is not None:
                text = "".join(t.text or "" for t in inline.iter(XML_NS + "t"))
            cells[col - 1] = text.strip()
        rows.append([cells.get(i, "") for i in range(max(cells) + 1 if cells else 0)])
    return rows


def rows_csv(rows):
    """The worksheet's cell values as CSV (the committed CoinEx extract): no workbook metadata
    (its author fields name a person, review P2a-R1-4), cells exactly as the parser reads them."""
    buf = io.StringIO()
    w = csv.writer(buf, lineterminator="\n")
    for r in rows:
        w.writerow(r)
    return buf.getvalue().encode("utf-8")


def parse_coinex(data):
    """CoinEx wallet workbook rows (`rows_csv` of the XLSX): (height, [(address, proof or None)]).
    Hot-wallet rows carry a direct signature; cold multisig rows carry owner-key signatures that
    CoinEx does not bind to the reserve address (no redeem script), so they are listed without a
    proof."""
    rows = list(csv.reader(io.StringIO(data.decode("utf-8"))))
    height, listed, mode = None, {}, None
    for r in rows:
        head = r[:6]
        if r[:3] == ["coin", "block_height", "amount"]:
            mode = "total"
            continue
        if r[:5] == ["coin", "address", "amount", "message", "signature"]:
            mode = "hot"
            continue
        if r[:5] == ["coin", "address", "amount", "owner_address", "message"]:
            mode = "cold"
            continue
        if r[:1] and r[0] in ("ETH Staking", "CET Staking", "Escrow", "Hot Wallet", "Cold Wallet"):
            mode = None if r[0] in ("ETH Staking", "CET Staking", "Escrow") else mode
            continue
        if not head or head[0] != "BTC":
            continue
        if mode == "total" and len(r) > 2 and r[1].isdigit():
            if height is not None:
                raise Malformed("coinex: two BTC total rows")
            height = int(r[1])
        elif mode == "hot":
            address, message, sig = r[1], r[3] if len(r) > 3 else "", r[4] if len(r) > 4 else ""
            c = canon(address)
            if c is None or not message or not sig:
                raise Malformed(f"coinex hot row {address!r}")
            listed[c] = (address, message, (sig,), "")
        elif mode == "cold":
            c = canon(r[1])
            if c is None:
                raise Malformed(f"coinex cold row {r[1]!r}")
            listed.setdefault(c, None)
    if height is None:
        raise Malformed("coinex workbook has no BTC height")
    return height, sorted(listed.items())


def parse_mexc(data):
    rows = list(csv.reader(io.StringIO(data.decode("utf-8-sig"))))
    if rows[0][:4] != ["资产", "链", "钱包地址", "余额"]:
        raise Malformed(f"mexc header {rows[0][:4]}")
    out = set()
    for r in rows[1:]:
        if len(r) >= 3 and r[0].strip() == "BTC" and r[1].strip() == "Bitcoin(BTC)":
            c = canon(r[2].strip())
            if c is None:
                raise Malformed(f"mexc address {r[2]!r}")
            out.add(c)
    return sorted(out)


def parse_bitfinex(data):
    out = set()
    for line in data.decode("utf-8").splitlines():
        m = re.fullmatch(r"(\S+) - BTC (?:hot|cold) wallet", line.strip())
        if m:
            c = canon(m.group(1))
            if c is None:
                raise Malformed(f"bitfinex address {m.group(1)!r}")
            out.add(c)
    return sorted(out)


def parse_addresses_file(data):
    lines = data.decode("ascii").splitlines()
    if lines != sorted(set(lines)) or any(canon(a) is None for a in lines):
        raise Malformed("addresses file must hold sorted unique valid addresses")
    return [canon(a) for a in lines]


def addresses_file(addrs):
    return "".join(a.removeprefix("btc:") + "\n" for a in sorted(set(addrs))).encode("ascii")


# ---------------------------------------------------------------------------------------------
# import: indexer run -> fixtures/rosters/<key>/
# ---------------------------------------------------------------------------------------------


def rec(sid, issued, fetched, url, height, basis, raw, file, extra=None):
    r = {"id": sid, "issued": issued, "fetched": fetched, "url": url, "height": int(height),
         "height_basis": basis, "raw_sha256": sha256(raw), "raw_keccak": keccak(raw),
         "raw_bytes": len(raw), "file": file}
    if extra:
        r.update(extra)
    return r


def read_csv(path):
    with open(path, newline="", encoding="utf-8") as fh:
        return list(csv.DictReader(fh))


def boundaries(path):
    return {r["boundary_id"]: r for r in read_csv(path)}


def import_okx(run, out):
    snaps = json.loads((run / "normalized/snapshots.json").read_text())
    proofs, records, skipped = {}, [], []
    for s in snaps:
        name = s["source"].rsplit("/", 1)[1]
        if s["snapshot_date"] < CUTOFF["okx"]:
            skipped.append({"id": s["snapshot_date"], "url": s["source"], "raw_sha256": s["source_hash"],
                            "addresses": s["address_count"], "reason": "before the size cutoff"})
            continue
        raw = (run / "raw/snapshots" / name).read_bytes()
        if sha256(raw) != s["source_hash"]:
            raise Malformed(f"okx {name}: sha256 differs from the indexer's")
        z = zipfile.ZipFile(io.BytesIO(raw))
        members = [n for n in z.namelist() if n.endswith(".csv") and "staking" not in n.lower()]
        if len(members) != 1:
            raise Malformed(f"okx {name}: members {members}")
        header, heights, n = None, set(), 0
        with z.open(members[0]) as fh:
            for parts in csv.reader(io.TextIOWrapper(fh, encoding="utf-8", newline="")):
                cells = [p.strip() for p in parts]
                if "address" in cells and "message" in cells and "signature1" in cells:
                    header = {c: i for i, c in enumerate(cells)}
                    continue
                if header is None or len(cells) <= header["redeem script/ public key"] or cells[0] != "BTC":
                    continue
                if cells[header["Network"]] != "BTC":
                    continue
                key = tuple(cells[header[c]] for c in
                            ("address", "message", "signature1", "signature2", "redeem script/ public key"))
                if canon(key[0]) is None:
                    raise Malformed(f"okx {name}: address {key[0]!r}")
                proofs.setdefault(key, []).append(s["snapshot_date"])
                heights.add(cells[header["Snapshot Height"]])
                n += 1
        if heights != {str(s["snapshot_height"])} or n != s["address_count"]:
            raise Malformed(f"okx {name}: heights {heights} rows {n} vs indexer {s['snapshot_height']} {s['address_count']}")
        records.append(rec(s["snapshot_date"], s["snapshot_date"], run.name[:4] + "-" + run.name[4:6] + "-" + run.name[6:8],
                           s["source"], s["snapshot_height"], "issuer", raw, "btc-proofs.csv.gz",
                           {"rows": n}))
    buf = io.StringIO()
    w = csv.writer(buf, lineterminator="\n")
    w.writerow(["address", "message", "signature1", "signature2", "script", "snapshots"])
    for key in sorted(proofs):
        w.writerow(list(key) + [" ".join(sorted(proofs[key]))])
    write_if_changed(out / "btc-proofs.csv.gz", gz(buf.getvalue().encode("utf-8")))
    return records, skipped


def import_bitmex(run, out):
    snaps = json.loads((run / "normalized/snapshots.json").read_text())
    records, skipped = [], []
    for s in snaps:
        name = s["source"].rsplit("/", 1)[1]
        if s["snapshot_date"] < CUTOFF["bitmex"]:
            skipped.append({"id": name.split("-reserves-")[0] + "-" + str(s["height"]), "url": s["source"],
                            "raw_sha256": s["source_hash"], "addresses": s["address_count"],
                            "reason": "before the size cutoff"})
            continue
        raw = gzip.decompress((run / "raw/snapshots" / (name + ".gz")).read_bytes())
        if sha256(raw) != s["source_hash"]:
            raise Malformed(f"bitmex {name}: sha256")
        text = raw.decode("utf-8")
        h = bitmex_height(text)
        if h != s["height"] or f"-reserves-{h}-" not in name:
            raise Malformed(f"bitmex {name}: height {h}")
        sid = f"{s['snapshot_date']}-{h}"
        file = f"{sid}.address.csv.gz"
        write_if_changed(out / file, gz(bitmex_extract(text)))
        records.append(rec(sid, s["snapshot_date"], "2026-08-22", s["source"], h, "issuer", raw, file,
                           {"extract": "height and the address section (addr, addr_type, script); the custodial section is never committed"}))
    return records, skipped


def import_binance(run, out):
    records = []
    for src in sorted((run / "raw").glob("*.source.json")):
        meta = json.loads(src.read_text())
        day = meta["snapshot_date"]
        raw = (run / "raw" / f"{day}.csv").read_bytes()
        if sha256(raw) != meta["member_sha256"]:
            raise Malformed(f"binance {day}: sha256")
        addrs, heights, _ = parse_binance(raw)
        if len(heights) != 1:
            raise Malformed(f"binance {day}: heights {heights}")
        file = f"{day}.csv.gz"
        write_if_changed(out / file, gz(raw))
        issued = f"{day[:4]}-{day[4:6]}-{day[6:]}"
        records.append(rec(issued, issued, meta["retrieved_at"][:10], meta["source_url"], heights.pop(),
                           "issuer", raw, file, {"zip_member": meta["member_name"]}))
    return records, []


def import_htx(run, out):
    records = []
    for s in read_csv(run / "normalized/snapshots.csv"):
        raw = (run / "raw/snapshots" / f"{s['snapshot_tag']}.csv").read_bytes()
        if sha256(raw) != s["source_sha256"]:
            raise Malformed(f"htx {s['snapshot_tag']}: sha256")
        h, _ = parse_htx(raw)
        if str(h) != s["native_bitcoin_height"]:
            raise Malformed(f"htx {s['snapshot_tag']}: height")
        file = f"{s['snapshot_tag']}.csv.gz"
        write_if_changed(out / file, gz(raw))
        records.append(rec(s["snapshot_date"], s["snapshot_date"], "2026-08-24", s["source_url"], h,
                           "issuer", raw, file))
    return records, []


def import_coinex(run, out):
    records = []
    for s in read_csv(run / "normalized/snapshots.csv"):
        name = s["wallet_proof_file"].rsplit("/", 1)[1]
        raw = (run / "raw/wallets" / name).read_bytes()
        if sha256(raw) != s["source_sha256"]:
            raise Malformed(f"coinex {name}: sha256")
        extract = rows_csv(xlsx_rows(raw))
        h, _ = parse_coinex(extract)
        if str(h) != s["snapshot_height"]:
            raise Malformed(f"coinex {name}: height {h} vs {s['snapshot_height']}")
        file = name.removesuffix(".xlsx") + ".rows.csv.gz"
        write_if_changed(out / file, gz(extract))
        records.append(rec(s["snapshot_date"], s["snapshot_date"], "2026-08-24", s["wallet_proof_file"], h,
                           "issuer", raw, file, {"extract": "worksheet cell values as CSV (no workbook metadata)"}))
    return records, []


def import_mexc(run, bounds_path, out):
    bounds = boundaries(bounds_path)
    records, skipped = [], []
    for s in read_csv(run / "normalized/wallet-snapshots.csv"):
        name = s["source_url"].rsplit("/", 1)[1]
        if s["roster_file_date"] < CUTOFF["mexc"]:
            skipped.append({"id": s["roster_file_date"], "url": s["source_url"], "raw_sha256": s["source_sha256"],
                            "addresses": int(s["unique_native_addresses"]), "reason": "before the size cutoff"})
            continue
        raw = (run / "raw/wallets" / name).read_bytes()
        if sha256(raw) != s["source_sha256"]:
            raise Malformed(f"mexc {name}: sha256")
        b = bounds[f"snapshot:{s['snapshot_timestamp_ms']}"]
        if int(b["next_height"]) != int(b["height"]) + 1:
            raise Malformed("mexc boundary")
        file = name + ".gz"
        write_if_changed(out / file, gz(raw))
        records.append(rec(s["roster_file_date"], s["roster_file_date"], "2026-08-24", s["source_url"],
                           b["next_height"], "indexer-mapped", raw, file,
                           {"snapshot_at": s["snapshot_at_utc"],
                            "mapping": {"last_block_at_or_before": int(b["height"]),
                                        "its_time": int(b["block_timestamp_raw"]),
                                        "next_block_time": int(b["next_block_timestamp_raw"])}}))
    return records, skipped


def import_bybit(run, bounds_path, out, prior_lists=None):
    bounds = boundaries(bounds_path)
    indexer = {s["snapshot_date"]: s for s in json.loads((run / "normalized/snapshots.json").read_text())}
    records = []
    for src in sorted((run / "raw").glob("*.source.json")):
        meta = json.loads(src.read_text())
        day = meta["snapshot_date"]
        raw = (run / "raw" / f"{day.replace('-', '')}.pdf").read_bytes()
        if sha256(raw) != meta["sha256"]:
            raise Malformed(f"bybit {day}: sha256")
        text = subprocess.run(["pdftotext", "-layout", "-", "-"], input=raw, capture_output=True,
                              check=True).stdout.decode("utf-8")
        addrs, repairs = addresses_in_text(text, repair=True)
        theirs = sorted("btc:" + a for a in indexer[day]["addresses"])
        if addrs != theirs:
            raise Malformed(f"bybit {day}: {len(addrs)} addresses differ from the indexer's {len(theirs)}")
        # The audit time when the report states one ("point"), else the latest instant of the
        # audit date (UTC-12): the first block after it, never earlier than the audit.
        point = f"point:{day}" in bounds
        b = bounds[f"point:{day}" if point else f"latest:{day}"]
        if int(b["next_height"]) != int(b["height"]) + 1:
            raise Malformed("bybit boundary")
        file = f"{day}.addresses"
        write_if_changed(out / file, addresses_file(addrs))
        records.append(rec(day, day, meta["retrieved_at"][:10], meta["url"], int(b["next_height"]),
                           "indexer-mapped", raw, file,
                           {"extract": "pdftotext -layout (poppler), wrapped cells joined: %d" % repairs,
                            "mapping": {"last_block_at_or_before": int(b["height"]),
                                        "its_time": int(b["block_timestamp_raw"]),
                                        "next_block_time": int(b["next_block_timestamp_raw"]),
                                        "of": "the audit time stated in the report" if point
                                        else "the end of the audit date at UTC-12 (no time stated)"}}))
    return records, []


def import_deribit(run, out):
    raw = (run / "raw/article.json").read_bytes()
    art = json.loads(raw)["article"]
    addrs, _ = addresses_in_text(art["body"])
    tip = int((run / "raw/bitcoin-tip-height-after.txt").read_text().strip())
    day = "2026-08-22"
    file = f"{day}.addresses"
    write_if_changed(out / file, addresses_file(addrs))
    return [rec(day, art["edited_at"][:10], day, art["html_url"], tip + 1, "fetch-tip", raw, file,
                {"note": "current roster read from the issuer's support-article API; listed at the fetch; the article was last edited on its issued date"})], []


def import_bitfinex(run, boundary_path, out):
    b = json.loads(Path(boundary_path).read_text())
    first = run / "raw/wallets" / (b["source_commit"] + ".txt")
    current = run / "raw/wallets-current.txt"
    tip = int((run / "raw/blockstream-tip-height.txt").read_text().strip())
    records = []
    for sid, path, issued, url, height, basis in (
        ("2022-11-11", first, "2022-11-11",
         f"https://raw.githubusercontent.com/bitfinexcom/pub/{b['source_commit']}/wallets.txt",
         b["first_post_evidence_block"]["height"], "indexer-mapped"),
        ("2026-08-24", current, "2026-08-24",
         "https://raw.githubusercontent.com/bitfinexcom/pub/main/wallets.txt", tip + 1, "fetch-tip"),
    ):
        raw = path.read_bytes()
        parse_bitfinex(raw)
        file = f"{sid}.wallets.txt"
        write_if_changed(out / file, raw)
        records.append(rec(sid, issued, "2026-08-24", url, height, basis, raw, file))
    return records, [{"id": "2022-12-12", "url": "https://raw.githubusercontent.com/bitfinexcom/pub/83d06bdb5f5a2ce05d5ba2db5a426dd44c9f5128/wallets.txt",
                      "reason": "no block height mapped for this commit; its Bitcoin wallets equal the current file's"}]


def import_cryptocom(run, boundary_path, out):
    b = json.loads(Path(boundary_path).read_text())
    raw = (run / "raw/wallet-disclosure.html").read_bytes()
    addrs, _ = addresses_in_text(raw.decode("utf-8"))
    day = b["source_date"]
    file = f"{day}.addresses"
    write_if_changed(out / file, addresses_file(addrs))
    return [rec(day, day, "2026-08-24", "https://crypto.com/en/company-news/transparency-first",
                b["first_next_day_block"]["height"], "indexer-mapped", raw, file,
                {"note": "first block of the next UTC day (the date has no time)"})], []


def import_bitb(run, tip_height, out):
    raw = (run / "raw/product-page.html").read_bytes()
    text = raw.decode("utf-8")
    m = re.search(r'"wallets":\{"walletBalances":(\[.*?\]),"updatedAt":"([0-9T:.\-Z]+)"', text)
    if not m:
        raise Malformed("bitb page has no wallet list")
    wallets = json.loads(m.group(1))
    addrs = sorted({canon(w["address"]) for w in wallets if w.get("active") is True})
    if None in addrs or len(addrs) != len(wallets):
        raise Malformed("bitb wallet list")
    day = m.group(2)[:10]
    file = f"{day}.addresses"
    write_if_changed(out / file, addresses_file(addrs))
    return [rec(day, day, day, "https://bitbetf.com/", int(tip_height), "bound", raw, file,
                {"note": "a block mined after the fetch (observed by the indexer later that day): the first block after the fetch is at or before it"})], []


def cmd_import(args):
    key = args[0]
    if key in EXCLUDED:
        raise SystemExit(f"{key} is excluded: {EXCLUDED[key]}")
    if key not in SOURCES:
        raise SystemExit(f"unknown source {key}")
    out = ROSTERS / key
    if "--out" in args:
        i = args.index("--out")
        out = Path(args[i + 1])
        args = args[:i] + args[i + 2:]
    run = Path(args[1])
    fn = {
        "okx": lambda: import_okx(run, out), "bitmex": lambda: import_bitmex(run, out),
        "binance": lambda: import_binance(run, out), "htx": lambda: import_htx(run, out),
        "coinex": lambda: import_coinex(run, out), "mexc": lambda: import_mexc(run, Path(args[2]), out),
        "bybit": lambda: import_bybit(run, Path(args[2]), out), "deribit": lambda: import_deribit(run, out),
        "bitfinex": lambda: import_bitfinex(run, args[2], out),
        "cryptocom": lambda: import_cryptocom(run, args[2], out), "bitb": lambda: import_bitb(run, args[2], out),
    }[key]
    records, skipped = fn()
    records.sort(key=lambda r: r["height"])
    source, entity, kind, name = SOURCES[key]
    files = sorted({r["file"] for r in records})
    manifest = {"key": key, "source": source, "entity": entity, "kind": kind, "name": name,
                "indexer_run": "/".join(run.parts[-3:]) if "runs" in run.parts else "staging/" + "/".join(run.parts[-2:]),
                "files": {f: sha256((out / f).read_bytes()) for f in files},
                "snapshots": records, "skipped": sorted(skipped, key=lambda s: s["id"])}
    write_if_changed(out / "snapshots.json", dumps(manifest).encode())
    print(f"{key}: {len(records)} snapshots, {len(skipped)} skipped, files {len(files)}")


# ---------------------------------------------------------------------------------------------
# listings: committed files -> normalized listings
# ---------------------------------------------------------------------------------------------


def snapshot_listing(key, r, data, okx):
    """[(address, proof or None)] for one snapshot of a committed source."""
    if key == "okx":
        got = okx[r["id"]]
        if len(got) != r["rows"]:
            raise Malformed(f"okx {r['id']}: rows")
        return got
    if key == "bitmex":
        height, rows = parse_bitmex_extract(gzip.decompress(data))
        if height != r["height"]:
            raise Malformed("bitmex height")
        out = []
        for addr, _t, script in rows:
            c = canon(addr)
            if c is None:
                raise Malformed(f"bitmex address {addr!r}")
            out.append((c, (addr, "", (), script) if script else None))
        return out
    if key == "binance":
        addrs, heights, _ = parse_binance(gzip.decompress(data))
        if heights != {str(r["height"])}:
            raise Malformed("binance height")
        return [(a, None) for a in addrs]
    if key == "htx":
        h, listed = parse_htx(gzip.decompress(data))
        if h != r["height"]:
            raise Malformed("htx height")
        return listed
    if key == "coinex":
        h, listed = parse_coinex(gzip.decompress(data))
        if h != r["height"]:
            raise Malformed("coinex height")
        return listed
    if key == "mexc":
        return [(a, None) for a in parse_mexc(gzip.decompress(data))]
    if key == "bitfinex":
        return [(a, None) for a in parse_bitfinex(data)]
    return [(a, None) for a in parse_addresses_file(data)]


# Entries allowed directly under fixtures/rosters: one directory per source, the deny list and its
# basis. Anything else (an EXCLUDED source's directory above all) stops the build: the wBTC and
# cbBTC exclusions hold for the committed bytes, not only for what the set reads (review P2a-R2-2).
DENY_FILES = ("deny.txt", "deny.basis.json")
SAFE_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*")


def check_tree(rosters):
    """Refuse an unknown entry under `rosters`, a stray file in a source directory (one its
    snapshots.json does not name), and a manifest file name that is not a plain name in it."""
    for entry in sorted(rosters.iterdir()):
        name = entry.name
        if name in EXCLUDED:
            raise Malformed(f"{name} is excluded and may not be committed: {EXCLUDED[name]}")
        if entry.is_dir() and name in SOURCES:
            m = json.loads((entry / "snapshots.json").read_text())
            for f in m["files"]:
                if not SAFE_NAME.fullmatch(f) or f == "snapshots.json":
                    raise Malformed(f"{name}: file name {f!r} is not a plain file in its directory")
            for f in sorted(x.name for x in entry.iterdir()):
                if f != "snapshots.json" and f not in m["files"]:
                    raise Malformed(f"{name}/{f}: not named by its snapshots.json")
            continue
        if entry.is_file() and name in DENY_FILES:
            continue
        raise Malformed(f"unknown entry under fixtures/rosters: {name}")


def addresses_digest(addrs):
    return sha256("".join(a + "\n" for a in sorted(set(addrs))).encode("ascii"))


def check_deny_basis(rosters, listed):
    """The deny list is the roster addresses a government filing names, computed by `deny`. It is
    current only for the roster addresses it was computed against: any change to them (a new
    snapshot, a re-import) must re-run `deny`, or a newly listed filed address would ship
    (review P2a-R2-1). The basis also pins deny.txt itself."""
    path = rosters / "deny.basis.json"
    if not path.exists():
        raise Malformed("no deny.basis.json: run build-rosters.py deny <filings.csv>")
    basis = json.loads(path.read_text())
    if basis.get("format") != 1:
        raise Malformed("deny.basis.json format")
    if basis["deny_sha256"] != sha256((rosters / "deny.txt").read_bytes()):
        raise Malformed("deny.txt differs from the one deny.basis.json records: run build-rosters.py deny")
    if basis["roster_addresses_sha256"] != addresses_digest(listed) or basis["roster_addresses"] != len(set(listed)):
        raise Malformed("the roster addresses changed since the deny list was built: "
                        "run build-rosters.py deny <filings.csv> before building the set")


def listings(rosters=None):
    rosters = ROSTERS if rosters is None else rosters
    check_tree(rosters)
    sources, proofs, every = [], {}, set()
    for key in sorted(SOURCES):
        mpath = rosters / key / "snapshots.json"
        if not mpath.exists():
            raise Malformed(f"{key}: no snapshots.json")
        m = json.loads(mpath.read_text())
        if (m["source"], m["entity"], m["kind"], m["name"]) != SOURCES[key]:
            raise Malformed(f"{key}: manifest header")
        data = {}
        for f, want in m["files"].items():
            b = (rosters / key / f).read_bytes()
            if sha256(b) != want:
                raise Malformed(f"{key}/{f}: sha256 differs from snapshots.json")
            data[f] = b
        okx = parse_okx_proofs(data["btc-proofs.csv.gz"]) if key == "okx" else None
        snaps = []
        for r in m["snapshots"]:
            if key not in ("okx",) and r["file"] not in data:
                raise Malformed(f"{key}: file {r['file']}")
            raw_is_file = key in ("binance", "htx", "mexc") and sha256(gzip.decompress(data[r["file"]])) != r["raw_sha256"]
            if raw_is_file or (key == "bitfinex" and sha256(data[r["file"]]) != r["raw_sha256"]):
                raise Malformed(f"{key} {r['id']}: committed raw does not hash to raw_sha256")
            seen = {}
            for addr, proof in snapshot_listing(key, r, data.get(r["file"]), okx):
                pid = None
                if proof is not None:
                    pid = proofs.setdefault(proof, len(proofs))
                if addr in seen and seen[addr] != pid:
                    # one address twice in one snapshot with different proofs: keep the address,
                    # no proof (it is still published by the issuer)
                    pid = None
                seen[addr] = pid
            every.update(seen)
            snaps.append({k: r[k] for k in ("id", "issued", "fetched", "url", "height", "height_basis",
                                             "raw_sha256", "raw_keccak")}
                         | {"listed": sorted([a, p] for a, p in seen.items())})
        sources.append({"key": key, "source": m["source"], "entity": m["entity"], "kind": m["kind"],
                        "name": m["name"], "snapshots": snaps})
    # Renumber proofs in sorted order so the file does not depend on reading order.
    order = sorted(proofs)
    new = {p: i for i, p in enumerate(order)}
    old_to_new = {i: new[p] for p, i in proofs.items()}
    for s in sources:
        for snap in s["snapshots"]:
            snap["listed"] = [[a, None if p is None else old_to_new[p]] for a, p in snap["listed"]]
    check_deny_basis(rosters, every)
    deny = parse_addresses_file((rosters / "deny.txt").read_bytes()) if (rosters / "deny.txt").read_bytes() else []
    return {"format": 1, "generated_by": "python3 -I scripts/build-rosters.py listings <out.json>",
            "sources": sources,
            "proofs": [{"address": a, "message": msg, "signatures": list(sigs), "script": sc}
                       for a, msg, sigs, sc in order],
            "deny": deny}


def cmd_listings(args):
    rosters = None
    if "--rosters" in args:  # another copy of fixtures/rosters (tests)
        i = args.index("--rosters")
        rosters = Path(args[i + 1])
        args = args[:i] + args[i + 2:]
    doc = json.dumps(listings(rosters), sort_keys=True, separators=(",", ":")) + "\n"
    out = Path(args[0])
    out.write_text(doc)
    n = sum(len(s["snapshots"]) for s in json.loads(doc)["sources"])
    print(f"wrote {out}: {n} snapshots", file=sys.stderr)


def cmd_deny(args):
    filed = set()
    with open(args[0], newline="", encoding="utf-8") as fh:
        for r in csv.DictReader(line for line in fh if not line.startswith("#")):
            a = (r.get("address") or "").strip()
            if canon(a):
                filed.add(canon(a))
    roster = set()
    for key in sorted(SOURCES):
        m = json.loads((ROSTERS / key / "snapshots.json").read_text())
        data = {f: (ROSTERS / key / f).read_bytes() for f in m["files"]}
        okx = parse_okx_proofs(data["btc-proofs.csv.gz"]) if key == "okx" else None
        for r in m["snapshots"]:
            roster.update(a for a, _ in snapshot_listing(key, r, data.get(r["file"]), okx))
    hit = sorted(roster & filed)
    deny_bytes = addresses_file(hit)
    write_if_changed(ROSTERS / "deny.txt", deny_bytes)
    fetched = ""
    with open(args[0], encoding="utf-8") as fh:
        for line in fh:
            if line.startswith("# fetched:"):
                fetched = line.split(":", 1)[1].strip()
                break
    basis = {"format": 1, "roster_addresses": len(roster), "roster_addresses_sha256": addresses_digest(roster),
             "filings_sha256": sha256(Path(args[0]).read_bytes()), "filings_fetched": fetched,
             "deny_sha256": sha256(deny_bytes), "denied": len(hit),
             "note": "deny.txt is current only for these roster addresses; listings refuses any change until deny is re-run"}
    write_if_changed(ROSTERS / "deny.basis.json", dumps(basis).encode())
    print(f"deny: {len(hit)} roster addresses named by a filing")


def cmd_fetch(args):
    key, cache = args[0], Path(args[1])
    m = json.loads((ROSTERS / key / "snapshots.json").read_text())
    cache.mkdir(parents=True, exist_ok=True)
    bad = 0
    for r in m["snapshots"]:
        req = urllib.request.Request(r["url"], headers={"User-Agent": USER_AGENT})
        with urllib.request.urlopen(req, timeout=120) as resp:
            body = resp.read()
        (cache / f"{r['id']}.raw").write_bytes(body)
        ok = sha256(body) == r["raw_sha256"]
        bad += not ok
        print(f"{key} {r['id']}: {'ok' if ok else 'DIFFERENT (the issuer changed or withdrew it)'}")
    sys.exit(1 if bad else 0)


# ---------------------------------------------------------------------------------------------
# self-test
# ---------------------------------------------------------------------------------------------


def self_test():
    fails = []

    def check(name, cond):
        if not cond:
            fails.append(name)

    check("keccak empty", keccak(b"") == "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470")
    check("keccak python empty", _keccak_py(b"") == keccak(b""))
    for n in (1, 135, 136, 137, 300):
        check(f"keccak python {n}", _keccak_py(b"a" * n) == keccak(b"a" * n))
    check("p2pkh", canon("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa") == "btc:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa")
    check("p2pkh bad checksum", canon("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNb") is None)
    check("p2sh", canon("3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy") is not None)
    check("p2wpkh", canon("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4") is not None)
    check("p2wpkh upper refused", canon("BC1QW508D6QEJXTDG4Y5R3ZARVARY0C5XW7KV8F3T4") is None)
    check("p2wpkh bad", canon("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t5") is None)
    check("p2tr", canon("bc1p5d7rjq7g6rdk2yhzks9smlaqtedr4dekq08ge8ztwac72sfr9rusxg3297") is not None)
    check("p2tr with bech32 checksum refused", canon("bc1p5d7rjq7g6rdk2yhzks9smlaqtedr4dekq08ge8ztwac72sfr9rusxg3298") is None)
    check("testnet refused", canon("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx") is None)
    full = "bc1qa2eu6p5rl9255e3xz7fcgm6snn4wl5kdfh7zpt05qp5fad9dmsys0qjg0e"
    text = f"   \u200b{full[:50]}\u200b\n \u200bBitcoin\u200b\n   \u200b{full[50:]}\u200b\n"
    got, rep = addresses_in_text(text, repair=True)
    check("wrapped cell joined", got == ["btc:" + full] and rep == 1)
    check("no repair outside pdf", addresses_in_text(text)[0] == [])
    lig = "bc1qs5vdqkusz4v7qac8ynx0vt9jrekwuupx2fl5udp9jql3sr03z3gsr2mf0f"
    check("pdf ligature undone", addresses_in_text(lig.replace("fl", "\ufb02"), repair=True)[0] == ["btc:" + lig])
    yaml = "address:\n- addr: 3BMEXNHBijbEUnDkFCkdM7odwQv11Bu93g\n  addr_type: sh\n  balance: '1'\n  script: 5141\nblockhash: x\ncustodial:\n- addr: 3E8PsgoGR6dhTSr2uiMbXmF2YjQmXV6rih\n  addr_type: deposit\nheight: 7\n"
    check("bitmex sections", bitmex_addresses(yaml) == [("3BMEXNHBijbEUnDkFCkdM7odwQv11Bu93g", "sh", "5141")])
    check("bitmex height", bitmex_height(yaml) == 7)
    old = "height:\n  9\naddress:\n  - {'addr_type': 'sh', 'addr': 'A', 'script': '51', 'balance': '1'}\nxpub:\n  - x\n"
    check("bitmex older layout", bitmex_height(old) == 9 and bitmex_addresses(old) == [("A", "sh", "51")])
    try:
        bitmex_addresses("address:\n- addr: x\n  surprise: 1\n")
        fails.append("bitmex unknown field accepted")
    except Malformed:
        pass
    b = "coin,network,address,balance,Height,Third party custodian name\nBTC,BTC,1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa,1,5,\nBTC,BTC,3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy,1,5,Ceffu\nBTC,BSC,0xab,1,5,\n"
    a, h, cust = parse_binance(b.encode())
    check("binance custody skipped", a == ["btc:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"] and h == {"5"} and cust == 1)
    try:
        parse_addresses_file(b"bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4\n1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa\n")
        fails.append("unsorted addresses file accepted")
    except Malformed:
        pass
    check("gz deterministic", gz(b"x") == gz(b"x") and gzip.decompress(gz(b"x")) == b"x")
    check("excluded never a source", not set(EXCLUDED) & set(SOURCES))
    if fails:
        print("self-test FAILED: " + ", ".join(fails))
        sys.exit(1)
    print("self-test ok")


def main():
    args = sys.argv[1:]
    if not args:
        raise SystemExit(__doc__)
    cmd, rest = args[0], args[1:]
    if cmd == "self-test":
        return self_test()
    if cmd == "import":
        return cmd_import(rest)
    if cmd == "deny":
        return cmd_deny(rest)
    if cmd == "listings":
        if "--check" in rest:
            i = rest.index("--check")
            committed = Path(rest[i + 1])
            fresh = json.dumps(listings(), sort_keys=True, separators=(",", ":")) + "\n"
            if committed.read_text() != fresh:
                raise SystemExit("listings differ from the committed files")
            print("listings reproduce")
            return None
        return cmd_listings(rest)
    if cmd == "fetch":
        return cmd_fetch(rest)
    raise SystemExit(__doc__)


if __name__ == "__main__":
    try:
        main()
    except Malformed as e:
        raise SystemExit(f"malformed: {e}")
