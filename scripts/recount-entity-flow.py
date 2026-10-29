#!/usr/bin/env python3
# Independent recount of entity net flow (Python, not the Rust recipe), for the golden and spread tests.
# Usage: python3 -I scripts/recount-entity-flow.py fixtures/btc/blocks-921080-921082.json fixtures/labels/test.onyx.entities.btc.v1.json
#    or: python3 -I scripts/recount-entity-flow.py fixtures-live/node fixtures/labels/test.onyx.entities.btc.v1.json 970543 970566
# "half" keeps every other OKX address in address order, as in golden.rs net_flow_spread_across_two_label_sets.
import json, sys
CH="qpzry9x8gf2tvdw0s3jn54khce6mua7l"
def polymod(v):
    g=[0x3b6a57b2,0x26508e6d,0x1ea119fa,0x3d4233dd,0x2a1462b3]; c=1
    for x in v:
        b=c>>25; c=(c&0x1ffffff)<<5^x
        for i in range(5): c^=g[i] if (b>>i)&1 else 0
    return c
def bech32_script(a):
    hrp,data=a[:2],[CH.index(c) for c in a[3:]]
    assert hrp=="bc"
    chk=polymod([ord(x)>>5 for x in hrp]+[0]+[ord(x)&31 for x in hrp]+data)
    ver=data[0]; assert chk==(1 if ver==0 else 0x2bc830a3)
    acc=bits=0; prog=[]
    for v in data[1:-6]:
        acc=(acc<<5)|v; bits+=5
        while bits>=8: bits-=8; prog.append((acc>>bits)&255)
    return bytes([0 if ver==0 else 0x50+ver,len(prog)])+bytes(prog)
B58="123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
def b58_script(a):
    n=0
    for c in a: n=n*58+B58.index(c)
    raw=n.to_bytes(25,'big'); ver,h=raw[0],raw[1:21]
    if ver==0: return b'\x76\xa9\x14'+h+b'\x88\xac'
    if ver==5: return b'\xa9\x14'+h+b'\x87'
    raise ValueError(a)
def script(addr):
    a=addr.split(':',1)[1]
    return (bech32_script(a) if a.startswith('bc1') else b58_script(a)).hex()
import os, gzip
from decimal import Decimal
if os.path.isdir(sys.argv[1]):
    # Node mode: <dir> <labels> <from> <to>, reading getblock-3 JSON (independent of the Rust raw-block parser).
    lo, hi = int(sys.argv[3]), int(sys.argv[4])
    blocks = []
    for h in range(lo, hi + 1):
        d = json.load(gzip.open(os.path.join(sys.argv[1], f"{h}.v3.json.gz")))
        sats = lambda v: int(Decimal(str(v)) * 100000000)
        txs = []
        for t in d["tx"]:
            vin = [[None, None, i["prevout"]["scriptPubKey"]["hex"], sats(i["prevout"]["value"])] for i in t["vin"] if "prevout" in i]
            vout = [[o["scriptPubKey"]["hex"], sats(o["value"])] for o in t["vout"]]
            txs.append({"vin": vin, "vout": vout})
        blocks.append({"txs": txs})
else:
    blocks=json.load(open(sys.argv[1]))['blocks']
labels=json.load(open(sys.argv[2]))['labels']
def net(lbls):
    sc={script(l['address']):l['entity'] for l in lbls}
    out={}
    for b in blocks:
        for t in b['txs']:
            for v in t['vout']:
                e=sc.get(v[0])
                if e: out.setdefault(e,[0,0]); out[e][0]+=v[1]
            for v in t.get('vin',[]):
                if len(v)>=4:
                    e=sc.get(v[2])
                    if e: out.setdefault(e,[0,0]); out[e][1]+=v[3]
    return {e:(r,s,r-s) for e,(r,s) in out.items()}
print("full", net(labels))
okx=[l for l in labels if l['entity']=='entity:okx']
okx.sort(key=lambda l:l['address'])
half=[l for i,l in enumerate(okx) if i%2==0]+[l for l in labels if l['entity']!='entity:okx']
print("half", net(half))
