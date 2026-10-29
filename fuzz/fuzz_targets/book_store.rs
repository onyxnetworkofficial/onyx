//! Level-2 state files an operator's server loads: the coordinator's
//! non-value book (`ObservationResults::absorb_non_value_book`, with its
//! Borsh announced draws and unfinished rounds) and the query API's results
//! store (`ObservationStore::from_json`, every round re-checked against the
//! pinned template). Corruption must be an error, never a panic.
#![no_main]

use libfuzzer_sys::fuzz_target;
use onyx_api::observations::ObservationStore;
use onyx_fuzz_common::level2;

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else {
        return;
    };
    let Ok(text) = std::str::from_utf8(rest) else {
        return;
    };
    if sel % 2 == 0 {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
            return;
        };
        let mut store = onyx_source_fiscaldata::round::store();
        if store.absorb_non_value_book(&v).is_ok() {
            // What was read writes back and reads again.
            let book = store.non_value_book().expect("an absorbed book writes");
            let mut again = onyx_source_fiscaldata::round::store();
            again
                .absorb_non_value_book(&book)
                .expect("a written book reads back");
        }
        let _ = ObservationStore::onchain_of(&v);
        let _ = ObservationStore::tag_of(&v);
    } else {
        for now in [0, level2::NOW, u64::MAX] {
            if let Ok(feed) = ObservationStore::from_json(text, now, &level2::pins()) {
                let _ = feed.metric();
                let _ = feed.head();
            }
        }
    }
});
