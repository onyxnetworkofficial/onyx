//! `onyx-node-btc`: `onyx-node` with the Bitcoin serving checks (ADR-0034
//! Amendment 1). With `ONYX_RESERVE_GATE=<config.json>` it serves
//! `btc.entity-reserve` through `onyx_reserve_serve::ReserveGate`; without
//! it, it is plain `onyx-node`, which refuses every shard naming that check.

use onyx_api::gate::ServingGate;
use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    let mut gates: Vec<Arc<dyn ServingGate>> = Vec::new();
    if let Some(p) = std::env::var_os("ONYX_RESERVE_GATE") {
        match onyx_reserve_serve::ReserveGate::from_config(std::path::Path::new(&p)) {
            Ok(g) => gates.push(Arc::new(g)),
            Err(e) => {
                eprintln!("onyx-node-btc: ONYX_RESERVE_GATE: {e}");
                return ExitCode::from(2);
            }
        }
    }
    onyx_node::app::main_with(gates)
}
