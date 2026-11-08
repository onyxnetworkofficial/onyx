//! Bulk datasets (ADR-0059) on every trust boundary: a publisher's manifest
//! and shard files (uploads), a catalog (uploads and peers), a node's catalog
//! files answer walked by the history check (amendment 3, with tombstones,
//! amendment 4), a signed report, and a rows answer checked by a client. The
//! first byte picks the parser. None may panic or allocate without bound
//! (THREATS FUZZ-6: no target covered `onyx-bulk` before 2026-11-08).
#![no_main]

use borsh::BorshDeserialize;
use libfuzzer_sys::fuzz_target;
use onyx_bulk::catalog::{Catalog, CatalogFiles, Walker};
use onyx_bulk::manifest::Manifest;
use onyx_bulk::shard::check_shard;
use onyx_core::{SignedReport, SourceId};

fuzz_target!(|data: &[u8]| {
    let Some((&sel, b)) = data.split_first() else {
        return;
    };
    match sel % 6 {
        0 => {
            if let Ok(m) = Manifest::from_bytes(b) {
                let _ = m.header.root();
                let _ = m.header.validate();
            }
        }
        1 => {
            let schema = onyx_fuzz_common::bulk_schema();
            if let Ok(c) = check_shard(&schema, b) {
                let _ = c.summary.root;
            }
        }
        2 => {
            if let Ok(c) = Catalog::try_from_slice(b) {
                if c.validate().is_ok() {
                    for i in 0..c.leaves.len().min(4) {
                        if let Some(lp) = c.leaf_proof(i) {
                            let _ = c.header.proves(&lp);
                        }
                    }
                }
            }
        }
        3 => {
            if let Ok(f) = CatalogFiles::try_from_slice(b) {
                let mut w = Walker::new(
                    SourceId("example.publisher".into()),
                    "synthetic.metrics".into(),
                );
                for file in &f.files {
                    if w.take(file).is_err() {
                        break;
                    }
                }
                let _ = (w.entered(), w.removed());
            }
        }
        4 => {
            if let Ok(r) = SignedReport::try_from_slice(b) {
                let _ = (r.hash(), r.payload.digest());
            }
        }
        _ => {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(b) {
                let _ = onyx_bulk::wire::check_rows_answer(&v, None, None);
                let _ = onyx_bulk::wire::check_rows_answer(&v, Some(0), Some(10));
            }
        }
    }
});
