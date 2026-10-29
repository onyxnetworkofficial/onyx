#!/usr/bin/env bash
# A deployable preview bundle (PRODUCT-PLAN.md U5, pre-public loop PP4, docs/ops/local-handoff.md):
# the public-safe website (ONYX_PUBLIC=1 ONYX_RIGHTS=fallback: no label rows, no label-derived
# totals), the API description, the Caddy and systemd units, an environment template with
# placeholders only, a README and SHA256SUMS, in a new folder OUT. It never uploads, deploys or
# publishes anything; a person copies OUT to a host they chose.
#
# Usage: scripts/preview-bundle.sh OUT [--with-node] [--skip-build]
#   --with-node   also build onyx-node in release mode and add the binary (about 5 minutes)
#   --skip-build  reuse an existing public fallback build in apps/web/build (its demo.json must say
#                 rights "fallback"); otherwise the site is rebuilt
# Checks in OUT, all required: no private file kinds (.env, keys, journals, stores, state); the
# site is a fallback build; gitleaks over OUT (redacted; ONYX_GITLEAKS, default `gitleaks`); no
# e-mail address outside the reviewed placeholder kinds. Prints PREVIEW BUNDLE READY on success.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD
out=${1:-}
[ -n "$out" ] || { echo "usage: scripts/preview-bundle.sh OUT [--with-node] [--skip-build]" >&2; exit 2; }
shift
with_node=0 skip_build=0
for a in "$@"; do
  case "$a" in
    --with-node) with_node=1 ;;
    --skip-build) skip_build=1 ;;
    *) echo "unknown option $a" >&2; exit 2 ;;
  esac
done
[ -e "$out" ] && { echo "$out exists; give a new folder" >&2; exit 2; }
fail() { echo "preview bundle: $*" >&2; exit 1; }

if [ "$skip_build" = 0 ]; then
  ONYX_PUBLIC=1 ONYX_RIGHTS=fallback scripts/build-web.sh >/dev/null
fi
python3 -I - apps/web/build/data/demo.json <<'PY' || fail "apps/web/build is not a public fallback build (build with ONYX_PUBLIC=1 ONYX_RIGHTS=fallback)"
import json, sys
d = json.load(open(sys.argv[1]))
sys.exit(0 if d.get("rights") == "fallback" and d.get("public") is True else 1)
PY

mkdir -p "$out"
cp -r apps/web/build "$out/site"
mkdir -p "$out/api" "$out/deploy"
cp docs/api/openapi.json "$out/api/openapi.json"
cp -r deploy/caddy deploy/systemd "$out/deploy/"
# The environment a node needs, with placeholders only (docs/ops/config.md has every variable).
cat >"$out/deploy/node.env.example" <<'ENV'
# onyx-node settings (docs/ops/config.md). Copy to /etc/onyx/node.env, mode 0600, and fill in.
# Testnet only: Base Sepolia (84532). Never a mainnet chain or real funds.
ONYX_ENV=testnet
ONYX_RIGHTS=fallback
ONYX_PAYMENTS=off
ONYX_LISTEN=127.0.0.1:8080
ONYX_PUBLIC_URL=https://preview.domain.tld/
# A Base Sepolia RPC URL; it often carries a provider key: prefer ONYX_RPC_URL_FILE.
ONYX_RPC_URL_FILE=/etc/onyx/rpc-url
ONYX_DEPLOYMENT=/etc/onyx/deployment.json
ONYX_CATALOG=/etc/onyx/catalog.json
ONYX_CAS_DIR=/var/lib/onyx/cas
ONYX_STATE_DIR=/var/lib/onyx/state
# Optional: data proposals (a private, owner-only directory; docs/ops/submission-review.md).
#ONYX_SUBMISSIONS=/var/lib/onyx/private-intake/submissions.journal
ENV
if [ "$with_node" = 1 ]; then
  cargo build --release --locked -q -p onyx-node --bin onyx-node
  mkdir -p "$out/bin"
  cp "${CARGO_TARGET_DIR:-$root/target}/release/onyx-node" "$out/bin/onyx-node"
fi
commit=$(git rev-parse --short HEAD)
dirty=$(git status --porcelain | wc -l | tr -d ' ')
cat >"$out/README.md" <<EOF
# Onyx preview bundle

Built from commit \`$commit\`$( [ "$dirty" = 0 ] || echo " with $dirty uncommitted change(s)" ) by \`scripts/preview-bundle.sh\`.
Testnet only. Nothing here is deployed: a person copies this folder to a host they chose.

- \`site/\`: the public-safe website (no label rows, no label-derived totals). Serve it as static
  files; it works offline. The connected "Add data" form needs a build with
  \`VITE_ONYX_SUBMIT_API=1\` and \`/v1/submit\` on the same origin (docs/ops/submission-review.md).
- \`api/openapi.json\`: every route of the node (OpenAPI 3.1; also served at \`/v1/openapi.json\`).
- \`deploy/caddy/Caddyfile\`, \`deploy/systemd/\`: the TLS front and the service units
  (docs/ops/deployment.md).
- \`deploy/node.env.example\`: the node's settings with placeholders; fill in on the host.
$( [ "$with_node" = 1 ] && echo "- \`bin/onyx-node\`: the node, release build for this machine's platform." )
- \`SHA256SUMS\`: check with \`sha256sum -c SHA256SUMS\`.

The local end-to-end rehearsal of this flow is docs/ops/local-handoff.md in the repository.
EOF

# Checks.
bad=$(find "$out" \( -name '.env' -o -name '.env.*' -o -name '*.key' -o -name '*.pem' -o -name '*.journal' \
  -o -name '*.feed.json' -o -name 'operator-key' -o -name 'rpc-url' -o -path '*/state/*' \) -print | grep -v 'node.env.example$' || true)
[ -z "$bad" ] || fail "private file kinds in the bundle: $bad"
gl="$(command -v "${ONYX_GITLEAKS:-gitleaks}" || true)"
[ -n "$gl" ] || fail "gitleaks missing: secret scan not run"
(cd "$out" && "$gl" dir . --config "$root/.gitleaks.toml" --redact --no-banner --exit-code 1 \
  --report-format json --report-path "$out.gitleaks.json" >/dev/null 2>&1) \
  || fail "gitleaks findings (redacted report: $out.gitleaks.json)"
rm -f "$out.gitleaks.json"
allow='@([A-Za-z0-9-]+\.)*(example|test|invalid)(\.[A-Za-z]+)?\b|@domain\.tld\b|git@(github\.com|gitlab\.com|bitbucket\.org)|@[0-9]x\.png|noreply@|@users\.noreply\.github\.com|\ba@b\.co\b|\bx@y\.io\b|user@api\.fiscaldata\.treasury\.gov|\blegal@chain\.link\b'
hits=$(grep -rInEo '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' "$out" --exclude='*.wasm' --exclude='onyx-node' \
  | grep -vE "$allow" | cut -d: -f1,2 | sort -u || true)
[ -z "$hits" ] || fail "e-mail addresses outside the placeholder list: $hits"
(cd "$out" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS)
echo "files: $(find "$out" -type f | wc -l), size: $(du -sh "$out" | cut -f1)"
echo "PREVIEW BUNDLE READY: $out (nothing was uploaded or deployed)"
