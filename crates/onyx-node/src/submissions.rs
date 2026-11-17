//! Optional intake routes, always merged under `router_with` by the node (ADR-0039).
use axum::Router;
use onyx_api::submit::{HttpsFetch, SubmitLimits, SubmitStore};
use std::path::Path;
use std::sync::Arc;

/// Open the explicitly configured ledger. The parent directory must already exist and be
/// private to the operator. Unset is an empty router; a bad path fails startup.
pub fn routes(path: Option<&Path>) -> std::io::Result<Router> {
    match path {
        None => Ok(Router::new()),
        Some(path) => {
            let store = SubmitStore::open(
                path,
                Some(Arc::new(HttpsFetch::new())),
                SubmitLimits::default(),
            )?;
            // ROADMAP S5: one warning line at start when the journal is near its recorded cap.
            store.warn_if_near_cap();
            Ok(onyx_api::submit::routes(Arc::new(store)))
        }
    }
}
