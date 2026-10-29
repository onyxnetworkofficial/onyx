//! Export the website's on-chain window (ADR-0043): one `btc-chain-stats`
//! shard per block over every block in the given folders, each preflighted
//! (<= half its fuel) and run on the canonical engine, then recounted by
//! `scripts/recount-chain-stats.py` from the node's own JSON.
//!
//! ```text
//! # The week and its tail (969,839-971,199; the first block is the parent):
//! cargo run --release -p onyx-source-btc --example export-chain-stats -- \
//!   apps/web/static/onchain fixtures-live/week:fixtures-live/tail
//! # A sub-range:
//! cargo run --release -p onyx-source-btc --example export-chain-stats -- \
//!   <out dir> <dir>[:<dir>...] <from> <to>
//! ```
//!
//! Writes `<out>/btc-chain-stats.json` (recipe id, range, tip, one leaf per
//! block grouped by UTC day, the commitment the website pins) and
//! `<out>/rerun-input.b64.txt` (the full recipe input of the window's
//! smallest block, which the browser reruns). Prints the pins
//! (`apps/web/verify` `pinned::CHAIN_*`). `ONYX_NO_RECOUNT=1` skips the
//! Python recount (the file then says so).
//!
//! `export-chain-stats rerun <out dir> <dirs> <height>` replaces only the
//! shipped block input with block `<height>`'s, after checking that its rerun
//! gives the leaf the window already holds for that block.

use borsh::BorshDeserialize;
use onyx_chain_schema::ChainStatsRow;
use onyx_coordinator::cas::{Cas, Kind, MemCas};
use onyx_coordinator::recompute::preflight;
use onyx_core::{keccak256, Registry, RuntimeProfile, ShardSpec, SourceId, SourceRef};
use onyx_labels::attest::AttestCache;
use onyx_merkle::MerkleTree;
use onyx_recipe_host::{assemble_input, Host, Outcome, HOST_ID};
use onyx_recipe_sdk::OutputRow;
use onyx_source_btc::chain::{block_dir, chain_stats_commitment, load_chain_range, ChainShard};
use onyx_source_btc::history::utc_date;
use onyx_source_btc::node::{NodePin, MAINNET_MIN_BITS};
use onyx_source_btc::{fixture_registry_chain, SCHEMA_CHAIN_STATS, SOURCE_ID};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Instant;

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

/// The contiguous run of heights in `dirs` whose blocks (and parent) exist:
/// from the lowest height with both files and its parent's raw file, upwards.
fn window(dirs: &[PathBuf]) -> Option<(u64, u64)> {
    let mut heights: Vec<u64> = Vec::new();
    for d in dirs {
        for e in std::fs::read_dir(d).ok()? {
            let name = e.ok()?.file_name().into_string().ok()?;
            if let Some(h) = name.strip_suffix(".v3.json.gz") {
                heights.push(h.parse().ok()?);
            }
        }
    }
    heights.sort_unstable();
    heights.dedup();
    let first = *heights.first()?;
    // The lowest block is the parent of the window's first block.
    let from = first + 1;
    let mut to = None;
    for w in heights.windows(2) {
        if w[1] != w[0] + 1 {
            break;
        }
        to = Some(w[1]);
    }
    to.map(|t| (from, t))
}

/// Fewest transactions a block the browser reruns may hold.
const RERUN_MIN_TXS: u64 = 1_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("rerun") {
        return match args.as_slice() {
            [_, out, dirs, h] => rerun_only(&root, &PathBuf::from(out), dirs, h.parse()?),
            _ => Err("usage: export-chain-stats rerun <out dir> <dir>[:<dir>...] <height>".into()),
        };
    }
    let usage = "usage: export-chain-stats <out dir> <dir>[:<dir>...] [<from> <to>]";
    let (out, dirs_arg) = match args.as_slice() {
        [o, d] | [o, d, _, _] => (PathBuf::from(o), d.clone()),
        _ => return Err(usage.into()),
    };
    let dirs: Vec<PathBuf> = dirs_arg.split(':').map(PathBuf::from).collect();
    let (from, to) = match args.as_slice() {
        [_, _, f, t] => (f.parse()?, t.parse()?),
        _ => window(&dirs).ok_or("no contiguous blocks in the folders")?,
    };
    if from > to || (from..=to).any(|h| block_dir(&dirs, h).is_none()) {
        return Err(format!("blocks {from}-{to} are not all in {dirs_arg}").into());
    }
    let pin = NodePin {
        min_bits: MAINNET_MIN_BITS,
        tip: None,
    };
    let wasm = std::fs::read(root.join("recipes/out/btc_chain_stats.wasm"))?;
    let id = keccak256(&wasm);
    let reg = fixture_registry_chain();
    let cas = MemCas::new();
    cas.put(Kind::Recipe, &wasm)?;
    let host = Host::new()?;
    let started = Instant::now();
    let mut shards = Vec::new();
    // (date, from, to, leaves)
    let mut days: Vec<(String, u64, u64, Vec<String>)> = Vec::new();
    let (mut max_fuel, mut fuel_sum) = (0u64, 0u64);
    // The smallest block's input: (source bytes, height, input).
    let mut rerun: Option<(usize, u64, Vec<u8>, u64)> = None;
    let mut tip = [0u8; 32];
    for height in from..=to {
        let (source, snapshot) = load_chain_range(&dirs, height, height, &pin)?;
        let spec = ShardSpec {
            recipe: Some(id),
            runtime: Some(profile()),
            label_set: None,
            source: SourceRef {
                source: SourceId(SOURCE_ID.into()),
                snapshot,
            },
            schema: keccak256(SCHEMA_CHAIN_STATS),
            rights_hash: reg
                .source_rights(&SourceId(SOURCE_ID.into()))
                .ok_or("source")?
                .hash(),
            params: vec![],
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
        .map_err(|e| format!("{height}: preflight refused the shard: {e:?}"))?;
        let size = source.len();
        let input = assemble_input(&spec, source, None, &[], &reg)?.bytes;
        let r = host.run(&id, &wasm, &input, &profile())?;
        if r.outcome != Outcome::Ok || r.fuel_used != fuel || r.leaves.len() != 1 {
            return Err(format!(
                "{height}: run {:?}, fuel {} vs {fuel}, {} leaves",
                r.outcome,
                r.fuel_used,
                r.leaves.len()
            )
            .into());
        }
        let leaf = &r.leaves[0];
        let row = ChainStatsRow::try_from_slice(&OutputRow::try_from_slice(leaf)?.value)?;
        if row.height != height {
            return Err(format!("{height}: row is for {}", row.height).into());
        }
        tip = row.hash;
        max_fuel = max_fuel.max(fuel);
        fuel_sum += fuel;
        // The browser reruns the smallest block that still holds at least
        // RERUN_MIN_TXS transactions (an empty block would prove little).
        if row.txs >= RERUN_MIN_TXS && rerun.as_ref().is_none_or(|(s, ..)| size < *s) {
            rerun = Some((size, height, input, fuel));
        }
        shards.push(ChainShard {
            from: height,
            to: height,
            root: MerkleTree::from_rows(&r.leaves).root(),
        });
        let date = utc_date(u64::from(row.time));
        match days.last_mut() {
            Some((d, _, t, leaves)) if *d >= date => {
                *t = height;
                leaves.push(hex0x(leaf));
            }
            _ => days.push((date, height, height, vec![hex0x(leaf)])),
        }
        if (height - from) % 100 == 0 || height == to {
            eprintln!(
                "{height}: fuel {fuel} ({} bp), {} txs, {:.0} s",
                fuel * 10_000 / 4_000_000_000,
                row.txs,
                started.elapsed().as_secs_f64()
            );
        }
    }
    let commitment = chain_stats_commitment(&id, &tip, &shards);
    let (rerun_size, rerun_height, rerun_input, rerun_fuel) = rerun.ok_or("no blocks")?;
    std::fs::create_dir_all(&out)?;
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &rerun_input);
    std::fs::write(out.join("rerun-input.b64.txt"), b64 + "\n")?;
    let rerun_root = shards
        .iter()
        .find(|s| s.from == rerun_height)
        .map(|s| hex0x(&s.root.0))
        .ok_or("rerun shard")?;
    let doc_path = out.join("btc-chain-stats.json");
    let mut doc = json!({
        "version": 1,
        "generated_by": "cargo run --release -p onyx-source-btc --example export-chain-stats -- <out dir> <dir>[:<dir>...]",
        "rerun_command": "cargo run --release -p onyx-source-btc --example export-chain-stats -- <out dir> <block folder> <from> <to> (blocks from scripts/fetch-btc-node.sh or scripts/refresh-onchain.py)",
        "recipe": "btc-chain-stats",
        "recipe_id": hex0x(&id.0),
        "schema": hex0x(&keccak256(SCHEMA_CHAIN_STATS).0),
        "shard_blocks": 1,
        "from": from, "to": to,
        "tip": hex::encode(tip),
        "days": days.iter().map(|(d, f, t, l)| json!({"date": d, "from": f, "to": t, "leaves": l})).collect::<Vec<_>>(),
        "max_fuel": max_fuel,
        "fuel_limit": 4_000_000_000u64,
        "rerun": {
            "height": rerun_height,
            "file": "onchain/rerun-input.b64.txt",
            "input_keccak": hex0x(&keccak256(&rerun_input).0),
            "source_bytes": rerun_size,
            "root": rerun_root,
            "fuel": rerun_fuel,
        },
        "commitment": hex0x(&commitment.0),
        "python_recount": "not run (ONYX_NO_RECOUNT=1)",
    });
    std::fs::write(&doc_path, serde_json::to_string(&doc)? + "\n")?;
    eprintln!(
        "wrote {} ({} blocks, {} days; max fuel {max_fuel}, mean {}; {:.0} s)",
        doc_path.display(),
        shards.len(),
        days.len(),
        fuel_sum / shards.len() as u64,
        started.elapsed().as_secs_f64()
    );
    if std::env::var("ONYX_NO_RECOUNT").ok().as_deref() != Some("1") {
        let line = recount(&root, &doc_path, &dirs_arg)?;
        doc["python_recount"] = Value::String(line);
        std::fs::write(&doc_path, serde_json::to_string(&doc)? + "\n")?;
    }
    println!("CHAIN_RECIPE_ID={}", hex0x(&id.0));
    println!("CHAIN_FROM={from}");
    println!("CHAIN_TO={to}");
    println!("CHAIN_TIP={}", hex::encode(tip));
    println!("CHAIN_COMMITMENT={}", hex0x(&commitment.0));
    println!("CHAIN_RERUN_HEIGHT={rerun_height}");
    println!("CHAIN_RERUN_INPUT={}", hex0x(&keccak256(&rerun_input).0));
    Ok(())
}

/// Run the independent Python recount over the written file; its last line
/// is the summary the file records. Fails on any difference.
fn recount(root: &Path, doc: &Path, dirs: &str) -> Result<String, Box<dyn std::error::Error>> {
    let o = std::process::Command::new("python3")
        .arg("-I")
        .arg(root.join("scripts/recount-chain-stats.py"))
        .arg(doc)
        .arg(dirs)
        .args(["-j", "4"])
        .output()?;
    let text = String::from_utf8_lossy(&o.stdout).to_string();
    if !o.status.success() {
        return Err(format!(
            "python recount differs:\n{text}\n{}",
            String::from_utf8_lossy(&o.stderr)
        )
        .into());
    }
    Ok(text.lines().last().unwrap_or("").to_string())
}

/// Write block `height`'s full recipe input as the shipped rerun input, after
/// checking that the canonical engine gives the window's own leaf for it.
fn rerun_only(
    root: &Path,
    out: &Path,
    dirs_arg: &str,
    height: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let dirs: Vec<PathBuf> = dirs_arg.split(':').map(PathBuf::from).collect();
    let doc_path = out.join("btc-chain-stats.json");
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&doc_path)?)?;
    let wasm = std::fs::read(root.join("recipes/out/btc_chain_stats.wasm"))?;
    let id = keccak256(&wasm);
    if doc["recipe_id"].as_str() != Some(hex0x(&id.0).as_str()) {
        return Err("the window was made by another recipe".into());
    }
    let want = doc["days"]
        .as_array()
        .ok_or("days")?
        .iter()
        .flat_map(|d| d["leaves"].as_array().cloned().unwrap_or_default())
        .filter_map(|l| l.as_str().map(str::to_string))
        .find(|l| {
            hex::decode(l.trim_start_matches("0x"))
                .ok()
                .and_then(|b| OutputRow::try_from_slice(&b).ok())
                .is_some_and(|r| r.key == ChainStatsRow::key(height))
        })
        .ok_or("the window holds no leaf for that block")?;
    let pin = NodePin {
        min_bits: MAINNET_MIN_BITS,
        tip: None,
    };
    let (source, snapshot) = load_chain_range(&dirs, height, height, &pin)?;
    let reg = fixture_registry_chain();
    let spec = ShardSpec {
        recipe: Some(id),
        runtime: Some(profile()),
        label_set: None,
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot,
        },
        schema: keccak256(SCHEMA_CHAIN_STATS),
        rights_hash: reg
            .source_rights(&SourceId(SOURCE_ID.into()))
            .ok_or("source")?
            .hash(),
        params: vec![],
        registry_root: reg.root(),
    };
    spec.validate(&reg)?;
    let size = source.len();
    let input = assemble_input(&spec, source, None, &[], &reg)?.bytes;
    let r = Host::new()?.run(&id, &wasm, &input, &profile())?;
    if r.outcome != Outcome::Ok || r.leaves.len() != 1 || hex0x(&r.leaves[0]) != want {
        return Err(format!("{height}: the rerun does not give the window's leaf").into());
    }
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &input);
    std::fs::write(out.join("rerun-input.b64.txt"), b64 + "\n")?;
    doc["rerun"] = json!({
        "height": height,
        "file": "onchain/rerun-input.b64.txt",
        "input_keccak": hex0x(&keccak256(&input).0),
        "source_bytes": size,
        "root": hex0x(&MerkleTree::from_rows(&r.leaves).root().0),
        "fuel": r.fuel_used,
    });
    std::fs::write(&doc_path, serde_json::to_string(&doc)? + "\n")?;
    println!("CHAIN_RERUN_HEIGHT={height}");
    println!("CHAIN_RERUN_INPUT={}", hex0x(&keccak256(&input).0));
    Ok(())
}
