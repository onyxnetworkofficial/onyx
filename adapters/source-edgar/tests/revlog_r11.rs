//! Review R11-2 (EDGAR): `--tag-store` is a mode of its own and tags every
//! company's results store, or none: a missing store (the binary skipped
//! it silently before) or any mismatch with the chain refuses them all.
//! The chain check itself is tested on the Treasury runner
//! (`onyx-source-fiscaldata`, `revlog_r8::r11_2_*`).

use onyx_api::observations::{ObservationStore, StoreTag};
use onyx_coordinator::observe::{RegistryDeployment, SimulatedRegistry};
use onyx_core::Hash32;
use onyx_source_edgar::round::{store, tag_stores};
use onyx_source_edgar::{template, COMPANIES};
use serde_json::Value;

#[test]
fn r11_2_tag_stores_tags_every_company_or_none() {
    let dir = std::env::temp_dir().join(format!("onyx-r11-edgar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let d = RegistryDeployment {
        chain_id: 31337,
        registry: [0x84; 20],
        deploy_block: Hash32([1; 32]),
    };
    let mut chain = SimulatedRegistry::new();
    chain.deployment = Some(d);
    let path = |i: usize| dir.join(format!("{}.json", COMPANIES[i].key));
    let empty = |i: usize| {
        ObservationStore::to_json_with_attempts(&template(&COMPANIES[i]), &[], &[], None)
    };
    let read = |i: usize| -> Value {
        serde_json::from_str(&std::fs::read_to_string(path(i)).expect("read")).expect("json")
    };
    // No stores: refused (before review R11 this was a silent no-op).
    let e = tag_stores(&dir, &chain, &store()).expect_err("missing");
    assert!(e.contains("no such results store"), "{e}");
    // One store missing: none tagged.
    std::fs::write(path(0), empty(0).to_string()).expect("write");
    let e = tag_stores(&dir, &chain, &store()).expect_err("one missing");
    assert!(e.contains(COMPANIES[1].key), "{e}");
    assert_eq!(read(0), empty(0), "nothing tagged");
    // No chain: refused.
    std::fs::write(path(1), empty(1).to_string()).expect("write");
    let e = tag_stores(&dir, &SimulatedRegistry::new(), &store()).expect_err("no chain");
    assert!(e.contains("--rpc"), "{e}");
    // Every store present and matching the chain: all tagged.
    let n = tag_stores(&dir, &chain, &store()).expect("tagged");
    assert_eq!(n, vec![(COMPANIES[0].key, 0), (COMPANIES[1].key, 0)]);
    for i in 0..COMPANIES.len() {
        assert_eq!(
            ObservationStore::tag_of(&read(i)).expect("tag"),
            Some(StoreTag::of(d))
        );
    }
    // Review R12 (REVLOG-R12-3): tagged again, nothing changes: the files
    // are not rewritten (compact JSON stays compact).
    let compact: Vec<String> = (0..COMPANIES.len()).map(|i| read(i).to_string()).collect();
    for (i, c) in compact.iter().enumerate() {
        std::fs::write(path(i), c).expect("write");
    }
    tag_stores(&dir, &chain, &store()).expect("tagged again");
    for (i, c) in compact.iter().enumerate() {
        assert_eq!(&std::fs::read_to_string(path(i)).expect("read"), c);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
