//! Benchmark the reference recipe on the fixture blocks with both engines.
//!
//! Run: `cargo run --release -p onyx-source-btc --example bench-golden`

// Benchmarks report wall-clock seconds; not protocol code.
#![allow(clippy::float_arithmetic)]

use onyx_core::{
    keccak256, LabelSetRef, Registry, RuntimeProfile, ShardSpec, SourceAdapter, SourceId, SourceRef,
};
use onyx_recipe_host::fast::FastHost;
use onyx_recipe_host::{assemble_input, Host, HOST_ID};
use onyx_source_btc::{fixture_registry, load_label_set, BtcFixtureAdapter, SOURCE_ID};
use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let fixtures = root.join("fixtures");
    let wasm = std::fs::read(root.join("recipes/out/btc_entity_balance.wasm"))?;
    let id = keccak256(&wasm);
    let reg = fixture_registry();
    let adapter = BtcFixtureAdapter::new(&fixtures, "btc/blocks-921080-921082.json");
    let t = Instant::now();
    let source = adapter.load(&adapter.range_snapshot()?)?;
    let load_s = t.elapsed().as_secs_f64();
    let labels = load_label_set(&fixtures, "labels/test.onyx.entities.btc.v1.json")?;
    let pin = LabelSetRef {
        name: labels.name.clone(),
        version: labels.version,
        content_hash: labels.content_hash(),
    };
    let snapshot = adapter.range_snapshot()?;
    let rights = reg
        .source_rights(&SourceId(SOURCE_ID.into()))
        .ok_or("source not registered")?
        .hash();
    let spec = ShardSpec {
        recipe: None,
        runtime: None,
        label_set: Some(pin),
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot,
        },
        schema: keccak256(b"onyx.btc.entity-flow-row.v1"),
        rights_hash: rights,
        params: vec![],
        registry_root: reg.root(),
    };
    let input = assemble_input(&spec, source, Some(&labels), &[], &reg)?.bytes;
    let profile = RuntimeProfile {
        abi_version: 1,
        host: HOST_ID.into(),
        fuel_limit: 4_000_000_000,
        max_memory_bytes: 256 << 20,
        max_output_bytes: 1 << 20,
        max_call_depth: 512,
    };
    println!(
        "input {} KiB, adapter load+verify {load_s:.2} s",
        input.len() / 1024
    );
    println!("| engine | first run s (incl. load) | warm run s (median of 5) | fuel used | root |");
    println!("|---|---|---|---|---|");
    let canonical = Host::new()?;
    let fast = FastHost::new()?.allowing(id);
    for name in ["wasmi (canonical)", "wasmtime opt none (advisory)"] {
        let run = || match name {
            "wasmi (canonical)" => canonical.run(&id, &wasm, &input, &profile),
            _ => fast.run(&id, &wasm, &input, &profile),
        };
        let t = Instant::now();
        let first = run()?;
        let first_s = t.elapsed().as_secs_f64();
        let mut times = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            let r = run()?;
            times.push(t.elapsed().as_secs_f64());
            assert_eq!(r, first, "nondeterministic");
        }
        times.sort_by(|a, b| a.total_cmp(b));
        println!(
            "| {name} | {first_s:.3} | {:.3} | {} | {} |",
            times[2],
            first.fuel_used,
            first.root()
        );
    }
    Ok(())
}
