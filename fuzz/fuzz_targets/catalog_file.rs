//! The operator's catalog file (`onyx-node --catalog`): JSON with Borsh
//! specs and rights in hex, end headers, markets and prices. Operator-local,
//! but a corrupt or hand-edited file must be refused with an error, never
//! panic the node.
#![no_main]

use libfuzzer_sys::fuzz_target;
use onyx_node::catalog_file::CatalogFile;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(f) = CatalogFile::from_json(text) else {
        return;
    };
    for now in [0, 1_700_000_000, 2_000_000_000, u64::MAX] {
        let _ = f.to_catalog_at(now);
        let _ = f.holdings_at(now);
    }
    let _ = f.params_blobs();
});
