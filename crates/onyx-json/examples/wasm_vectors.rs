//! The shared vectors of the generic JSON fetcher as a program, for wasm32 (THREATS CL3-3):
//! `scripts/check-json-wasm.sh` builds it for `wasm32-unknown-unknown` and runs it in node, so
//! the browser build's reader gives the same answer as every native observer on every case. A
//! failed check panics, which traps, and the script fails. Natively it runs the same checks
//! (`cargo run -p onyx-json --example wasm_vectors`).
//!
//! Built with `ONYX_JSON_VECTORS_BREAK=1`, it checks a copy of the vectors with one answer
//! changed, which must fail: the script's self-test that a failure is reported on wasm32.

const VECTORS: &[u8] = include_bytes!("../../../fixtures/json-fetcher/vectors.json");

fn main() {
    let file: Vec<u8> = if option_env!("ONYX_JSON_VECTORS_BREAK").is_some() {
        String::from_utf8_lossy(VECTORS)
            .replacen("{\"number\":\"1052530\"}", "{\"number\":\"1052531\"}", 1)
            .into_bytes()
    } else {
        VECTORS.to_vec()
    };
    match onyx_json::vectors::check(&file) {
        Ok(n) => assert!(n >= 40, "only {n} cases"),
        Err(e) => panic!("json fetcher vectors failed: {e}"),
    }
}
