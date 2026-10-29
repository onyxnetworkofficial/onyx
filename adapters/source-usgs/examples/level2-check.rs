//! A client's check of a served level-2 value of this source (ROADMAP B6):
//! [`onyx_level2::live::client_check`] with this adapter's own template.
//!
//! `cargo run -p onyx-source-usgs --example level2-check --
//!   --url BASE --subject S --rpc URL --registry ADDR [--cast PATH] [--ladder-state DIR]`

use onyx_source_usgs::round::from_env;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    onyx_level2::live::client_check(from_env()?.as_ref(), &args)
}
