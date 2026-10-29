//! The live level-2 coordinator for this source (ROADMAP B6): the shared
//! runner [`onyx_level2::live::coordinator`] with the USGS earthquake count. Flags as
//! `onyx-source-fiscaldata`'s `examples/level2-coordinator.rs` documents;
//! `scripts/level2-sepolia.sh` runs it on Base Sepolia.

use onyx_source_usgs::round::from_env;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    onyx_level2::live::coordinator(from_env()?.as_ref(), &args)
}
