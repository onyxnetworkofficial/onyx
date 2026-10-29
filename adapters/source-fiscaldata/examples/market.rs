//! Futures markets on a future Treasury value, live on Base Sepolia
//! (NORTH-STAR NS11): [`onyx_level2::markets::run`] with this adapter's own
//! template. Values and edges are mantissas at the feed's scale (cents).
//!
//! `cargo run -p onyx-source-fiscaldata --example market -- open|bet|settle|status
//!   --rpc URL --markets FILE ...` (`scripts/markets-sepolia.sh` runs it).

use onyx_source_fiscaldata::round::FiscalData;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    onyx_level2::markets::run(&FiscalData, &args)
}
