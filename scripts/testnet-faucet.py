#!/usr/bin/env python3
"""Request testnet funds from the Coinbase Developer Platform (CDP) faucet.

Testnets only. Reads CDP_API_KEY_ID and CDP_API_KEY_SECRET from the
environment or from .env; never prints the secret.

API: POST https://api.cdp.coinbase.com/platform/v2/evm/faucet
     https://docs.cdp.coinbase.com/api-reference/v2/rest-api/faucets/request-funds-on-evm-test-networks
Auth: https://docs.cdp.coinbase.com/api-reference/v2/authentication
Per request the faucet sends 0.0001 ETH or 1 USDC (limits per rolling 24 h).

Usage:
  scripts/testnet-faucet.py 0xADDRESS --eth 20 --usdc 1
"""

import argparse
import base64
import json
import os
import re
import secrets
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

HOST = "api.cdp.coinbase.com"
PATH = "/platform/v2/evm/faucet"
TESTNETS = ("base-sepolia", "ethereum-sepolia")


def load_key():
    vals = {k: os.environ.get(k, "") for k in ("CDP_API_KEY_ID", "CDP_API_KEY_SECRET")}
    env = Path(__file__).resolve().parent.parent / ".env"
    if env.exists() and not all(vals.values()):
        for line in env.read_text().splitlines():
            m = re.match(r"^\s*(CDP_API_KEY_ID|CDP_API_KEY_SECRET)\s*=\s*(.*)$", line)
            if m and not vals[m.group(1)]:
                vals[m.group(1)] = m.group(2).strip().strip('"').strip("'")
    if not all(vals.values()):
        sys.exit("CDP_API_KEY_ID / CDP_API_KEY_SECRET missing (env or .env)")
    return vals["CDP_API_KEY_ID"], vals["CDP_API_KEY_SECRET"].replace("\\n", "\n")


def b64url(data):
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def jwt(key_id, secret, method):
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec, ed25519
    from cryptography.hazmat.primitives.asymmetric.utils import decode_dss_signature

    if "BEGIN" in secret:  # legacy ECDSA key (PEM)
        key = serialization.load_pem_private_key(secret.encode(), password=None)
        alg = "ES256"
    else:  # Ed25519: base64 of seed (32 B) + public key (32 B)
        raw = base64.b64decode(secret)
        key = ed25519.Ed25519PrivateKey.from_private_bytes(raw[:32])
        alg = "EdDSA"
    now = int(time.time())
    header = {"alg": alg, "kid": key_id, "typ": "JWT", "nonce": secrets.token_hex(8)}
    claims = {
        "sub": key_id,
        "iss": "cdp",
        "aud": ["cdp_service"],
        "nbf": now,
        "exp": now + 120,
        "uris": [f"{method} {HOST}{PATH}"],
    }
    signing = f"{b64url(json.dumps(header).encode())}.{b64url(json.dumps(claims).encode())}"
    if alg == "EdDSA":
        sig = key.sign(signing.encode())
    else:
        r, s = decode_dss_signature(key.sign(signing.encode(), ec.ECDSA(hashes.SHA256())))
        sig = r.to_bytes(32, "big") + s.to_bytes(32, "big")
    return f"{signing}.{b64url(sig)}"


def request(key_id, secret, network, address, token):
    body = json.dumps({"network": network, "address": address, "token": token}).encode()
    req = urllib.request.Request(
        f"https://{HOST}{PATH}",
        data=body,
        method="POST",
        headers={
            "Authorization": f"Bearer {jwt(key_id, secret, 'POST')}",
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return True, json.load(resp).get("transactionHash", "?")
    except urllib.error.HTTPError as e:
        try:
            err = json.load(e)
            msg = f"{e.code} {err.get('errorType', '')}: {err.get('errorMessage', '')}"
        except Exception:
            msg = f"{e.code}"
        return False, msg


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("address")
    ap.add_argument("--network", default="base-sepolia", choices=TESTNETS)
    ap.add_argument("--eth", type=int, default=0, help="ETH requests (0.0001 ETH each)")
    ap.add_argument("--usdc", type=int, default=0, help="USDC requests (1 USDC each)")
    args = ap.parse_args()
    if not re.fullmatch(r"0x[0-9a-fA-F]{40}", args.address):
        sys.exit("bad address")
    key_id, secret = load_key()
    failed = 0
    for token, n in (("eth", args.eth), ("usdc", args.usdc)):
        for i in range(n):
            ok, out = request(key_id, secret, args.network, args.address, token)
            print(f"{token} {i + 1}/{n}: {'tx ' + out if ok else 'FAILED ' + out}", flush=True)
            if not ok:
                failed += 1
                if out.startswith(("401", "403", "429")):
                    break
            time.sleep(1)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
