#!/usr/bin/env bash
# The NWS forecast of the Denver high as a testnet forecaster (north-star loop NL4, ADR-0036
# amendment 8, docs/ops/nws-relay.md): fetch the forecast once, state the coming days through the
# track-record intake on Base Sepolia, and write the publication a node serves on /v1/track.
#
# Usage: scripts/track-nws-sepolia.sh <export dir> [state dir]
#   <export dir>  the hosted node's export (deployment.json; the publication goes to track/)
#   [state dir]   default ~/.cache/onyx-nl4/track-nws (the intake state and every fetched forecast)
# ONYX_TRACK_NO_POST=1 only rewrites the publication (no fetch, no transaction).
#
# Keys (never printed): .onyx-testnet/keys/track-nws-relay.key signs the statements and
# .onyx-testnet/keys/track-nws-poster.key posts the headers (both made here once); on the first
# run the deployer key from .env (BASE_SEPOLIA_PRIVATE_KEY) gives the poster its gas.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=$PWD
[ $# -ge 1 ] || { echo "usage: $0 <export dir> [state dir]" >&2; exit 2; }
export_dir=$1
state=${2:-$HOME/.cache/onyx-nl4/track-nws}
RPC=${ONYX_TRACK_RPC:-https://sepolia.base.org}
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
KEYS=$REPO/.onyx-testnet/keys
[ -s "$export_dir/deployment.json" ] || { echo "missing $export_dir/deployment.json" >&2; exit 1; }
mkdir -p "$state/forecasts"
chmod 700 "$state"
work=$(mktemp -d "$HOME/.cache/onyx-nl4-cast.XXXXXX")
trap 'rm -rf "$work"' EXIT
castw() { (cd "$work" && ETH_RPC_URL=$RPC "$FOUNDRY_BIN/cast" "$@"); }
[ "$(castw chain-id)" = 84532 ] || { echo "not Base Sepolia (chain 84532): refused" >&2; exit 1; }
REG=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["registry"])' "$export_dir/deployment.json")
SETTLEMENT=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["settlement"])' "$export_dir/deployment.json")
COORD=$(castw call "$SETTLEMENT" "coordinator()(address)")

newkey() {
  [ -e "$1" ] && return 0
  python3 -I - "$1" <<'PY'
import os, secrets, sys
fd = os.open(sys.argv[1], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
os.write(fd, secrets.token_hex(32).encode()); os.close(fd)
PY
}
newkey "$KEYS/track-nws-relay.key"
newkey "$KEYS/track-nws-poster.key"

no_post=()
if [ "${ONYX_TRACK_NO_POST:-0}" = 1 ]; then
  no_post=(--no-post)
  forecast=$(ls -1 "$state"/forecasts/*.json 2>/dev/null | tail -1)
  [ -n "$forecast" ] || { echo "no forecast fetched yet" >&2; exit 1; }
else
  forecast=$state/forecasts/$(date -u +%Y%m%dT%H%M%SZ).json
  curl -fsS -m 30 -A onyx-dev -H 'Accept: application/geo+json' -o "$forecast" \
    https://api.weather.gov/gridpoints/BOU/75,66
fi

key=$(python3 -I - "$REPO/.env" <<'PY'
import sys
for line in open(sys.argv[1]).read().splitlines():
    k, sep, v = line.partition("=")
    if sep and k.strip() == "BASE_SEPOLIA_PRIVATE_KEY":
        print(v.strip().strip('"').strip("'"))
        break
PY
)
[ -n "$key" ] || { echo "BASE_SEPOLIA_PRIVATE_KEY is missing from .env" >&2; exit 1; }

CARGO_INCREMENTAL=0 cargo build -q -p onyx-source-ncei --example track-nws
bin=$work/track-nws
cp "${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/track-nws" "$bin"
mkdir -p "$export_dir/track"
TMPDIR=$work ONYX_TRACK_RELAY_KEY_FILE=$KEYS/track-nws-relay.key \
  ONYX_TRACK_POSTER_KEY_FILE=$KEYS/track-nws-poster.key ONYX_TRACK_FUNDER_KEY=$key \
  "$bin" --forecast "$forecast" --state "$state" --out "$export_dir/track/publication.borsh" \
  --rpc "$RPC" --registry "$REG" --coordinator "$COORD" --cast "$FOUNDRY_BIN/cast" "${no_post[@]}"
