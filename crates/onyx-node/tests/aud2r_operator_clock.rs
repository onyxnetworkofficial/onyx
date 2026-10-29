//! AUD-2R-F2: the price clock no longer trusts the catalog file's data time.
//! Before the fix, `snapshotUnix` was taken unchecked, so an operator who set
//! it in the future made a round over old blocks, finalized late, priced as
//! fresh. Now a block shard's time is read from its end header (which must
//! hash to the pinned tip) and a stated `snapshotUnix` must equal it.
use onyx_core::{keccak256, ChainId, DataRights, Hash32, ShardSpec, Snapshot, SourceId, SourceRef};
use onyx_node::catalog_file::{CatalogFile, Pricing, ShardEntry, CATALOG_VERSION};

fn fixture_end() -> (Vec<u8>, Hash32) {
    let p = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/btc/blocks-921080-921082.json"
    );
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p).expect("fixture")).expect("json");
    let b = v["blocks"]
        .as_array()
        .and_then(|a| a.last())
        .expect("end block");
    let h = hex::decode(b["header"].as_str().expect("header")).expect("hex");
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&hex::decode(b["hash"].as_str().expect("hash")).expect("hash hex"));
    (h, Hash32(hash))
}

#[test]
fn a_future_snapshot_unix_is_refused_and_the_header_time_prices() {
    let (header, tip) = fixture_end();
    let rights = DataRights::public_chain(SourceId("chain.test".into()));
    let spec = ShardSpec {
        recipe: None,
        runtime: None,
        label_set: None,
        source: SourceRef {
            source: SourceId("chain.test".into()),
            snapshot: Snapshot::Block {
                chain: ChainId("btc.mainnet".into()),
                height: 921_082,
                block_hash: tip,
            },
        },
        schema: keccak256(b"schema"),
        rights_hash: rights.hash(),
        params: vec![],
        registry_root: keccak256(b"r"),
    };
    let mut entry =
        ShardEntry::new(1, "test.flow", &spec, &rights, Some(1), Some(&header)).expect("entry");
    let real = entry.snapshot_unix.expect("time from the header");
    entry.snapshot_unix = Some(u64::MAX);
    let file = |e: ShardEntry| CatalogFile {
        version: CATALOG_VERSION,
        shards: vec![e],
        held: vec![],
        markets: vec![],
        default_pricing: Some(Pricing {
            base_price_micro_usdc: 100,
            fresh_window_secs: 3_600,
        }),
        pricing: Default::default(),
        default_dataset_pricing: None,
        dataset_pricing: Default::default(),
    };
    let late = real + 30 * 86_400;
    assert!(file(entry.clone())
        .to_catalog_at(late)
        .expect_err("an operator's u64::MAX is refused")
        .contains("disagrees"));
    // Without the operator's number, the header's time prices: finalized 30
    // days after the data, the shard is old (free), not fresh.
    entry.snapshot_unix = None;
    let c = file(entry).to_catalog_at(late).expect("catalog");
    let meta = c.shard(1, &spec.shard_id()).expect("shard");
    assert_eq!(meta.clock_start(late), real + 86_400);
    assert_eq!(c.price(meta, late, late + 10, 1), 0);
}
