#!/usr/bin/env bash
# Deploy the query API (onyx-node, testnet) to the Onyx web server next to the site (NS17,
# docs/ops/site-hosting.md "API"). Caddy proxies https://<domain>/v1/... to it.
#
# Usage: scripts/deploy-api.sh <export dir> <onyx-node binary>
#   <export dir>  the --export-node directory of a testnet run
#                 (scripts/demo-testnet.sh testnet --yes --export-node DIR)
#   <binary>      a release onyx-node built with RUSTFLAGS="--remap-path-prefix=$HOME=/build"
#                 (no home folder path in it; refused otherwise)
#
# Level-2 stores in <export>/observations (ROADMAP B6) go to /etc/onyx/observations with their
# pins (ONYX_OBSERVATIONS, ONYX_OBSERVATION_TEMPLATES).
# A track-record publication in <export>/track/publication.borsh (north-star loop NL4) goes to
# /etc/onyx/track (ONYX_TRACK_PUBLICATIONS).
#
# Order (review NS17-R1): first Caddy, so its admin endpoint is off before the node runs
# (deploy/caddy/onyx-web.Caddyfile, validated, the old file kept as a dated backup and put
# back if Caddy does not restart); then the binary, /etc/onyx/{node.env,deployment.json,
# catalog.json}, the hardened unit deploy/systemd/onyx-node.service and the row batches; the
# service restarts, and the script waits until /v1/health shows at least one servable shard
# on the server and answers over HTTPS.
#
# Payments are OFF unless ONYX_API_PAYMENTS=1: with ONYX_RIGHTS=fallback every current shard
# is label-derived and answers 403 before any 402, so a spendable key on a public host would
# guard nothing (review NS17-R1 finding 3). ONYX_API_PAYMENTS=1 keeps the export's mode and
# installs the testnet operator key from .onyx-testnet/keys as a systemd credential.
# The node URL is the public https://sepolia.base.org (no API key on the server).
#
# Also (ROADMAP S5, north-star loop NL5 and NL21), each on unless its variable is 0:
#   ONYX_API_INTAKE    proposals: ONYX_SUBMISSIONS=/var/lib/onyx/state/submissions.journal (the
#                      node's per-client and global limits; a journal the operator reviews with
#                      submit-review while the node is stopped, docs/ops/submission-review.md)
#   ONYX_API_MCP       the agent connector at https://<domain>/mcp: apps/agent in /opt/onyx-mcp
#                      run by deploy/systemd/onyx-mcp.service (proposals on; skipped with a note when
#                      the host has no Node 20 or later)
#   ONYX_API_CONN_CAP  deploy/nftables/onyx-conn-cap.nft (32 connections per address on 443),
#                      checked with nft -c first, loaded by deploy/systemd/onyx-conn-cap.service
# Env: ONYX_SITE_HOST (onyx-web), ONYX_SITE_DOMAIN (onyx.cryptagraph.com).
set -euo pipefail
cd "$(dirname "$0")/.."
[ $# = 2 ] || { echo "usage: $0 <export dir> <onyx-node binary>" >&2; exit 2; }
export_dir=$1
bin=$2
host=${ONYX_SITE_HOST:-onyx-web}
domain=${ONYX_SITE_DOMAIN:-onyx.cryptagraph.com}
for f in node.service.env deployment.json catalog.json; do
  [ -s "$export_dir/$f" ] || { echo "missing $export_dir/$f" >&2; exit 1; }
done
[ -d "$export_dir/cas" ] || { echo "missing $export_dir/cas" >&2; exit 1; }
[ -x "$bin" ] || { echo "not an executable: $bin" >&2; exit 1; }
# Nothing that names the builder's machine goes to the server.
! grep -qaF "$HOME" "$bin" || { echo "$bin holds the home folder path: rebuild with --remap-path-prefix" >&2; exit 1; }
grep -qx 'ONYX_ENV=testnet' "$export_dir/node.service.env" || { echo "not a testnet export" >&2; exit 1; }
grep -qx 'ONYX_RIGHTS=fallback' "$export_dir/node.service.env" || { echo "the export must serve no label-derived data (ONYX_RIGHTS=fallback)" >&2; exit 1; }
if [ "${ONYX_API_PAYMENTS:-0}" = 1 ]; then
  payments=$(sed -n 's/^ONYX_PAYMENTS=//p' "$export_dir/node.service.env")
else
  payments=off
fi

stage=$(mktemp -d "${HOME}/.cache/onyx-deploy-api.XXXXXX")
trap 'rm -rf "$stage"' EXIT
# The export's settings, with this host's public URL, node URL, proxy, health and payment settings.
grep -vE '^(#|ONYX_PUBLIC_URL=|ONYX_RPC_URL=|ONYX_LISTEN=|ONYX_TRUSTED_PROXIES=|ONYX_INDEX_HEALTH=|ONYX_PAYMENTS=|ONYX_OBSERVATIONS=|ONYX_OBSERVATION_TEMPLATES=|ONYX_TRACK_PUBLICATIONS=)' \
  "$export_dir/node.service.env" > "$stage/node.env"
cat >> "$stage/node.env" <<EOF
ONYX_LISTEN=127.0.0.1:8080
ONYX_PUBLIC_URL=https://$domain
ONYX_RPC_URL=https://sepolia.base.org
ONYX_TRUSTED_PROXIES=127.0.0.1
ONYX_INDEX_HEALTH=summary
ONYX_PAYMENTS=$payments
EOF
intake=${ONYX_API_INTAKE:-1}
mcp=${ONYX_API_MCP:-1}
conn_cap=${ONYX_API_CONN_CAP:-1}
[ "$intake" = 0 ] || echo "ONYX_SUBMISSIONS=/var/lib/onyx/state/submissions.journal" >>"$stage/node.env"
# Level-2 results stores (ROADMAP B6, scripts/level2-sepolia.sh): <export>/observations/*.json,
# pinned by <export>/observations/pins (metric=0x<template id>, one per line); the node checks
# each store against its pin and the registry on chain before it serves it.
obs=()
for f in "$export_dir"/observations/*.json; do [ -e "$f" ] && obs+=("$f"); done
if [ ${#obs[@]} -gt 0 ]; then
  [ -s "$export_dir/observations/pins" ] || { echo "missing $export_dir/observations/pins" >&2; exit 1; }
  # THREATS NS5-8, ADR-0048 amendment 1: level-2 values of free public sources are free to read
  # (their on-chain result hash is unsalted). Refuse an export whose catalog prices them.
  python3 -I - "$export_dir/catalog.json" "$export_dir/observations/pins" <<'PRICE' || exit 1
import json, sys
cat = json.load(open(sys.argv[1]))
metrics = [l.split("=", 1)[0].strip() for l in open(sys.argv[2]) if "=" in l]
priced = [m for m in metrics if m in (cat.get("pricing") or {})]
if cat.get("defaultPricing") or priced:
    sys.exit(f"the export prices level-2 values of free public sources ({priced or 'defaultPricing'}): they are free to read (THREATS NS5-8)")
PRICE
  mkdir "$stage/observations"
  cp "${obs[@]}" "$stage/observations/"
  echo "ONYX_OBSERVATIONS=$(for f in "${obs[@]}"; do echo "/etc/onyx/observations/$(basename "$f")"; done | paste -sd,)" >>"$stage/node.env"
  echo "ONYX_OBSERVATION_TEMPLATES=$(paste -sd, "$export_dir/observations/pins")" >>"$stage/node.env"
fi
# The track-record publication (north-star loop NL4, scripts/track-nws-sepolia.sh): served on
# /v1/track; the node refuses a file that does not start with what it served before.
if [ -s "$export_dir/track/publication.borsh" ]; then
  mkdir "$stage/track"
  cp "$export_dir/track/publication.borsh" "$stage/track/"
  echo "ONYX_TRACK_PUBLICATIONS=/etc/onyx/track/publication.borsh" >>"$stage/node.env"
fi
chmod 600 "$stage/node.env"
cp deploy/systemd/onyx-node.service "$stage/onyx-node.service"
if [ "$mcp" != 0 ]; then
  # The toolkit runs alone (its own OpenAPI and dataset copies, no npm dependencies).
  node apps/agent/scripts/sync.mjs --check >/dev/null || { echo "apps/agent copies are stale: run node apps/agent/scripts/sync.mjs" >&2; exit 1; }
  mkdir "$stage/mcp"
  cp -r apps/agent/src apps/agent/package.json apps/agent/openapi.json apps/agent/catalog.json "$stage/mcp/"
  cp deploy/systemd/onyx-mcp.service "$stage/"
fi
if [ "$conn_cap" != 0 ]; then
  cp deploy/nftables/onyx-conn-cap.nft deploy/systemd/onyx-conn-cap.service "$stage/"
fi
if [ "$payments" = off ]; then
  # No operator key: the unit's two key lines go (deploy/systemd/onyx-node.service).
  sed -i -e '/^LoadCredential=operator-key:/d' -e '/^Environment=ONYX_OPERATOR_KEY_FILE=/d' "$stage/onyx-node.service"
fi

remote=onyx-api-stage
ssh "$host" "rm -rf ~/$remote && install -d -m 700 ~/$remote"
scp -q "$bin" "$stage/node.env" "$stage/onyx-node.service" "$export_dir/deployment.json" \
  "$export_dir/catalog.json" deploy/caddy/onyx-web.Caddyfile "$host:$remote/"
[ "$payments" = off ] || scp -q .onyx-testnet/keys/operator.key "$host:$remote/operator.key"
rsync -a --delete "$export_dir/cas/" "$host:$remote/cas/"
[ ${#obs[@]} = 0 ] || rsync -a "$stage/observations/" "$host:$remote/observations/"
[ ! -d "$stage/track" ] || rsync -a "$stage/track/" "$host:$remote/track/"
[ "$mcp" = 0 ] || { rsync -a "$stage/mcp/" "$host:$remote/mcp/" && scp -q "$stage/onyx-mcp.service" "$host:$remote/"; }
[ "$conn_cap" = 0 ] || scp -q "$stage/onyx-conn-cap.nft" "$stage/onyx-conn-cap.service" "$host:$remote/"
ssh "$host" "set -euo pipefail
  trap 'rm -rf ~/$remote' EXIT
  cd ~/$remote
  # 1. Caddy first: its admin endpoint must be off before the node runs (NS17-R1 finding 2).
  if ! sudo cmp -s onyx-web.Caddyfile /etc/caddy/Caddyfile; then
    caddy validate --config onyx-web.Caddyfile --adapter caddyfile 2>&1 | grep -v '^{' || true
    caddy validate --config onyx-web.Caddyfile --adapter caddyfile >/dev/null 2>&1
    bak=/etc/caddy/Caddyfile.bak-\$(date -u +%Y%m%dT%H%M%SZ)
    sudo cp -p /etc/caddy/Caddyfile \"\$bak\"
    sudo install -m 0644 onyx-web.Caddyfile /etc/caddy/Caddyfile
    if ! sudo systemctl restart caddy; then
      echo 'caddy did not restart: putting the previous file back' >&2
      sudo cp -p \"\$bak\" /etc/caddy/Caddyfile
      sudo systemctl restart caddy
      exit 1
    fi
  fi
  if curl -s -m 3 -o /dev/null http://127.0.0.1:2019/config/; then
    echo 'Caddy admin endpoint answers on 127.0.0.1:2019: not starting the node' >&2
    exit 1
  fi
  # 2. The node. First keep the running release for a rollback (THREATS NL20-4): one copy of
  # the binary, the unit and /etc/onyx without the operator key, replaced at every deploy.
  if [ -x /usr/local/bin/onyx-node ]; then
    sudo rm -rf /var/backups/onyx-prev
    sudo install -d -m 0700 /var/backups/onyx-prev
    sudo cp -p /usr/local/bin/onyx-node /var/backups/onyx-prev/
    sudo cp -p /etc/systemd/system/onyx-node.service /var/backups/onyx-prev/ 2>/dev/null || true
    sudo rsync -a --exclude operator.key /etc/onyx/ /var/backups/onyx-prev/etc-onyx/
  fi
  sudo install -m 0755 onyx-node /usr/local/bin/onyx-node
  sudo install -d -m 0755 /etc/onyx
  sudo install -m 0600 node.env /etc/onyx/node.env
  sudo install -m 0644 deployment.json catalog.json /etc/onyx/
  sudo rm -rf /etc/onyx/observations
  if [ -d observations ]; then
    sudo install -d -m 0755 /etc/onyx/observations
    sudo install -m 0644 observations/*.json /etc/onyx/observations/
  fi
  sudo rm -rf /etc/onyx/track
  if [ -d track ]; then
    sudo install -d -m 0755 /etc/onyx/track
    sudo install -m 0644 track/publication.borsh /etc/onyx/track/
  fi
  if [ -f operator.key ]; then
    sudo install -m 0600 operator.key /etc/onyx/operator.key
    rm -f operator.key
  else
    sudo rm -f /etc/onyx/operator.key
  fi
  sudo install -m 0644 onyx-node.service /etc/systemd/system/onyx-node.service
  sudo systemctl daemon-reload
  sudo systemctl stop onyx-node 2>/dev/null || true
  sudo systemctl start onyx-node
  sudo rsync -a --delete cas/ /var/lib/private/onyx/cas/
  sudo chown -R --reference=/var/lib/private/onyx/cas /var/lib/private/onyx/cas
  sudo systemctl enable onyx-node >/dev/null 2>&1
  # 3. Serving: the indexer admits the held shards once final and their rows hash to the root.
  for i in \$(seq 120); do
    n=\$(curl -fsS -m 5 http://127.0.0.1:8080/v1/health 2>/dev/null \
      | python3 -I -c 'import json,sys; print(json.load(sys.stdin).get(\"indexer\",{}).get(\"servable\",0))' 2>/dev/null || echo 0)
    [ \"\$n\" -ge 1 ] && break
    sleep 2
  done
  [ \"\$n\" -ge 1 ] || { echo 'the node serves no shard after 240 s (journalctl -u onyx-node)' >&2; exit 1; }
  echo \"servable shards: \$n\"
  # 4. The connection cap on 443 (THREATS NS17-4, TLS-2): checked before it is loaded.
  if [ -f onyx-conn-cap.nft ]; then
    sudo nft -c -f onyx-conn-cap.nft
    sudo install -m 0644 onyx-conn-cap.nft /etc/onyx/onyx-conn-cap.nft
    sudo install -m 0644 onyx-conn-cap.service /etc/systemd/system/onyx-conn-cap.service
    sudo systemctl daemon-reload
    sudo systemctl enable onyx-conn-cap >/dev/null 2>&1
    sudo systemctl restart onyx-conn-cap
    sudo nft list table inet onyx_conn_cap >/dev/null
    echo 'connection cap: loaded'
  fi
  # 5. The agent connector (NL21), when the host has Node 20 or later.
  if [ -d mcp ]; then
    major=\$(node -p 'process.versions.node.split(\".\")[0]' 2>/dev/null || echo 0)
    if [ \"\$major\" -ge 20 ]; then
      sudo rm -rf /opt/onyx-mcp
      sudo install -d -m 0755 /opt/onyx-mcp
      sudo cp -r mcp/. /opt/onyx-mcp/
      sudo chmod -R a+rX,go-w /opt/onyx-mcp
      sudo install -m 0644 onyx-mcp.service /etc/systemd/system/onyx-mcp.service
      sudo systemctl daemon-reload
      sudo systemctl enable onyx-mcp >/dev/null 2>&1
      sudo systemctl restart onyx-mcp
      for i in \$(seq 20); do
        c=\$(curl -s -m 5 -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' \
          -H 'accept: application/json, text/event-stream' \
          --data '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"onyx-dev\",\"version\":\"0\"}}}' \
          http://127.0.0.1:8090/mcp || echo 000)
        [ \"\$c\" = 200 ] && break
        sleep 1
      done
      [ \"\$c\" = 200 ] || { echo 'the MCP connector does not answer on 127.0.0.1:8090 (journalctl -u onyx-mcp)' >&2; exit 1; }
      echo 'mcp: answering'
    else
      echo 'mcp: skipped, no Node 20 or later on the host (install nodejs, then deploy again)'
    fi
  fi"
code=$(curl -sS -m 20 -o /dev/null -w '%{http_code}' -A onyx-dev "https://$domain/v1/health") || code=000
[ "$code" = 200 ] || { echo "https://$domain/v1/health answered $code" >&2; exit 1; }
code=$(curl -sS -m 20 -o /dev/null -w '%{http_code}' -A onyx-dev "https://$domain/") || code=000
[ "$code" = 200 ] || { echo "https://$domain/ answered $code" >&2; exit 1; }
if [ "$mcp" != 0 ]; then
  code=$(curl -sS -m 20 -o /dev/null -w '%{http_code}' -A onyx-dev -X POST -H 'content-type: application/json' \
    -H 'accept: application/json, text/event-stream' \
    --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"onyx-dev","version":"0"}}}' \
    "https://$domain/mcp") || code=000
  echo "https://$domain/mcp answered $code (200 when the connector runs, 502 when the host has no Node)"
fi
echo "API DEPLOYED: https://$domain/v1/health (payments $payments, intake $([ "$intake" = 0 ] && echo off || echo on))"
