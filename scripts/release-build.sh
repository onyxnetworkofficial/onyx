#!/usr/bin/env bash
# The test-network release's downloads (contributor loop, release v0.2.0-testnet and later): what
# the Add data page links to on the GitHub release. Built from this checkout, Linux x86-64, with the
# builder's home folder remapped.
#
# Usage: scripts/release-build.sh <out dir> <version>      (version like v0.2.0-testnet)
#   onyx-join-linux-x86_64.tar.gz            the observer kit (scripts/join-build.sh): the level-2
#                                            observers, the manifest observer with the reviewed
#                                            manifests, Foundry's cast and join.sh (docs/join.md)
#   onyx-contrib-<version>.tar.gz and .zip   the contributor client (apps/contrib with the apps/agent
#                                            files it uses; Node 20, no dependencies, any system)
#   onyx-contrib-keyed-linux-x86_64.tar.gz   level2-keyed, for keyed rounds (apps/contrib README 7)
#   onyx-publish-linux-x86_64.tar.gz         onyx-publish and onyx-check, for large datasets
#                                            (docs/ops/bulk-publish.md)
#   SHA256SUMS
# Archives: owner 0/0, mtime 0, sorted names. Refused when a file is a key or holds the builder's
# home folder path, or, with ONYX_RELEASE_DENY=<file of words, one per line>, any of those words.
# Needs: Rust (rust-toolchain.toml), Foundry's cast, python3, tar, sha256sum.
set -euo pipefail
cd "$(dirname "$0")/.."
[ $# = 2 ] || { echo "usage: $0 <out dir> <version, like v0.2.0-testnet>" >&2; exit 2; }
out=$1
ver=$2
[[ $ver =~ ^v[0-9]+\.[0-9]+\.[0-9]+-testnet$ ]] || { echo "version must look like v0.2.0-testnet" >&2; exit 2; }
[ ! -e "$out" ] || { echo "$out exists: give a new folder" >&2; exit 1; }
mkdir -p "$out" "$HOME/.cache"
out=$(cd "$out" && pwd)
stage=$(mktemp -d "$HOME/.cache/onyx-release.XXXXXX")
trap 'rm -rf "$stage"' EXIT
target=${CARGO_TARGET_DIR:-$PWD/target}
export RUSTFLAGS="--remap-path-prefix=$HOME=/build" CARGO_INCREMENTAL=0
pack() { tar --sort=name --owner=0 --group=0 --numeric-owner --mtime=@0 -C "$stage" -czf "$out/$2" "$1"; }

# 1. The observer kit.
scripts/join-build.sh "$stage/onyx-join" >/dev/null
cp LICENSE NOTICE "$stage/onyx-join/"

# 2. The publisher's tools.
cargo build -q --locked --release -p onyx-publish -p onyx-check
mkdir -p "$stage/onyx-publish/bin"
cp "$target/release/onyx-publish" "$target/release/onyx-check" "$stage/onyx-publish/bin/"
cp docs/ops/bulk-publish.md "$stage/onyx-publish/README.md"
cp LICENSE NOTICE "$stage/onyx-publish/"

# 3. The contributor client: the same relative layout as the repository, so its imports resolve.
c=$stage/onyx-contrib
mkdir -p "$c/apps/contrib" "$c/apps/agent"
cp -r apps/contrib/src apps/contrib/README.md apps/contrib/package.json "$c/apps/contrib/"
cp -r apps/agent/src apps/agent/catalog.json apps/agent/openapi.json "$c/apps/agent/"
cp LICENSE NOTICE "$c/"
cat >"$c/README.txt" <<EOF
onyx-contrib $ver (test network only: Base Sepolia, no real money)

Needs Node.js 20 or later; no other install. Start:
  node apps/contrib/src/cli.mjs doctor
The full guide is apps/contrib/README.md. Your API keys stay in your computer's keychain.
For keyed rounds, also download onyx-contrib-keyed-linux-x86_64.tar.gz and set
ONYX_CONTRIB_KEYED_BIN to its bin/level2-keyed.
EOF

# 4. The keyed-round tool.
cargo build -q --locked --release -p onyx-level2 --example level2-keyed
mkdir -p "$stage/onyx-contrib-keyed/bin"
cp "$target/release/examples/level2-keyed" "$stage/onyx-contrib-keyed/bin/"
cp LICENSE NOTICE "$stage/onyx-contrib-keyed/"

# Nothing private ships.
bad=$(find "$stage" -type f \( -name '*.key' -o -name '.env' -o -name '*.pem' \) | sed "s|$stage/||")
[ -z "$bad" ] || { echo "key files in the release: $bad" >&2; exit 1; }
! grep -rlaF "$HOME" "$stage" || { echo "the builder's home folder path is in the files above: refused" >&2; exit 1; }
if [ -n "${ONYX_RELEASE_DENY:-}" ]; then
  ! grep -rlaiwF -f "$ONYX_RELEASE_DENY" "$stage" || { echo "a denied word is in the files above: refused" >&2; exit 1; }
fi

pack onyx-join onyx-join-linux-x86_64.tar.gz
pack onyx-publish onyx-publish-linux-x86_64.tar.gz
pack onyx-contrib "onyx-contrib-$ver.tar.gz"
pack onyx-contrib-keyed onyx-contrib-keyed-linux-x86_64.tar.gz
python3 -I - "$stage" "$out/onyx-contrib-$ver.zip" <<'PY'
import os, sys, zipfile
root, dest = sys.argv[1], sys.argv[2]
names = []
for d, _, files in os.walk(os.path.join(root, "onyx-contrib")):
    names += [os.path.join(d, f) for f in files]
with zipfile.ZipFile(dest, "w", zipfile.ZIP_DEFLATED) as z:
    for p in sorted(names):
        info = zipfile.ZipInfo(os.path.relpath(p, root), (1980, 1, 1, 0, 0, 0))
        info.external_attr = (0o644 << 16)
        info.compress_type = zipfile.ZIP_DEFLATED
        z.writestr(info, open(p, "rb").read())
PY
(cd "$out" && sha256sum -- *.tar.gz *.zip >SHA256SUMS)
echo "release $ver in $out:"
(cd "$out" && ls -l --time-style=+ | awk 'NR > 1 {print "  " $NF, $5}')
