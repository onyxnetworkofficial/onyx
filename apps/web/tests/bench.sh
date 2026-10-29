#!/usr/bin/env bash
# Lighthouse (mobile, simulated slow 4G) on every page of the built site,
# served with gzip. Prints scores and core metrics; JSON goes to $OUT.
# Run after `pnpm build`. Uses npx lighthouse@12 and the local Chrome.
# BUILD=<dir> benchmarks another build (default apps/web/build), PORT=<n> the local port,
# PAGES="home run" only those pages.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=${OUT:-/tmp/onyx-bench}
mkdir -p "$OUT"
PORT=${PORT:-4179}
node tests/serve.mjs "$PORT" "${BUILD:-build}" & SRV=$!
trap 'kill $SRV' EXIT
sleep 1
for page in ${PAGES:-home data reserves run explorer labels api}; do
  url="http://127.0.0.1:$PORT/"; [ "$page" = home ] || url="$url#/$page"
  npx -y lighthouse@12 "$url" --chrome-path="${CHROME:-/usr/bin/google-chrome}" \
    --chrome-flags="--headless=new" --only-categories=performance,accessibility,best-practices,seo \
    --output=json --output-path="$OUT/$page.json" --quiet
  python3 - "$OUT/$page.json" "$page" <<'PY'
import json, sys
d = json.load(open(sys.argv[1])); a = d['audits']
s = {k: round(v['score'] * 100) for k, v in d['categories'].items()}
m = {k: a[k]['displayValue'] for k in ['first-contentful-paint', 'largest-contentful-paint', 'total-blocking-time', 'cumulative-layout-shift', 'total-byte-weight']}
print(f"{sys.argv[2]:9} perf {s['performance']:3} a11y {s['accessibility']:3} bp {s['best-practices']:3} seo {s['seo']:3} | FCP {m['first-contentful-paint']} LCP {m['largest-contentful-paint']} TBT {m['total-blocking-time']} CLS {m['cumulative-layout-shift']} | {m['total-byte-weight']}")
PY
done
