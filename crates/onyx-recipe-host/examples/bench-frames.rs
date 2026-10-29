//! Measure native bytes per frame on the advisory wasmtime fast path for
//! worst-case recipe shapes (the canonical wasmi engine has no native frames).
//!
//! Method: give wasm a small, fixed native stack (`max_wasm_stack`) and find,
//! by binary search, the deepest recursion that still succeeds. Native frame
//! size ~= stack / depth. Compare with the wasm-level frame slots the host
//! counts (params + locals + max operand height).
//!
//! Run: `cargo run --release -p onyx-recipe-host --example bench-frames`

// Benchmarks report wall-clock seconds; not protocol code.
#![allow(clippy::float_arithmetic)]

use onyx_core::{keccak256, RuntimeProfile};
use onyx_recipe_host::fast::{FastHost as Host, HostOptions};
use onyx_recipe_host::{Outcome, HOST_ID, MAX_CALL_DEPTH};

const OK_RUN: &str = "i64.const 8796093022223";
const STACK: usize = 8 << 20;

fn wrap(run_body: &str, extra: &str) -> Vec<u8> {
    let text = format!(
        r#"(module
          (memory (export "memory") 1)
          (data (i32.const 2048) "\00\01\00\00\00\01\00\00\00\01\01\00\00\00\02")
          (func (export "onyx_abi_version") (result i32) i32.const 1)
          (func (export "onyx_alloc") (param i32) (result i32) i32.const 1024)
          (func (export "onyx_run") (param i32 i32) (result i64) {run_body})
          {extra})"#
    );
    wat::parse_str(text).expect("wat")
}

/// A recursive `$f (param $n i32)` whose body is `body` around the recursive call.
fn recursive(pre: &str, post: &str, locals: &str, depth: u32) -> Vec<u8> {
    let f = format!(
        r#"(func $f (param $n i32) {locals}
        {pre}
        (if (i32.gt_u (local.get $n) (i32.const 0)) (then
          (call $f (i32.sub (local.get $n) (i32.const 1)))))
        {post})"#
    );
    wrap(&format!("(call $f (i32.const {depth})) {OK_RUN}"), &f)
}

fn shapes(depth: u32) -> Vec<(&'static str, u32, Vec<u8>)> {
    // Live operand stack across the call: 2000 i64 values.
    let loads = "(i64.load (i32.const 0)) ".repeat(2000);
    // Use every value after the call so none can be dropped early.
    let drops = format!("{} drop", "i64.add ".repeat(1999));
    let opstack = {
        let f = format!(
            r#"(func $f (param $n i32)
            {loads}
            (if (i32.gt_u (local.get $n) (i32.const 0)) (then
              (call $f (i32.sub (local.get $n) (i32.const 1)))))
            {drops})"#
        );
        wrap(&format!("(call $f (i32.const {depth})) {OK_RUN}"), &f)
    };
    // 2000 i64 locals, all written before and read after the call.
    let locals = format!("(local {})", "i64 ".repeat(2000));
    let sets: String = (1..=2000)
        .map(|i| format!("(local.set {i} (i64.load (i32.const {})))", i * 8 % 60000))
        .collect();
    let gets: String = (1..=2000)
        .map(|i| format!("(i64.store (i32.const 0) (local.get {i}))"))
        .collect();
    // Loop-invariant products (LICM shape), 36.9k of them.
    let mut licm = String::new();
    for k in 0..30_000i64 {
        licm.push_str(&format!(
            "local.get 1 i64.const {} i64.mul i64.xor\n",
            2 * k + 3
        ));
    }
    let licm_pre = format!(
        "(local.set 1 (i64.load (i32.const 0))) (loop $L local.get 2 {licm} local.set 2 \
         local.get 3 i32.const 1 i32.add local.tee 3 i32.const 2 i32.lt_u br_if $L)"
    );
    vec![
        ("empty frame", 1, recursive("", "", "", depth)),
        ("2000 live operands", 2001, opstack),
        ("2000 locals", 2001, recursive(&sets, &gets, &locals, depth)),
        (
            "licm 30k (7 slots)",
            7,
            recursive(&licm_pre, "", "(local i64 i64 i32)", depth),
        ),
    ]
}

fn outcome(host: &Host, wasm: &[u8]) -> String {
    let p = RuntimeProfile {
        abi_version: 1,
        host: HOST_ID.into(),
        fuel_limit: 400_000_000_000,
        max_memory_bytes: 16 << 20,
        max_output_bytes: 1 << 20,
        max_call_depth: MAX_CALL_DEPTH,
    };
    host.allow(keccak256(wasm));
    match host.run(&keccak256(wasm), wasm, b"", &p) {
        Ok(r) => format!("{:?}", r.outcome),
        Err(e) => format!("rejected: {e}"),
    }
}

fn ok_at(host: &Host, wasm: &[u8]) -> bool {
    let p = RuntimeProfile {
        abi_version: 1,
        host: HOST_ID.into(),
        fuel_limit: 400_000_000_000,
        max_memory_bytes: 16 << 20,
        max_output_bytes: 1 << 20,
        max_call_depth: MAX_CALL_DEPTH,
    };
    host.allow(keccak256(wasm));
    matches!(
        host.run(&keccak256(wasm), wasm, b"", &p).map(|r| r.outcome),
        Ok(Outcome::Ok)
    )
}

fn main() {
    println!("native stack for wasm: {} MiB", STACK >> 20);
    println!("| shape | wasm slots | optimizer | max depth ok | bytes/frame | bytes/slot |");
    println!("|---|---|---|---|---|---|");
    for optimize in [false, true] {
        let host = Host::with_options(HostOptions {
            optimize,
            max_wasm_stack: STACK,
            ..HostOptions::default()
        })
        .expect("host");
        for idx in 0..4 {
            let (name, slots, w1) = &shapes(1)[idx];
            if !ok_at(&host, w1) {
                println!(
                    "| {name} | {slots} | {} | fails at depth 1: {} | | |",
                    if optimize { "speed" } else { "none" },
                    outcome(&host, w1)
                );
                continue;
            }
            // Binary search the deepest successful recursion (bounded by MAX_CALL_DEPTH - 2).
            let (mut lo, mut hi) = (0u32, MAX_CALL_DEPTH - 2);
            if ok_at(&host, &shapes(hi)[idx].2) {
                println!(
                    "| {name} | {slots} | {} | >= {hi} (no overflow) | < {} | < {} |",
                    if optimize { "speed" } else { "none" },
                    STACK / hi as usize,
                    STACK / hi as usize / *slots as usize
                );
                continue;
            }
            while lo + 1 < hi {
                let mid = (lo + hi) / 2;
                if ok_at(&host, &shapes(mid)[idx].2) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let per_frame = STACK / (lo.max(1) as usize + 1);
            println!(
                "| {name} | {slots} | {} | {lo} | ~{per_frame} | ~{} |",
                if optimize { "speed" } else { "none" },
                per_frame / *slots as usize
            );
        }
    }
}
