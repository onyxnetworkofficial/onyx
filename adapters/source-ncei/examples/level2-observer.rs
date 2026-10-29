//! One independent level-2 observer for this source (ROADMAP B6): the shared
//! observer [`onyx_level2::live::observer`] with the NOAA NCEI daily high temperature. Flags as
//! `onyx-source-fiscaldata`'s `examples/level2-observer.rs` documents;
//! `scripts/level2-sepolia.sh` runs it on Base Sepolia. `ONYX_L2_SERIES` picks another series
//! (north-star loop NL17, `onyx_source_ncei::series`).

use onyx_source_ncei::round::from_env;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match from_env() {
        Ok(src) => std::process::exit(onyx_level2::live::observer(src.as_ref(), &args)),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
}
