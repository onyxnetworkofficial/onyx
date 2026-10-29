#!/usr/bin/env bash
# Fetch Bitcoin mainnet blocks from a node over ssh, read-only (getblockhash, getblock).
# Usage: scripts/fetch-btc-node.sh <ssh-host> <from-height> <to-height> <out-dir>
# Writes <height>.raw.hex (getblock 0: the exact block bytes) and <height>.v3.json.gz
# (getblock 3: adds each input's prevout script and value). The Rust adapter
# (onyx_source_btc::node) re-checks everything it can from the raw bytes.
set -euo pipefail
host=$1; from=$2; to=$3; out=$4
mkdir -p "$out"
for ((h = from; h <= to; h++)); do
  hash=$(ssh -o BatchMode=yes "$host" "bitcoin-cli getblockhash $h")
  ssh -o BatchMode=yes "$host" "bitcoin-cli getblock $hash 0" > "$out/$h.raw.hex"
  ssh -o BatchMode=yes "$host" "bitcoin-cli getblock $hash 3 | gzip -c" > "$out/$h.v3.json.gz"
  echo "$h $hash $(wc -c < "$out/$h.raw.hex") hex chars"
done
