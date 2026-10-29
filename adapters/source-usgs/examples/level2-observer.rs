//! One independent level-2 observer for this source (ROADMAP B6): the shared
//! observer [`onyx_level2::live::observer`] with the USGS earthquake count. Flags as
//! `onyx-source-fiscaldata`'s `examples/level2-observer.rs` documents;
//! `scripts/level2-sepolia.sh` runs it on Base Sepolia.

use onyx_source_usgs::round::from_env;

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
