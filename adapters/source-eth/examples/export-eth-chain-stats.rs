//! Export the website's Ethereum window (ADR-0044): one `eth-chain-stats`
//! shard per block over a window folder (`scripts/refresh-eth-onchain.py`
//! writes it), each preflighted (at most half its fuel) and run on the
//! canonical engine, then recounted by `scripts/recount-eth-chain-stats.py`
//! from the RPC's own JSON fields.
//!
//! ```text
//! cargo run --release -p onyx-source-eth --example export-eth-chain-stats -- \
//!   apps/web/static/onchain fixtures-live/eth [<from> <to>]
//! ```
//!
//! Writes `<out>/eth-chain-stats.json` (recipe id, range, tip, one leaf per
//! block grouped by UTC day, the window's fetch record and the commitment the
//! website pins) and `<out>/eth-rerun-input.b64.txt` (the full recipe input
//! of the window's smallest block with at least 100 transactions and a blob
//! transaction whose bytes hold no e-mail-like text or profile link (ETH-6),
//! which the browser reruns). Prints the pins
//! (`apps/web/verify` `pinned::ETH_CHAIN_*`). `ONYX_NO_RECOUNT=1` skips the
//! Python recount (the file then says so).

use borsh::BorshDeserialize;
use onyx_coordinator::cas::{Cas, Kind, MemCas};
use onyx_coordinator::recompute::preflight;
use onyx_core::{keccak256, RuntimeProfile};
use onyx_eth_schema::EthChainStatsRow;
use onyx_labels::attest::AttestCache;
use onyx_merkle::MerkleTree;
use onyx_recipe_host::{assemble_input, Host, Outcome, HOST_ID};
use onyx_recipe_sdk::OutputRow;
use onyx_source_eth::{
    cross_checked, eth_chain_stats_commitment, fixture_registry_eth, load_eth_range,
    spec_binds_snapshot, stats_spec, utc_date, EthShard, Window,
};
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

/// Fewest transactions a block the browser reruns may hold.
const RERUN_MIN_TXS: u64 = 100;

/// Hosts of personal profile links (as the public-build guard lists them, L9A-8).
const PROFILE_HOSTS: [&str; 14] = [
    "twitter.com/",
    "x.com/",
    "t.me/",
    "linkedin.com/in/",
    "facebook.com/",
    "instagram.com/",
    "reddit.com/u",
    "github.com/",
    "bsky.app/profile/",
    "youtube.com/",
    "tiktok.com/@",
    "discord.gg/",
    "medium.com/@",
    "mailto:",
];

/// ETH-6: a block input ships verbatim (the trie roots cover every byte), so the rerun block is
/// one whose bytes hold no e-mail-like text and no profile link. Deliberately broad: a false
/// positive only moves the choice to another block.
fn personal_text(bytes: &[u8]) -> bool {
    let low = bytes.to_ascii_lowercase();
    if PROFILE_HOSTS
        .iter()
        .any(|h| low.windows(h.len()).any(|w| w == h.as_bytes()))
    {
        return true;
    }
    let name = |b: u8| b.is_ascii_alphanumeric() || b"._%+-".contains(&b);
    low.iter().enumerate().any(|(i, &c)| {
        if c != b'@' || i == 0 || !name(low[i - 1]) {
            return false;
        }
        let domain: Vec<u8> = low[i + 1..]
            .iter()
            .copied()
            .take_while(|b| b.is_ascii_alphanumeric() || b".-".contains(b))
            .collect();
        domain
            .rsplit(|b| *b == b'.')
            .next()
            .is_some_and(|tld| tld.len() >= 2 && tld.iter().all(u8::is_ascii_alphabetic))
            && domain.contains(&b'.')
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: export-eth-chain-stats <out dir> <window dir> [<from> <to>]";
    let (out, dir) = match args.as_slice() {
        [o, d] | [o, d, _, _] => (PathBuf::from(o), PathBuf::from(d)),
        _ => return Err(usage.into()),
    };
    let w = Window::open(&dir)?;
    // P4A-R1-3: a window no second operator confirmed is not pinned unless asked for by name.
    let unchecked = std::env::var("ONYX_ETH_UNCHECKED").ok().as_deref() == Some("1");
    if let Err(why) = cross_checked(&w) {
        if !unchecked {
            return Err(format!(
                "{why} (ONYX_ETH_UNCHECKED=1 exports it anyway, marked unchecked)"
            )
            .into());
        }
        eprintln!("warning: {why}; the export says so");
    }
    let (from, to) = match args.as_slice() {
        [_, _, f, t] => (f.parse()?, t.parse()?),
        _ => w.range().ok_or("the manifest names no range")?,
    };
    if from > to || (from..=to).any(|n| !w.has(n)) {
        return Err(format!("blocks {from}-{to} are not all in {}", dir.display()).into());
    }
    let wasm = std::fs::read(root.join("recipes/out/eth_chain_stats.wasm"))?;
    let id = keccak256(&wasm);
    let reg = fixture_registry_eth();
    let cas = MemCas::new();
    cas.put(Kind::Recipe, &wasm)?;
    let host = Host::new()?;
    let started = Instant::now();
    let mut shards = Vec::new();
    let mut days: Vec<(String, u64, u64, Vec<String>)> = Vec::new();
    let (mut max_fuel, mut fuel_sum) = (0u64, 0u64);
    // The rerun block: (source bytes, number, input, fuel, txs).
    let mut rerun: Option<(usize, u64, Vec<u8>, u64, u64)> = None;
    let mut tip = [0u8; 32];
    let mut first_time = 0u64;
    let mut last_time = 0u64;
    for n in from..=to {
        let (source, snapshot) = load_eth_range(&w, n, n)?;
        let spec = stats_spec(id, profile(), snapshot);
        spec.validate(&reg)?;
        if !spec_binds_snapshot(&spec) {
            return Err(format!("{n}: the params do not pin the snapshot's range and tip").into());
        }
        let fuel = preflight(
            &spec,
            source.clone(),
            &reg,
            &cas,
            &host,
            &AttestCache::new(),
            &Default::default(),
        )
        .map_err(|e| format!("{n}: preflight refused the shard: {e:?}"))?;
        let size = source.len();
        let clean = !personal_text(&source);
        let input = assemble_input(&spec, source, None, &[], &reg)?.bytes;
        let r = host.run(&id, &wasm, &input, &profile())?;
        if r.outcome != Outcome::Ok || r.fuel_used != fuel || r.leaves.len() != 1 {
            return Err(format!(
                "{n}: run {:?}, fuel {} vs {fuel}, {} leaves",
                r.outcome,
                r.fuel_used,
                r.leaves.len()
            )
            .into());
        }
        let leaf = &r.leaves[0];
        let row = EthChainStatsRow::try_from_slice(&OutputRow::try_from_slice(leaf)?.value)?;
        if row.number != n {
            return Err(format!("{n}: row is for {}", row.number).into());
        }
        tip = row.hash;
        if n == from {
            first_time = row.time;
        }
        last_time = row.time;
        max_fuel = max_fuel.max(fuel);
        fuel_sum += fuel;
        if row.txs >= RERUN_MIN_TXS
            && row.blob_txs > 0
            && clean
            && rerun.as_ref().is_none_or(|(s, ..)| size < *s)
        {
            rerun = Some((size, n, input, fuel, row.txs));
        }
        shards.push(EthShard {
            from: n,
            to: n,
            root: MerkleTree::from_rows(&r.leaves).root(),
        });
        let date = utc_date(row.time);
        match days.last_mut() {
            Some((d, _, t, leaves)) if *d >= date => {
                *t = n;
                leaves.push(hex0x(leaf));
            }
            _ => days.push((date, n, n, vec![hex0x(leaf)])),
        }
        if (n - from) % 50 == 0 || n == to {
            eprintln!(
                "{n}: fuel {fuel} ({} bp), {} txs, {:.0} s",
                fuel * 10_000 / 4_000_000_000,
                row.txs,
                started.elapsed().as_secs_f64()
            );
        }
    }
    let commitment = eth_chain_stats_commitment(&id, &tip, &shards);
    let (rerun_size, rerun_n, rerun_input, rerun_fuel, rerun_txs) =
        rerun.ok_or("no block with 100 transactions and a blob transaction")?;
    std::fs::create_dir_all(&out)?;
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &rerun_input);
    std::fs::write(out.join("eth-rerun-input.b64.txt"), b64 + "\n")?;
    let rerun_root = shards
        .iter()
        .find(|s| s.from == rerun_n)
        .map(|s| hex0x(&s.root.0))
        .ok_or("rerun shard")?;
    let m = &w.manifest;
    let doc_path = out.join("eth-chain-stats.json");
    let mut doc = json!({
        "version": 1,
        "generated_by": "cargo run --release -p onyx-source-eth --example export-eth-chain-stats -- <out dir> <window dir>",
        "rerun_command": "scripts/refresh-eth-onchain.py --rpc <your node> --to <last block> --count <blocks> --no-cross-check --fetch-only --dir <window>, then ONYX_ETH_UNCHECKED=1 cargo run --release -p onyx-source-eth --example export-eth-chain-stats -- <out dir> <window>",
        "recipe": "eth-chain-stats",
        "recipe_id": hex0x(&id.0),
        "schema": hex0x(&keccak256(onyx_source_eth::SCHEMA_ETH_CHAIN_STATS).0),
        "shard_blocks": 1,
        "from": from, "to": to,
        "first_time": first_time, "last_time": last_time,
        "tip": hex::encode(tip),
        "fetch": {
            "rpc": m["rpc"],
            "check_rpc": m["check_rpc"],
            "fetched_at": m["fetched_at"],
            "finalized_at_fetch": m["finalized_at_fetch"]["number"],
            "cross_checked_hashes": m["cross_checked"].as_array().map_or(0, Vec::len),
            "second_operator_confirmed": cross_checked(&w).is_ok(),
        },
        "days": days.iter().map(|(d, f, t, l)| json!({"date": d, "from": f, "to": t, "leaves": l})).collect::<Vec<_>>(),
        "max_fuel": max_fuel,
        "fuel_limit": 4_000_000_000u64,
        "rerun": {
            "number": rerun_n,
            "txs": rerun_txs,
            "file": "onchain/eth-rerun-input.b64.txt",
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
        "wrote {} ({} blocks, {} days; max fuel {max_fuel}, mean {}; {:.1} s)",
        doc_path.display(),
        shards.len(),
        days.len(),
        fuel_sum / shards.len() as u64,
        started.elapsed().as_secs_f64()
    );
    if std::env::var("ONYX_NO_RECOUNT").ok().as_deref() != Some("1") {
        let line = recount(&root, &doc_path, &dir)?;
        doc["python_recount"] = Value::String(line);
        std::fs::write(&doc_path, serde_json::to_string(&doc)? + "\n")?;
    }
    println!("ETH_CHAIN_RECIPE_ID={}", hex0x(&id.0));
    println!("ETH_CHAIN_FROM={from}");
    println!("ETH_CHAIN_TO={to}");
    println!("ETH_CHAIN_TIP={}", hex::encode(tip));
    println!("ETH_CHAIN_COMMITMENT={}", hex0x(&commitment.0));
    println!("ETH_CHAIN_RERUN_HEIGHT={rerun_n}");
    println!(
        "ETH_CHAIN_RERUN_INPUT={}",
        hex0x(&keccak256(&rerun_input).0)
    );
    Ok(())
}

/// Run the independent Python recount over the written file; its last line
/// is the summary the file records. Fails on any difference.
fn recount(root: &Path, doc: &Path, dir: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let o = std::process::Command::new("python3")
        .arg("-I")
        .arg(root.join("scripts/recount-eth-chain-stats.py"))
        .arg(doc)
        .arg(dir)
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
