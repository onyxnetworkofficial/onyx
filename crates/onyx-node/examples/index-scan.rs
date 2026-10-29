//! Read-only scan of a settlement deployment with the chain indexer
//! (ADR-0033): every `Finalized` event from the deploy block to the node's
//! `finalized` block, and the contract's own `getFinalizedRoot` for each.
//! Holds nothing, so it serves nothing; it sends no transaction.
//!
//! ```text
//! cargo run -p onyx-node --example index-scan -- \
//!   --rpc https://sepolia.base.org --chain-id 84532 \
//!   --settlement 0x7d2576ee99c98ce8de353eff23708c26742a7116 --from 48037567 [--span 200]
//! ```

use onyx_api::catalog::Catalog;
use onyx_api::indexer::{Holdings, Indexer, IndexerConfig};
use onyx_api::roots::{decode_finalized_root, get_finalized_root_calldata, HttpJsonRpc, JsonRpc};
use onyx_api::x402::HyperPost;
use onyx_coordinator::cas::{Cas, MemCas};
use serde_json::json;
use std::sync::{Arc, RwLock};
use std::time::Duration;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rpc_url = arg(&args, "--rpc").ok_or("--rpc URL")?;
    let chain_id: u64 = arg(&args, "--chain-id").ok_or("--chain-id N")?.parse()?;
    let settlement = arg(&args, "--settlement").ok_or("--settlement 0x...")?;
    let from: u64 = arg(&args, "--from").ok_or("--from BLOCK")?.parse()?;
    let span: u64 = arg(&args, "--span").map_or(Ok(200), |s| s.parse())?;
    let mut contract = [0u8; 20];
    contract.copy_from_slice(&hex::decode(settlement.trim_start_matches("0x"))?);
    let rpc = || {
        HttpJsonRpc::new(
            rpc_url.clone(),
            HyperPost::for_logs(Duration::from_secs(30)),
        )
    };
    let cfg = IndexerConfig {
        span,
        ..IndexerConfig::new(chain_id, contract, from)
    };
    let idx = Indexer::open(
        cfg,
        rpc(),
        Holdings::new(),
        Arc::new(MemCas::new()) as Arc<dyn Cas + Send + Sync>,
        None,
    )?;
    idx.check_deploy_block().await?;
    println!(
        "deploy block {from}: no code at {}, code at {from}",
        from.saturating_sub(1)
    );
    let catalog = RwLock::new(Catalog::new());
    let (mut ranges, mut splits, mut head) = (0, 0, 0);
    // Polls until the scan reached the finalized block this node reports.
    for _ in 0..20 {
        match idx.poll(&catalog).await {
            Ok(r) => {
                ranges += r.ranges;
                splits += r.splits;
                head = r.head;
                if idx.next_block() > r.head {
                    break;
                }
            }
            Err(e) => eprintln!("poll: {e}; again"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!(
        "scanned blocks {from}..{} (finalized head {head}) in {ranges} eth_getLogs calls, {splits} split(s)",
        idx.next_block().saturating_sub(1)
    );
    let node = rpc();
    for e in idx.events() {
        let ret = node
            .call(
                "eth_call",
                json!([
                    {"to": format!("0x{}", hex::encode(contract)),
                     "data": format!("0x{}", hex::encode(get_finalized_root_calldata(e.round, &e.shard)))},
                    format!("0x{head:x}")
                ]),
            )
            .await?;
        let data = hex::decode(ret.as_str().unwrap_or("0x").trim_start_matches("0x"))?;
        let view = decode_finalized_root(&data)?;
        let agrees = view.is_some_and(|(r, b, _)| (r, b) == (e.root, e.binding));
        println!(
            "Finalized(round {}, shard {}) at block {} log {} tx {}\n  event root {} binding {}\n  getFinalizedRoot at {head}: {}",
            e.round,
            e.shard,
            e.block,
            e.log_index,
            e.tx.as_deref().unwrap_or("?"),
            e.root,
            e.binding,
            match view {
                Some((r, b, at)) => format!(
                    "root {r} binding {b} finalizedAt {at} ({})",
                    if agrees { "agrees with the event" } else { "DIFFERS from the event" }
                ),
                None => "not final".into(),
            }
        );
    }
    for k in idx.known() {
        println!(
            "round {} shard {}: {:?} (this scan holds no data, so nothing is served)",
            k.round, k.shard, k.status
        );
    }
    Ok(())
}
