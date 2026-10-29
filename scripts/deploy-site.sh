#!/usr/bin/env bash
# Deploy the public-safe website to the Onyx web server (pre-public loop PP13, docs/ops/site-hosting.md).
#
# Usage: scripts/deploy-site.sh [--skip-build]
# Builds the site with ONYX_PUBLIC=1 ONYX_RIGHTS=fallback (no label rows, no label-derived totals;
# build-web.sh runs the public guards and the e2e), refuses a build that is not a fallback build,
# copies it to a new release folder on the server (ssh alias ONYX_SITE_HOST, default onyx-web),
# switches /srv/onyx/current to it in one step, keeps the last 5 releases, and checks the live
# page over HTTPS. Rolling back is pointing `current` at an earlier release. ONYX_SITE_SUBMIT=0 keeps the
# Add data page offline (it downloads proposals instead of sending them to the hosted node).
set -euo pipefail
cd "$(dirname "$0")/.."
host=${ONYX_SITE_HOST:-onyx-web}
domain=${ONYX_SITE_DOMAIN:-onyx.cryptagraph.com}
[ "${1:-}" = --skip-build ] || ONYX_PUBLIC=1 ONYX_RIGHTS=fallback scripts/build-web.sh >/dev/null
python3 -I - apps/web/build/data/demo.json <<'PY' || { echo "apps/web/build is not a public fallback build" >&2; exit 1; }
import json, sys
d = json.load(open(sys.argv[1]))
sys.exit(0 if d.get("rights") == "fallback" and d.get("public") is True else 1)
PY
# The owner's choice (2026-10-25): no personal or partner names on the public site.
names=$(grep -rIlwE 'the project owner|A partner|A partner' apps/web/build --exclude='*.wasm' || true)
[ -z "$names" ] || { echo "names on the site, not deployed: $names" >&2; exit 1; }
# Nor the builder's home folder path, binaries included (the verifier once carried it).
home_hits=$(grep -rlaF "$HOME" apps/web/build || true)
[ -z "$home_hits" ] || { echo "home folder path on the site, not deployed: $home_hits" >&2; exit 1; }
rel="$(date -u +%Y%m%dT%H%M%SZ)-$(git rev-parse --short HEAD)"
dirty=$(git status --porcelain | wc -l | tr -d ' ')
[ "$dirty" = 0 ] || rel="$rel-dirty"
ssh "$host" "install -d -m 755 /srv/onyx/releases/$rel"
rsync -a --delete --chmod=D755,F644 apps/web/build/ "$host:/srv/onyx/releases/$rel/"
# Meeting polish MP2: the hosted node takes proposals (scripts/deploy-api.sh, ONYX_API_INTAKE on by
# default), so this release's Add data page sends them there (src/lib/submit-api.svelte.ts reads it).
# The local build keeps its {"submit": false}; ONYX_SITE_SUBMIT=0 deploys the offline page.
if [ "${ONYX_SITE_SUBMIT:-1}" = 1 ]; then
  ssh "$host" "printf '%s\n' '{ \"submit\": true }' > /srv/onyx/releases/$rel/intake.json && chmod 644 /srv/onyx/releases/$rel/intake.json"
fi
ssh "$host" "ln -sfn /srv/onyx/releases/$rel /srv/onyx/current.new && mv -T /srv/onyx/current.new /srv/onyx/current \
  && cd /srv/onyx/releases && ls -1dt */ | tail -n +6 | grep -v '^placeholder/' | xargs -r rm -rf"
ip=$(dig +short "$domain" @1.1.1.1 | tail -1)
code=$(curl -sS -m 20 -o /dev/null -w '%{http_code}' --resolve "$domain:443:$ip" "https://$domain/") || code=000
[ "$code" = 200 ] || { echo "https://$domain/ answered $code" >&2; exit 1; }
echo "SITE DEPLOYED: https://$domain/ ($rel)"
