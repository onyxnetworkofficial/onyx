//! Compile-time and frame-size benchmark for adversarial recipe shapes.
//!
//! Run: `cargo run --release -p onyx-recipe-host --example bench-compile`
//! Prints a markdown table: shape, wasm body size, optimizer on/off, compile
//! time, and whether a recursion to depth `d` (< the pinned limit) succeeded.

// Benchmarks report wall-clock seconds; not protocol code.
#![allow(clippy::float_arithmetic)]

use onyx_core::{keccak256, RuntimeProfile};
use onyx_recipe_host::fast::{FastHost, HostOptions};
use onyx_recipe_host::{Host, HostError, RunResult, HOST_ID, MAX_CALL_DEPTH};
use std::time::Instant;

const OK_RUN: &str = "i64.const 8796093022223";

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

/// Loop-invariant products (Cranelift LICM hoists them all): R-HOST-STACK-2 shape.
fn licm(values: usize, depth: u32) -> Vec<u8> {
    let mut body = String::new();
    let mut cnt = 0;
    'o: for base in ["$a", "$b", "$c", "$d", "$e"] {
        for k in (-8191i64..=8191).step_by(2) {
            if k == 1 || k == -1 {
                continue;
            }
            body.push_str(&format!("local.get {base} i64.const {k} i64.mul i64.xor\n"));
            cnt += 1;
            if cnt == values {
                break 'o;
            }
        }
    }
    let f = format!(
        r#"(func $f (param $n i32) (result i64) (local $acc i64) (local $i i32)
        (local $a i64) (local $b i64) (local $c i64) (local $d i64) (local $e i64)
        (local.set $a (i64.load (i32.const 0))) (local.set $b (i64.load (i32.const 8)))
        (local.set $c (i64.load (i32.const 16))) (local.set $d (i64.load (i32.const 24)))
        (local.set $e (i64.load (i32.const 32)))
        (loop $L local.get $acc {body} local.set $acc
          local.get $i i32.const 1 i32.add local.tee $i i32.const 2 i32.lt_u br_if $L)
        (if (i32.gt_u (local.get $n) (i32.const 0)) (then
          (local.set $acc (i64.add (local.get $acc) (call $f (i32.sub (local.get $n) (i32.const 1)))))))
        local.get $acc)"#
    );
    wrap(
        &format!("(drop (call $f (i32.const {depth}))) {OK_RUN}"),
        &f,
    )
}

/// Long straight-line function: compile-cost shape.
fn straight(ops: usize) -> Vec<u8> {
    let body = "local.get 0 i64.const 3 i64.mul i64.const 7 i64.add local.set 0\n".repeat(ops);
    wrap(OK_RUN, &format!("(func (param i64) {body})"))
}

/// Worst case the caps allow for br_table cost: many functions, each with one
/// br_table of `MAX_BR_TABLE_TARGETS_PER_FUNCTION - 1` targets carrying
/// `MAX_TYPE_RESULTS` values, filling about `kib` KiB (L8-COMPILE-3).
fn br_table_worst(kib: usize) -> Vec<u8> {
    let n = onyx_recipe_host::MAX_TYPE_RESULTS;
    let results = "i64 ".repeat(n);
    let consts = "i64.const 0 ".repeat(n);
    let targets = "$b ".repeat(onyx_recipe_host::MAX_BR_TABLE_TARGETS_PER_FUNCTION - 2);
    let one = format!(
        "(func (result {results}) (block $b (result {results}) {consts} (i32.const 0) (br_table {targets} $b)))\n"
    );
    let per = one.len() / 4; // rough wasm bytes per function (2-byte targets)
    let count = (kib * 1024 / per.max(1)).max(1);
    wrap(OK_RUN, &one.repeat(count))
}

/// Worst typed-block shape the caps allow: many functions, each with
/// `MAX_TYPED_BLOCKS_PER_FUNCTION` nested 64-param blocks, about `kib` KiB (L9-F2).
fn nested_multivalue(kib: usize) -> Vec<u8> {
    let params = "i32 ".repeat(64);
    let results = "i32 ".repeat(onyx_recipe_host::MAX_TYPE_RESULTS);
    // Each function holds the most typed blocks allowed, nested as deep as possible.
    let depth = onyx_recipe_host::MAX_TYPED_BLOCKS_PER_FUNCTION as usize;
    let push = "i32.const 1 ".repeat(64);
    let inner_drop = "drop ".repeat(64 - onyx_recipe_host::MAX_TYPE_RESULTS);
    let drop = "drop ".repeat(onyx_recipe_host::MAX_TYPE_RESULTS);
    // Block type: 64 params -> 16 results. Each level passes its 64 params into
    // the next; the innermost drops down to 16; every level returns 16.
    let nest = format!(
        "{push}{}{inner_drop}{}{drop}",
        "(block (type $t) ".repeat(depth),
        ")".repeat(depth)
    );
    let per = depth * 3 + 64 * 3; // rough wasm bytes per nest
    let count = (kib * 1024 / per).max(1);
    let text = format!(
        r#"(module
          (type $t (func (param {params}) (result {results})))
          (memory (export "memory") 1)
          (data (i32.const 2048) "\00\01\00\00\00\01\00\00\00\01\01\00\00\00\02")
          (func (export "onyx_abi_version") (result i32) i32.const 1)
          (func (export "onyx_alloc") (param i32) (result i32) i32.const 1024)
          (func (export "onyx_run") (param i32 i32) (result i64) {OK_RUN})
          {})"#,
        format!("(func {nest})").repeat(count)
    );
    wat::parse_str(text).expect("wat")
}

fn profile() -> RuntimeProfile {
    RuntimeProfile {
        abi_version: 1,
        host: HOST_ID.into(),
        fuel_limit: 40_000_000_000,
        max_memory_bytes: 16 << 20,
        max_output_bytes: 1 << 20,
        max_call_depth: MAX_CALL_DEPTH,
    }
}

fn main() {
    println!("| shape | wasm KiB | engine | load+run s | outcome |");
    println!("|---|---|---|---|---|");
    let depth = MAX_CALL_DEPTH - 24;
    let cases: Vec<(String, Vec<u8>)> = vec![
        ("licm 8k values".into(), licm(8_000, depth)),
        ("licm 16k values".into(), licm(16_000, depth)),
        ("licm 36.9k values".into(), licm(36_900, depth)),
        ("straight 4k ops".into(), straight(4_000)),
        ("straight 16k ops".into(), straight(16_000)),
        ("br_table worst under caps".into(), br_table_worst(760)),
        (
            "64-param typed blocks at the per-function cap, many functions".into(),
            nested_multivalue(900),
        ),
    ];
    type Runner = Box<dyn Fn(&[u8]) -> Result<RunResult, HostError>>;
    let canonical = Host::new().expect("host");
    let fast_none = FastHost::with_options(HostOptions::default()).expect("fast");
    let fast_speed = FastHost::with_options(HostOptions {
        optimize: true,
        ..HostOptions::default()
    })
    .expect("fast");
    let engines: Vec<(&str, Runner)> = vec![
        (
            "wasmi (canonical)",
            Box::new(move |w: &[u8]| canonical.run(&keccak256(w), w, b"", &profile())),
        ),
        (
            "wasmtime opt none",
            Box::new(move |w: &[u8]| {
                fast_none.allow(keccak256(w));
                fast_none.run(&keccak256(w), w, b"", &profile())
            }),
        ),
        (
            "wasmtime opt speed",
            Box::new(move |w: &[u8]| {
                fast_speed.allow(keccak256(w));
                fast_speed.run(&keccak256(w), w, b"", &profile())
            }),
        ),
    ];
    for (engine, run) in &engines {
        for (name, wasm) in &cases {
            let t = Instant::now();
            let r = run(wasm);
            let secs = t.elapsed().as_secs_f64();
            let outcome = match r {
                Ok(r) => format!("{:?}", r.outcome),
                Err(e) => format!("rejected: {e}"),
            };
            println!(
                "| {name} | {} | {engine} | {secs:.2} | {outcome} |",
                wasm.len() / 1024
            );
        }
    }
}
