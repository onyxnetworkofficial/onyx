#!/usr/bin/env bash
# Test deploy/nftables/onyx-conn-cap.nft without touching this machine's firewall or any server
# (ROADMAP L19). Needs Docker. The test runs in a throwaway container with its own network
# namespace and no network (--network none, NET_ADMIN inside that namespace only), using this
# machine's nft and python3 through a read-only bind mount of / and the host's dynamic loader, so
# nothing is downloaded: the container image is an empty one imported from an empty tar.
# Opt-in (not in check-all, like scripts/docker-smoke.sh).
set -euo pipefail
cd "$(dirname "$0")/.."
image=onyx-empty:conn-cap
docker image inspect "$image" >/dev/null 2>&1 || tar -cf - --files-from /dev/null | docker import - "$image" >/dev/null
lib=/host/lib/x86_64-linux-gnu:/host/usr/lib/x86_64-linux-gnu
py=$(readlink -f "$(command -v python3)")
docker run --rm --name onyx-conn-cap-test --network none --cap-add NET_ADMIN --user 0 \
  -v /:/host:ro -e ONYX_CONN_CAP_IN_CONTAINER=1 -e PYTHONHOME=/host/usr -e PYTHONDONTWRITEBYTECODE=1 \
  --tmpfs /tmp --entrypoint /host/lib64/ld-linux-x86-64.so.2 "$image" \
  --library-path "$lib" "/host$py" -I "/host$PWD/scripts/conn-cap-test.py" "/host$PWD/deploy/nftables/onyx-conn-cap.nft"
