#!/usr/bin/env bash
# ROADMAP C50 (THREATS C50-9, C12-R2-6): the time-lock known-answer checks on wasm32. Builds onyx-tlock's
# `wasm_kat` example for wasm32-unknown-unknown (blst's C compiled by clang, as
# scripts/build-web.sh does for secp256k1) and runs it in node: drand/tlock's
# Go and tlock-js ciphertexts decrypt under published drand signatures and
# re-encrypt to identical bytes, published quicknet rounds verify, and Onyx's
# zero-tail vectors and the far-date copies of ROADMAP C52 (sigma derived from the
# body key with Keccak-256) re-create byte for byte (fixtures/tlock). The module must
# import nothing. Self-test first: a build that requires a tampered ciphertext
# to open must FAIL here, so a trap is never read as a pass.
# Needs: clang, node (both already needed by scripts/build-web.sh).
set -euo pipefail
cd "$(dirname "$0")/.."
command -v clang >/dev/null || { echo "check-tlock-wasm: clang missing (needed for blst on wasm32)"; exit 1; }
command -v node >/dev/null || { echo "check-tlock-wasm: node missing"; exit 1; }
target="${CARGO_TARGET_DIR:-target}"
wasm="$target/wasm32-unknown-unknown/debug/examples/wasm_kat.wasm"
run() { # wasm file
  node - "$1" <<'JS'
const fs = require("fs");
const m = new WebAssembly.Module(fs.readFileSync(process.argv[2]));
const imports = WebAssembly.Module.imports(m);
if (imports.length) { console.error("unexpected imports:", JSON.stringify(imports)); process.exit(1); }
const inst = new WebAssembly.Instance(m, {});
let ret;
try { ret = inst.exports.main(0, 0); } catch (e) { console.error("trap:", e.message); process.exit(1); }
if (ret !== 0) { console.error("main returned", ret); process.exit(1); }
JS
}
cargo build --locked -q -p onyx-tlock --example wasm_kat --target wasm32-unknown-unknown
cp "$wasm" "$target/wasm_kat-ok.wasm"
ONYX_TLOCK_KAT_BREAK=1 cargo build --locked -q -p onyx-tlock --example wasm_kat --target wasm32-unknown-unknown
if run "$wasm" 2>/dev/null; then
  echo "check-tlock-wasm: self-test FAILED (a tampered ciphertext passed on wasm32)"; exit 1
fi
run "$target/wasm_kat-ok.wasm"
echo "tlock wasm32 ok: 7 drand rounds, 3 drand/tlock interop vectors, 8 zero-tail vectors, 5 far-date vectors (decrypt and re-encrypt byte for byte)"
