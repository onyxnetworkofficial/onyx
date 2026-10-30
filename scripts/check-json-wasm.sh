#!/usr/bin/env bash
# CL3 (ADR-0053, THREATS CL3-3): the generic JSON fetcher's shared vectors on wasm32. Builds
# onyx-json's `wasm_vectors` example for wasm32-unknown-unknown and runs it in node: every
# descriptor of fixtures/json-fetcher/vectors.json gives its Borsh bytes and fetcher id, and every
# case its answer, as natively (crates/onyx-json/tests/vectors.rs). The module must import
# nothing. Self-test first: a build that checks a copy with one answer changed must FAIL here.
# Needs: node.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v node >/dev/null || { echo "check-json-wasm: node missing"; exit 1; }
target="${CARGO_TARGET_DIR:-target}"
wasm="$target/wasm32-unknown-unknown/debug/examples/wasm_vectors.wasm"
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
ONYX_JSON_VECTORS_BREAK=1 cargo build --locked -q -p onyx-json --example wasm_vectors --target wasm32-unknown-unknown
if run "$wasm" 2>/dev/null; then
  echo "check-json-wasm: self-test FAILED (a changed answer passed on wasm32)"; exit 1
fi
cargo build --locked -q -p onyx-json --example wasm_vectors --target wasm32-unknown-unknown
run "$wasm"
echo "json fetcher wasm32 ok: every descriptor and case of fixtures/json-fetcher/vectors.json as natively"
