//! The live level-2 coordinator for this source (ROADMAP B6): the shared
//! runner [`onyx_level2::live::coordinator`] with the NOAA NCEI daily high temperature. Flags as
//! `onyx-source-fiscaldata`'s `examples/level2-coordinator.rs` documents;
//! `scripts/level2-sepolia.sh` runs it on Base Sepolia. `ONYX_L2_SERIES` picks another series
//! (north-star loop NL17, `onyx_source_ncei::series`).

use onyx_source_ncei::round::from_env;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    onyx_level2::live::coordinator(from_env()?.as_ref(), &args)
}
