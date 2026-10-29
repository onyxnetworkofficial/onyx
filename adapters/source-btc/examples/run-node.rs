//! Run the reference recipe on blocks fetched from a node by
//! `scripts/fetch-btc-node.sh`, with the test label set.
//! Run: `cargo run --release -p onyx-source-btc --example run-node -- fixtures-live/node 970564 970566`

use borsh::BorshDeserialize;
use onyx_btc_schema::EntityFlowRow;
use onyx_core::{keccak256, LabelSetRef, Registry, RuntimeProfile, ShardSpec, SourceId, SourceRef};
use onyx_merkle::MerkleTree;
use onyx_recipe_host::{assemble_input, Host, HOST_ID};
use onyx_recipe_sdk::OutputRow;
use onyx_source_btc::node::{load_node_range, NodePin, MAINNET_MIN_BITS};
use onyx_source_btc::{fixture_registry, load_label_set, SOURCE_ID};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let (dir, from, to) = match &args[..] {
        [_, d, f, t] => (PathBuf::from(d), f.parse::<u64>()?, t.parse::<u64>()?),
        _ => return Err("usage: run-node <dir> <from> <to>".into()),
    };
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let pin = NodePin {
        min_bits: MAINNET_MIN_BITS,
        tip: None,
    };
    let (source, snapshot) = load_node_range(&dir, from, to, &pin)?;
    println!(
        "checked blocks {from}-{to}: {} source bytes, content hash {}",
        source.len(),
        keccak256(&source)
    );
    let reg = fixture_registry();
    let labels = load_label_set(
        &root.join("fixtures"),
        "labels/test.onyx.entities.btc.v1.json",
    )?;
    let wasm = std::fs::read(root.join("recipes/out/btc_entity_balance.wasm"))?;
    let profile = RuntimeProfile {
        abi_version: 1,
        host: HOST_ID.into(),
        fuel_limit: 4_000_000_000,
        max_memory_bytes: 256 << 20,
        max_output_bytes: 1 << 20,
        max_call_depth: 512,
    };
    let spec = ShardSpec {
        recipe: Some(keccak256(&wasm)),
        runtime: Some(profile.clone()),
        label_set: Some(LabelSetRef {
            name: labels.name.clone(),
            version: labels.version,
            content_hash: labels.content_hash(),
        }),
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot,
        },
        schema: keccak256(b"onyx.btc.entity-flow-row.v1"),
        rights_hash: reg
            .source_rights(&SourceId(SOURCE_ID.into()))
            .ok_or("rights")?
            .hash(),
        params: Vec::new(),
        registry_root: reg.root(),
    };
    let input = assemble_input(&spec, source, Some(&labels), &[], &reg)?.bytes;
    let r = Host::new()?.run(&keccak256(&wasm), &wasm, &input, &profile)?;
    for l in &r.leaves {
        let row = EntityFlowRow::try_from_slice(&OutputRow::try_from_slice(l)?.value)?;
        println!(
            "{}: received {} sent {} net {} sats, {} txs",
            row.entity, row.received_sats, row.sent_sats, row.net_sats, row.tx_count
        );
    }
    println!(
        "root {} fuel {}",
        MerkleTree::from_rows(&r.leaves).root(),
        r.fuel_used
    );
    Ok(())
}
