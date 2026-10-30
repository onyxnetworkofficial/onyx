#!/usr/bin/env bash
# The join kit's programs (north-star loop NL16, docs/join.md "Get the programs"): builds the
# level-2 observer of each dated source and the manifest observer (with the reviewed manifests)
# in release mode and puts them, Foundry's `cast` and `join.sh` in one folder an outside operator copies to the machine that observes.
#
# Usage: scripts/join-build.sh <kit dir> [fiscaldata|usgs|ncei ...]   (default: all three)
# Needs: Rust (rust-toolchain.toml), Foundry's cast (FOUNDRY_BIN, default ~/.foundry/bin).
# The binaries are built with the builder's home folder remapped (refused if it still shows):
# nothing in the kit names the machine that built it, and nothing in it is a key or a secret.
set -euo pipefail
cd "$(dirname "$0")/.."
[ $# -ge 1 ] || { echo "usage: $0 <kit dir> [fiscaldata|usgs|ncei ...]" >&2; exit 2; }
kit=$1
shift
srcs=("$@")
[ ${#srcs[@]} -gt 0 ] || srcs=(fiscaldata usgs ncei)
FOUNDRY_BIN=${FOUNDRY_BIN:-$HOME/.foundry/bin}
target=${CARGO_TARGET_DIR:-$PWD/target}
mkdir -p "$kit/bin"
for s in "${srcs[@]}"; do
  case "$s" in fiscaldata | usgs | ncei) ;; *) echo "unknown source $s" >&2; exit 2 ;; esac
  RUSTFLAGS="--remap-path-prefix=$HOME=/build" CARGO_INCREMENTAL=0 \
    cargo build -q --release -p "onyx-source-$s" --example level2-observer
  # Copied at once: every source's example has the same name in the target folder.
  cp "$target/release/examples/level2-observer" "$kit/bin/level2-observer-$s"
done
# Manifest sources (contributor loop CL3): one observer for every reviewed manifest, with the
# manifests and the terms table it checks them against (docs/ops/data-sources.md).
RUSTFLAGS="--remap-path-prefix=$HOME=/build" CARGO_INCREMENTAL=0 \
  cargo build -q --release -p onyx-level2 --example level2-manifest
cp "$target/release/examples/level2-manifest" "$kit/bin/level2-manifest"
mkdir -p "$kit/docs/ops"
cp -r sources "$kit/sources"
cp docs/ops/data-sources.md "$kit/docs/ops/data-sources.md"
cp -L "$FOUNDRY_BIN/cast" "$kit/bin/cast"
cp scripts/join.sh "$kit/join.sh"
chmod 755 "$kit/join.sh" "$kit/bin/"*
for b in "$kit/bin/level2-observer-"* "$kit/bin/level2-manifest"; do
  ! grep -qaF "$HOME" "$b" || { echo "$b holds the builder's home folder path: refused" >&2; exit 1; }
done
echo "join kit in $kit: $(cd "$kit/bin" && ls | paste -sd' ')"
