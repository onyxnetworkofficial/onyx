//! The chain indexer's state file (cursor, events, an interrupted rescan):
//! operator-local, written atomically, but a corrupt or edited file must be
//! refused with an error at open, never panic.
#![no_main]

use libfuzzer_sys::fuzz_target;
use onyx_api::indexer::{HeldShard, Holdings, Indexer, IndexerConfig};
use onyx_coordinator::cas::MemCas;
use onyx_fuzz_common::{held_spec, MethodRpc, CHAIN, CONTRACT};
use std::sync::Arc;

fuzz_target!(|data: &[u8]| {
    let dir = std::env::temp_dir().join(format!("onyx-fuzz-indexer-{}", std::process::id()));
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("indexer-state.json");
    if std::fs::write(&path, data).is_err() {
        return;
    }
    let (spec, rights) = held_spec();
    let mut held = Holdings::new();
    if let Some(h) = HeldShard::from_spec(&spec, "test.flow", rights) {
        let _ = held.insert(h);
    }
    let mut cfg = IndexerConfig::new(CHAIN, CONTRACT, 100);
    cfg.rescan_every = 1;
    let rpc = MethodRpc::new(&serde_json::Value::Null);
    if let Ok(idx) = Indexer::open(cfg, rpc, held, Arc::new(MemCas::new()), Some(path)) {
        let _ = idx.known();
        let _ = idx.events();
        let _ = idx.next_block();
        use onyx_api::indexer::IndexerHealth;
        let _ = idx.indexer_health();
    }
});
