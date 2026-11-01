//! How fast a node computes placement (docs/STORAGE-NETWORK.md "Scaling"):
//! the R holders of `SHARDS` shards over `NODES` nodes, and how many shards
//! move when one node leaves. Run in release:
//! `cargo run --release -p onyx-storage --example bench-placement -- 200000 118`.

use onyx_core::Hash32;
use onyx_storage::ids::{node_id, shard_key, NodeId};
use onyx_storage::placement::holders;
use std::time::Instant;

fn main() {
    let args: Vec<u64> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let shards = args.first().copied().unwrap_or(200_000) as u32;
    let n = args.get(1).copied().unwrap_or(118);
    let nodes: Vec<NodeId> = (0..n)
        .map(|i| node_id(&[7; 20], i, &Hash32([1; 32])))
        .collect();
    let salt = Hash32([2; 32]);
    let t = Instant::now();
    let mut first = Vec::with_capacity(shards as usize);
    for i in 0..shards {
        first.push(holders(&shard_key(&salt, i), &nodes, 3, |_| false));
    }
    let ms = t.elapsed().as_millis().max(1) as u64;
    let rest: Vec<NodeId> = nodes[1..].to_vec();
    let mut moved = 0u64;
    for (i, h) in first.iter().enumerate() {
        if h.contains(&nodes[0]) {
            let after = holders(&shard_key(&salt, i as u32), &rest, 3, |_| false);
            moved += after.iter().filter(|x| !h.contains(x)).count() as u64;
        }
    }
    // Integers only (ADR-0009 lint): rates per second from milliseconds, the
    // moved share in basis points.
    let s = u64::from(shards);
    println!(
        "shards={s} nodes={n} r=3 placement_ms={ms} shards_per_sec={} weights_per_sec={} moved_when_one_leaves={moved} ({} bp of shard copies)",
        s * 1000 / ms,
        s * n * 1000 / ms,
        moved * 10_000 / (3 * s),
    );
}
