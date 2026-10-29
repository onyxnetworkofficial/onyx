//! `onyx-node`: the onyx-api service (docs/ops/deployment.md), without
//! sector serving checks ([`onyx_node::app`]).
//!
//! ```text
//! onyx-node                 # serve, configured from ONYX_* variables
//! onyx-node --check-config  # load and check config, manifest, catalog; no network
//! onyx-node healthcheck     # probe GET /v1/health on ONYX_LISTEN (container health check)
//! ```

fn main() -> std::process::ExitCode {
    onyx_node::app::main_with(Vec::new())
}
