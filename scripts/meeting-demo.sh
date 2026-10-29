#!/usr/bin/env bash
# North-star loop NL26: the meeting demo in one command. Reads the live API and Base Sepolia (test
# network, tONYX test token with no value) and prints what a partner would click through, in plain
# words: the checked data series and their proof level, market 1's result, market 2's open pools,
# and the insurance pool. It only reads: no key, no transaction, nothing written.
#
# Usage: scripts/meeting-demo.sh
# Env: ONYX_API (https://onyx.cryptagraph.com), ONYX_MARKETS_RPC (https://sepolia.base.org),
# FOUNDRY_BIN (~/.foundry/bin). Addresses come from the site's own file
# (apps/web/static/markets/base-sepolia.json), so the script reads what the page reads.
set -euo pipefail
cd "$(dirname "$0")/.."
API=${ONYX_API:-https://onyx.cryptagraph.com}
RPC=${ONYX_MARKETS_RPC:-https://sepolia.base.org}
CAST=${FOUNDRY_BIN:-$HOME/.foundry/bin}/cast
FILE=apps/web/static/markets/base-sepolia.json
UA=onyx-dev
work=$(mktemp -d "$HOME/.cache/onyx-meeting.XXXXXX")
trap 'rm -rf "$work"' EXIT
# cast runs outside the repo: Foundry loads a .env from its working directory (M8-L55-F4).
castw() { (cd "$work" && "$CAST" "$@" --rpc-url "$RPC"); }
# A value of the site file by path; numbers index lists.
field() { python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1])); [d := d[int(k)] if isinstance(d, list) else d[k] for k in sys.argv[2:]]; print(d)' "$FILE" "$@"; }

[ "$(castw chain-id)" = 84532 ] || { echo "the RPC is not Base Sepolia (84532)" >&2; exit 1; }

echo "== 1. The API is up"
curl -sf -A "$UA" "$API/v1/health" | python3 -I -c '
import json, sys
h = json.load(sys.stdin)
print(f"   chain {h["chainId"]} (Base Sepolia), payments {"on" if h["acceptingPayments"] else "off (test network)"}")'

echo "== 2. Checked data series (proof level 2: several observers fetched the source and agreed on chain)"
# metric | subject | plain name | unit
missing=0
while IFS='|' read -r metric subject name unit; do
  curl -s -L -G -A "$UA" "$API/v1/observations/$metric" --data-urlencode "subject=$subject" \
    | python3 -I -c '
import json, sys
from decimal import Decimal
name, unit, subject = sys.argv[1:4]
d = json.load(sys.stdin)
day = subject.rsplit(":", 1)[1]
if "value" in d:
    if unit == "USD":
        v = f"${Decimal(d["value"]):,.2f}"
    elif unit == "USD million":
        v = f"${int(d["value"]):,} million"
    else:
        # One earthquake, not "1 earthquakes".
        v = f"{d["value"]} {unit[:-1] if d["value"] == "1" and unit.endswith("s") else unit}"
    print(f"   {name:46} {day}  {v}  (level {d["proofLevel"]})")
else:
    print(f"   {name:46} {day}  not served: {(d.get("error") or {}).get("message", "?")}")
    sys.exit(1)' "$name" "$unit" "$subject" || missing=$((missing + 1))
done <<'EOF'
us.treasury.total_public_debt_outstanding|us.treasury.federal_debt:2026-10-27|U.S. total public debt|USD
us.treasury.tga_closing_balance|us.treasury.operating_cash:2026-10-27|U.S. Treasury cash balance (TGA close)|USD million
noaa.ghcnd.usw00094728.tmax|noaa.ghcnd.usw00094728:2026-09-27|New York Central Park, daily high|°C
noaa.ghcnd.usw00094728.prcp|noaa.ghcnd.usw00094728:2026-09-27|New York Central Park, rain|mm
noaa.ghcnd.usw00003017.tmax|noaa.ghcnd.usw00003017:2026-09-27|Denver, daily high|°C
noaa.ghcnd.usw00003017.prcp|noaa.ghcnd.usw00003017:2026-09-27|Denver, rain|mm
usgs.earthquakes.m5_count|usgs.earthquakes.m5_world:2026-07-19|Earthquakes M5+, whole world|earthquakes
usgs.earthquakes.la300km_m2_5_count|usgs.earthquakes.la300km_m2_5:2026-07-20|Earthquakes M2.5+, 300 km of Los Angeles|earthquakes
EOF

[ "$missing" = 0 ] || { echo "   $missing series not served: fix before the demo" >&2; exit 1; }

# One market: `market(id)` on futures F, printed with its title from the site file.
market() {
  local futures=$1 id=$2 number=$3 title=$4
  castw call "$futures" 'market(uint256)((bytes32,uint64,uint64,uint64,uint8,uint8,uint32,uint32,uint32,uint128,uint128,uint128,uint128,int256,int256[],uint128[],uint32[]))' "$id" \
    | python3 -I -c '
import re, sys, time
number, title = sys.argv[1:3]
t = sys.stdin.read()
# cast prints ints with a [1.2e3] hint; drop the hints, then split the tuple.
t = re.sub(r"\s*\[[0-9.e+]+\]", "", t).strip()[1:-1]
head, rest = t.split("[", 1)
f = [x.strip() for x in head.split(",") if x.strip()]
arrays = [[int(v) for v in a.split(",") if v.strip()] for a in re.findall(r"([^\[\]]*)\]", "[" + rest)]
edges, pools, counts = arrays[0], arrays[1], arrays[2]
close, state, winner, bettors, total, value = int(f[1]), int(f[4]), int(f[5]), int(f[7]), int(f[9]), int(f[13])
T = 10**18
states = ["none", "open", "resolved", "void"]
def usd(c): return f"${c / 100 / 1e12:.2f}T"
labels = [f"under {usd(edges[0])}"] + [f"{usd(a)} to {usd(b)}" for a, b in zip(edges, edges[1:])] + [f"{usd(edges[-1])} or more"]
print(f"   Market {number}: {title}")
s = states[state] if state < 4 else str(state)
if s == "resolved":
    print(f"   SETTLED. Observed value ${value / 100:,.2f} (level 2, from the Treasury). Winning bucket: {labels[winner]}.")
elif s == "open":
    when = time.strftime("%Y-%m-%d %H:%M UTC", time.gmtime(close))
    print(f"   OPEN, trading closes {when}.")
else:
    print(f"   {s.upper()}.")
for k, lab in enumerate(labels):
    mark = "  <- won" if s == "resolved" and k == winner else ""
    print(f"     {lab:24} {pools[k] / T:7.1f} tONYX, {counts[k]} bet(s){mark}")
print(f"     pool {total / T:.1f} tONYX from {bettors} bettors")' "$number" "$title"
}

echo "== 3. Markets (test bets in tONYX, settled only from observed data, never a vote)"
market "$(field futures)" 1 1 "$(field markets 0 title)"
n=$(python3 -I -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["insurance"].get("markets", [])))' "$FILE")
for k in $(seq 0 $((n - 1))); do
  market "$(field insurance futures)" "$(field insurance markets "$k" id)" "$(field insurance markets "$k" number)" "$(field insurance markets "$k" title)"
done

echo "== 4. Test insurance (pays from observed data; the pool must cover every policy sold)"
ins=$(field insurance address)
pool=$(castw call "$ins" 'pool()(uint256)' | cut -d' ' -f1)
reserved=$(castw call "$ins" 'reserved()(uint256)' | cut -d' ' -f1)
python3 -I -c 'import sys; p, r = (int(x) / 10**18 for x in sys.argv[1:3]); print(f"   pool {p:.0f} tONYX, held for sold policies {r:.0f}, free {p - r:.0f}")' "$pool" "$reserved"
for k in 0 1; do
  id=$(field insurance products "$k" id)
  read -r _ _ state sold max buyby _ _ _ payout premium _ < <(castw call "$ins" 'product(uint256)((bytes32,uint8,uint8,uint32,uint32,uint64,uint64,uint64,int256,uint128,uint128,int256))' "$id" \
    | sed 's/ \[[^]]*\]//g' | tr -d '()' | tr ',' ' ')
  python3 -I -c '
import sys, time
title, state, sold, mx, buyby, payout, premium, pool, reserved = sys.argv[1:]
free = (int(pool) - int(reserved)) / 10**18
s = ["none", "open", "paid", "expired", "void"][int(state)]
open_now = s == "open" and time.time() < int(buyby)
sale = "for sale" if open_now and free >= int(payout) / 10**18 else ("no cover for sale" if open_now else s)
print(f"   {title}: pays {int(payout)/10**18:.0f}, premium {int(premium)/10**18:.0f} tONYX; {sold} of {mx} sold; {sale} until {time.strftime("%Y-%m-%d %H:%M UTC", time.gmtime(int(buyby)))}")' \
    "$(field insurance products "$k" title)" "$state" "$sold" "$max" "$buyby" "$payout" "$premium" "$pool" "$reserved"
done
echo "== Done: everything above was read just now from $API and Base Sepolia. Open $API/#/markets and press 'Read the chain' to see the same in the browser."
