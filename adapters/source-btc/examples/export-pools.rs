//! Exports for the mining-pool block metric (`btc-pool-blocks`, ADR-0032).
//!
//! ```text
//! # Committed proof file (header + coinbase + Merkle branch per block). Only
//! # commit a range whose coinbases carry pool tags alone and pay at most two
//! # outputs (POOL-R1-F2; `tests/pool_review.rs` enforces it):
//! cargo run --release -p onyx-source-btc --example export-pools -- \
//!   proofs fixtures-live/week 970019 970029 fixtures/btc/coinbase-970019-970029.json
//! # One-week pool shares for the website, one shard per UTC day of the
//! # week history (same days as apps/web/static/history/week/index.json):
//! cargo run --release -p onyx-source-btc --example export-pools -- \
//!   week fixtures-live/week apps/web/static/history/week/index.json \
//!   apps/web/static/history/pools-week.json
//! ```
//!
//! `week` preflights every day's shard (<= half its fuel), runs it on the
//! canonical engine and prints the commitment the website pins
//! (`apps/web/verify` `pinned::POOLS_*`).

use borsh::BorshDeserialize;
use onyx_coordinator::cas::{Cas, MemCas};
use onyx_coordinator::recompute::preflight;
use onyx_core::{keccak256, Registry, RuntimeProfile, ShardSpec, SourceId, SourceRef};
use onyx_labels::attest::AttestCache;
use onyx_merkle::MerkleTree;
use onyx_pool_schema::PoolBlocksRow;
use onyx_recipe_host::{assemble_input, Host, Outcome, HOST_ID};
use onyx_recipe_sdk::OutputRow;
use onyx_source_btc::node::{NodePin, MAINNET_MIN_BITS};
use onyx_source_btc::pools::{
    export_coinbase_proofs, load_coinbase_range, pool_table, pools_commitment, PoolShard,
};
use onyx_source_btc::{fixture_registry_pools, SCHEMA_POOL_BLOCKS, SOURCE_ID};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn profile() -> RuntimeProfile {
    RuntimeProfile {
        abi_version: 1,
        host: HOST_ID.into(),
        fuel_limit: 4_000_000_000,
        max_memory_bytes: 256 << 20,
        max_output_bytes: 1 << 20,
        max_call_depth: 512,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pin = NodePin {
        min_bits: MAINNET_MIN_BITS,
        tip: None,
    };
    match args.first().map(String::as_str) {
        Some("proofs") if args.len() == 5 => {
            let (from, to): (u64, u64) = (args[2].parse()?, args[3].parse()?);
            let proofs = export_coinbase_proofs(&PathBuf::from(&args[1]), from, to, &pin)?;
            std::fs::write(&args[4], serde_json::to_string_pretty(&proofs)? + "\n")?;
            eprintln!("wrote {} ({} blocks)", args[4], proofs.blocks.len());
            Ok(())
        }
        Some("week") if args.len() == 4 => week(&root, &args[1], &args[2], &args[3], &pin),
        _ => Err("usage: export-pools proofs <raw dir> <from> <to> <out.json> | week <raw dir> <week index.json> <out.json>".into()),
    }
}

fn week(
    root: &std::path::Path,
    dir: &str,
    index: &str,
    out: &str,
    pin: &NodePin,
) -> Result<(), Box<dyn std::error::Error>> {
    let idx: Value = serde_json::from_slice(&std::fs::read(index)?)?;
    let tip_hex = idx["tip"].as_str().ok_or("index tip")?;
    let mut tip = [0u8; 32];
    hex::decode_to_slice(tip_hex, &mut tip)?;
    let (table, info) = pool_table(&root.join("fixtures"))?;
    let params = borsh::to_vec(&table)?;
    let wasm = std::fs::read(root.join("recipes/out/btc_pool_blocks.wasm"))?;
    let id = keccak256(&wasm);
    let reg = fixture_registry_pools();
    let cas = MemCas::new();
    cas.put(onyx_coordinator::cas::Kind::Recipe, &wasm)?;
    let host = Host::new()?;
    let mut days = Vec::new();
    let mut shards = Vec::new();
    let mut totals: BTreeMap<String, u64> = BTreeMap::new();
    let last = idx["days"].as_array().ok_or("days")?.len();
    for (n, d) in idx["days"].as_array().ok_or("days")?.iter().enumerate() {
        let date = d["date"].as_str().ok_or("date")?;
        let from = d["from"].as_u64().ok_or("from")?;
        let to = d["to"].as_u64().ok_or("to")?;
        let day_pin = NodePin {
            min_bits: pin.min_bits,
            tip: (n + 1 == last).then(|| (to, tip_hex.to_string())),
        };
        let (source, snapshot) = load_coinbase_range(&PathBuf::from(dir), from, to, &day_pin)?;
        let spec = ShardSpec {
            recipe: Some(id),
            runtime: Some(profile()),
            label_set: None,
            source: SourceRef {
                source: SourceId(SOURCE_ID.into()),
                snapshot,
            },
            schema: keccak256(SCHEMA_POOL_BLOCKS),
            rights_hash: reg
                .source_rights(&SourceId(SOURCE_ID.into()))
                .ok_or("source")?
                .hash(),
            params: params.clone(),
            registry_root: reg.root(),
        };
        spec.validate(&reg)?;
        let fuel = preflight(
            &spec,
            source.clone(),
            &reg,
            &cas,
            &host,
            &AttestCache::new(),
            &Default::default(),
        )
        .map_err(|e| format!("{date}: preflight refused the shard: {e:?}"))?;
        let input = assemble_input(&spec, source, None, &[], &reg)?.bytes;
        let r = host.run(&id, &wasm, &input, &profile())?;
        if r.outcome != Outcome::Ok || r.fuel_used != fuel {
            return Err(format!(
                "{date}: run {:?}, fuel {} vs {fuel}",
                r.outcome, r.fuel_used
            )
            .into());
        }
        let tree = MerkleTree::from_rows(&r.leaves);
        let mut rows = Vec::new();
        for l in &r.leaves {
            let row = OutputRow::try_from_slice(l)?;
            let v = PoolBlocksRow::try_from_slice(&row.value)?;
            *totals.entry(v.pool.clone()).or_default() += v.blocks;
            rows.push(json!({"pool": v.pool, "blocks": v.blocks}));
        }
        let blocks: u64 = rows.iter().filter_map(|r| r["blocks"].as_u64()).sum();
        if blocks != to - from + 1 {
            return Err(format!(
                "{date}: rows count {blocks} blocks, range has {}",
                to - from + 1
            )
            .into());
        }
        eprintln!(
            "{date} {from}-{to}: {} rows, fuel {fuel} ({} bp of limit)",
            rows.len(),
            fuel * 10_000 / 4_000_000_000
        );
        shards.push(PoolShard {
            from,
            to,
            root: tree.root(),
        });
        days.push(
            json!({"date": date, "from": from, "to": to, "root": hex0x(&tree.root().0),
            "leaves": r.leaves.iter().map(|l| hex0x(l)).collect::<Vec<_>>(),
            "rows": rows, "fuel": fuel}),
        );
    }
    let table_hash = keccak256(&params);
    let commitment = pools_commitment(&id, &table_hash, &tip, &shards);
    let named: Vec<Value> = info
        .iter()
        .map(|p| json!({"id": p.id, "name": p.name, "link": p.link}))
        .collect();
    let doc = json!({
        "version": 1,
        "generated_by": "cargo run --release -p onyx-source-btc --example export-pools -- week <raw dir> <week index.json> <out>",
        "recipe_id": hex0x(&id.0),
        "table_hash": hex0x(&table_hash.0),
        "list_hash": hex0x(&table.list_hash),
        "list_source": "https://github.com/bitcoin-data/mining-pools (branch generated), MIT; fixtures/pools/",
        "tip": tip_hex,
        "from": idx["from"], "to": idx["to"],
        "pools": named,
        "totals": totals,
        "days": days,
        "commitment": hex0x(&commitment.0),
        "python_recount": "scripts/recount-pool-blocks.py: every day matches",
    });
    std::fs::write(out, serde_json::to_string(&doc)? + "\n")?;
    eprintln!("wrote {out}");
    println!("POOLS_COMMITMENT={}", hex0x(&commitment.0));
    println!("POOLS_RECIPE={}", hex0x(&id.0));
    println!("POOLS_TABLE={}", hex0x(&table_hash.0));
    Ok(())
}
