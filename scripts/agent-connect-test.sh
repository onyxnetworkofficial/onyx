#!/usr/bin/env bash
# NL21 agent connect: docs/agents.md's steps, run the way a new user would. A fresh Node container
# gets only apps/agent (read-only; no repository, no Rust, no network), starts the mock node and
# runs apps/agent/test/quickstart.mjs: connect over HTTP and stdio, ask "do you have X?", get
# "not yet", propose (check, then confirm), follow the receipt, refuse a keyed source.
# --live: the read-only steps (find, status) on this machine against the hosted node, with
# User-Agent onyx-dev; no proposal is sent.
# Needs docker and the official node:22-alpine image (pulled once).
set -euo pipefail
cd "$(dirname "$0")/.."
image=node:22-alpine
if [ "${1:-}" = --live ]; then
  ONYX_API_URL="${ONYX_API_URL:-https://onyx.cryptagraph.com}" ONYX_QUICKSTART_LIVE=1 ONYX_USER_AGENT=onyx-dev \
    node apps/agent/test/quickstart.mjs
  exit 0
fi
docker image inspect "$image" >/dev/null 2>&1 || docker pull -q "$image" >/dev/null
docker run --rm --network none --read-only --tmpfs /tmp -v "$PWD/apps/agent:/agent:ro" -w /agent "$image" sh -ec '
  node test/mock-node.mjs >/tmp/mock.url &
  for _ in $(seq 1 50); do [ -s /tmp/mock.url ] && break; sleep 0.1; done
  ONYX_API_URL="$(cat /tmp/mock.url)" node test/quickstart.mjs
'
