#!/usr/bin/env bash
# Reproducible recipe build. Output: recipes/out/<name>.wasm and <name>.wasm.keccak (recipe id).
# CI rebuilds on a clean machine and fails if the bytes differ (ADR-0004, M1 acceptance).
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)
SYSROOT=$(rustc --print sysroot)
CARGO_HOME_DIR=${CARGO_HOME:-$HOME/.cargo}
export CARGO_TARGET_DIR="$ROOT/target/recipes"
export CARGO_PROFILE_RELEASE_STRIP=symbols
export CARGO_PROFILE_RELEASE_DEBUG=false
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
export CARGO_PROFILE_RELEASE_LTO=true
export CARGO_PROFILE_RELEASE_PANIC=abort
export CARGO_PROFILE_RELEASE_OPT_LEVEL=s
export RUSTFLAGS="--remap-path-prefix=$ROOT=/onyx --remap-path-prefix=$CARGO_HOME_DIR=/cargo --remap-path-prefix=$SYSROOT=/rust"
RECIPES=(btc-entity-balance btc-entity-flow-series btc-entity-reserve btc-pool-blocks btc-chain-stats test-float test-clock test-rand)
args=()
# ROADMAP P4a (ADR-0044): the Ethereum chain statistics recipe.
RECIPES+=(eth-chain-stats)
for r in "${RECIPES[@]}"; do args+=(-p "$r"); done
cargo build --locked --release --target wasm32-unknown-unknown "${args[@]}"
# Start from an empty output dir so stale artifacts cannot linger (R-REPRO-2).
rm -rf recipes/out
mkdir -p recipes/out
for r in "${RECIPES[@]}"; do
  f=${r//-/_}.wasm
  cp "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/$f" "recipes/out/$f"
done
# Recipe id = Keccak-256 of the wasm bytes, computed by onyx-core.
cargo run -q --locked -p onyx-source-btc --example recipe-id -- recipes/out/*.wasm
# Exactly one .wasm and one .keccak per listed recipe, nothing else.
want=$(for r in "${RECIPES[@]}"; do f=${r//-/_}.wasm; echo "$f"; echo "$f.keccak"; done | sort)
have=$(ls recipes/out | sort)
if [ "$want" != "$have" ]; then
  echo "recipes/out does not match RECIPES"; diff <(echo "$want") <(echo "$have"); exit 1
fi
