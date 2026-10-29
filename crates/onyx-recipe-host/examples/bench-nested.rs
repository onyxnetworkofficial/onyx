//! Isolate fast-path compile cost of multi-value blocks.
//! Usage: bench-nested <engine: wasmi|fast> <nests> <depth> <params>

// Benchmarks report wall-clock seconds; not protocol code.
#![allow(clippy::float_arithmetic)]

use onyx_core::{keccak256, RuntimeProfile};
use onyx_recipe_host::fast::FastHost;
use onyx_recipe_host::{Host, HOST_ID};
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (engine, nests, depth, params): (&str, usize, usize, usize) = (
        &a[1],
        a[2].parse().expect("nests"),
        a[3].parse().expect("depth"),
        a[4].parse().expect("params"),
    );
    let results = params.min(16);
    let p = "i32 ".repeat(params);
    let r = "i32 ".repeat(results);
    let nest = format!(
        "{}{}{}{}{}",
        "i32.const 1 ".repeat(params),
        "(block (type $t) ".repeat(depth),
        "drop ".repeat(params - results),
        ")".repeat(depth),
        "drop ".repeat(results)
    );
    let text = format!(
        r#"(module (type $t (func (param {p}) (result {r})))
          (memory (export "memory") 1)
          (data (i32.const 2048) "\00\01\00\00\00\01\00\00\00\01\01\00\00\00\02")
          (func (export "onyx_abi_version") (result i32) i32.const 1)
          (func (export "onyx_alloc") (param i32) (result i32) i32.const 1024)
          (func (export "onyx_run") (param i32 i32) (result i64) i64.const 8796093022223)
          (func {}))"#,
        nest.repeat(nests)
    );
    let w = wat::parse_str(text).expect("wat");
    let prof = RuntimeProfile {
        abi_version: 1,
        host: HOST_ID.into(),
        fuel_limit: 1 << 30,
        max_memory_bytes: 16 << 20,
        max_output_bytes: 1 << 20,
        max_call_depth: 64,
    };
    let t = Instant::now();
    let r = match engine {
        "wasmi" => Host::new().expect("h").run(&keccak256(&w), &w, b"", &prof),
        _ => {
            FastHost::new()
                .expect("h")
                .allowing(keccak256(&w))
                .run(&keccak256(&w), &w, b"", &prof)
        }
    };
    println!(
        "{engine} nests={nests} depth={depth} params={params} bytes={} -> {:?} in {:.2}s",
        w.len(),
        r.map(|r| r.outcome),
        t.elapsed().as_secs_f64()
    );
}
