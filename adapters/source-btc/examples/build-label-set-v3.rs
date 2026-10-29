// SPDX-License-Identifier: Apache-2.0
//! Build label set v3 of `test.onyx.entities.btc` from the issuers' rosters
//! (ROADMAP P2a, ADR-0037 Amendment 1). Reproducible: the same committed
//! snapshot files give the same bytes.
//!
//! ```text
//! python3 -I scripts/build-rosters.py listings /tmp/listings.json
//! cargo run --release -p onyx-source-btc --example build-label-set-v3 -- \
//!   /tmp/listings.json fixtures/labels/test.onyx.entities.btc.v3.json
//! ```
//!
//! Not `scripts/build-label-set-v3.py` (the ADR-0031 indexer bridge, whose
//! undated output never ships). Then record the file's sha256 in
//! `fixtures/SHA256SUMS`. The test
//! `labels_v3::v3_rebuilds_byte_for_byte_from_the_committed_rosters` runs the
//! same two steps and compares bytes.

use onyx_source_btc::rosters::{build_v3, v3_to_json};
use onyx_source_btc::{dated_label_set_from_json, fixture_registry_v3};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [listings, out] = args.as_slice() else {
        return Err("usage: build-label-set-v3 <listings.json> <out.json>".into());
    };
    let built = build_v3(&std::fs::read_to_string(listings)?)?;
    built.set.validate(&fixture_registry_v3())?;
    let text = v3_to_json(&built)?;
    // The file must read back as the same set (evidence hashes included).
    if dated_label_set_from_json(&text)?.content_hash() != built.set.content_hash() {
        return Err("the written file does not read back as the built set".into());
    }
    std::fs::write(out, &text)?;
    for r in &built.report {
        let members = built
            .set
            .members
            .iter()
            .filter(|m| m.label.entity == r.entity)
            .count();
        eprintln!(
            "{:<10} snapshots {:>3} addresses {:>5} memberships {:>5} rows signed {:>6} published {:>6} unverified-signature {:>4} message-not-naming-issuer {:>3} script-mismatch {:>3}",
            r.key, r.snapshots, r.addresses, members, r.signed_rows, r.published_rows,
            r.unverified_signature_rows, r.message_not_naming_issuer_rows, r.script_mismatch_rows
        );
    }
    for d in &built.dropped {
        eprintln!("dropped {} ({:?}, {:?})", d.address, d.cause, d.entities);
    }
    let open = built.set.members.iter().filter(|m| m.to.is_none()).count();
    eprintln!(
        "wrote {out}: {} memberships ({open} open at {}), {} snapshots, content hash 0x{}",
        built.set.members.len(),
        built.set.published,
        built.records.len(),
        hex::encode(built.set.content_hash().0)
    );
    Ok(())
}
