//! Bulk datasets (ADR-0059) on every trust boundary: a publisher's manifest
//! and shard files (uploads), a catalog (uploads and peers), a node's catalog
//! files answer walked by the history check (amendment 3, with tombstones,
//! amendment 4), a signed report, a rows answer checked by a client, and a
//! storage node's learn answer (amendment 4 item 5a slice 2a: the API node's
//! catalog header, a leaf proof and its report, THREATS FUZZ-8), as Borsh
//! parts and as the JSON the leaf route answers, through the storage node's
//! own check (`onyx_api::bulk_serve::check_leaf_answer`, review FUZZ-8-R1).
//! The first byte picks the parser. None may panic or allocate without bound (THREATS
//! FUZZ-7: no target covered `onyx-bulk` before 2026-11-08).
#![no_main]

use borsh::BorshDeserialize;
use libfuzzer_sys::fuzz_target;
use onyx_bulk::catalog::{Catalog, CatalogFiles, CatalogHeader, LeafProof, Walker};
use onyx_bulk::manifest::Manifest;
use onyx_bulk::shard::check_shard;
use onyx_core::{SignedReport, SourceId};

fuzz_target!(|data: &[u8]| {
    let Some((&sel, b)) = data.split_first() else {
        return;
    };
    match sel % 8 {
        0 => {
            // The header's Borsh is written by hand (ADR-0059 amendment 5:
            // `earlier` only in format 2): every manifest that decodes,
            // valid or not, encodes back to exactly its bytes.
            if let Ok(m) = Manifest::try_from_slice(b) {
                assert_eq!(
                    m.to_bytes(),
                    b,
                    "a decoded manifest re-encodes to its bytes"
                );
            }
            if let Ok(m) = Manifest::from_bytes(b) {
                let _ = m.header.root();
                let _ = m.header.validate();
                for i in 0..m.header.shard_count.min(4) {
                    let _ = (m.header.schema_of(i), m.header.schema_index(i));
                }
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
        5 => {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(b) {
                let _ = onyx_bulk::wire::check_rows_answer(&v, None, None);
                let _ = onyx_bulk::wire::check_rows_answer(&v, Some(0), Some(10));
            }
        }
        6 => {
            // The parts of a leaf answer, past the JSON layer: the header proves the
            // leaf and its neighbours, the report signs the header.
            if let Ok((h, lp, r)) = <(CatalogHeader, LeafProof, SignedReport)>::try_from_slice(b) {
                let _ = h.proves(&lp);
                let _ = (lp.leaf.is_tombstone(), lp.leaf.row(), h.root());
                let _ = onyx_bulk::catalog::verify(&h, &r);
            }
        }
        _ => {
            // The whole answer as a storage node gets it: JSON, hex fields, and every
            // check it runs before any chain read (the dataset it asked for is the
            // one the answer names, so a valid seed passes them all).
            let dataset = serde_json::from_slice::<serde_json::Value>(b)
                .ok()
                .and_then(|v| v["dataset"].as_str().map(str::to_owned))
                .unwrap_or_else(|| "synthetic.metrics".into());
            let _ = onyx_api::bulk_serve::check_leaf_answer("example.publisher", &dataset, b);
        }
    }
});
