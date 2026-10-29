//! Salted label rows, one buyer row at a time (ROADMAP D30, ADR-0037 amendment 2, pre-public loop
//! PP3).
//!
//! `GET /v1/labels/{set}/{address}`: the memberships of `address` in the format-3 set whose content
//! hash is `set`, each as a [`onyx_labels::salted::SaltedAnswer`] with its own salt and proof,
//! never a sibling's salt and never a plain (format-2) row. The client checks every answer
//! against the content hash it pinned itself (`onyx_labels::salted::verify_salted_answer`; the
//! browser verifier's `check_salted_answer`). A 404 `no_label_row` is the server's word, not a
//! proof that the address is unlisted.
//!
//! Testnet prototype: a store refuses any set whose name does not start with `test.`, so no real
//! label set (whose rights are not cleared: OKX, Bitwise, the v3 issuers) can be served from here
//! until its rights review and the paid path (x402, ROADMAP D31) exist. Free, 30 answers per
//! client a minute, `no-store` (a buyer's questions stay out of shared caches).

use crate::cache::Caches;
use crate::{bad_request, rate_scopes, ApiError, ClientKey};
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use onyx_core::Hash32;
use onyx_labels::dated::DatedLabelSet;
use onyx_labels::salted::{json, SaltedDatedSet};
use serde_json::{json as j, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Free answers per client a minute.
pub const FREE_PER_MINUTE: u32 = 30;
/// Most memberships one served set may hold.
pub const MAX_MEMBERS: usize = 1_000_000;
/// The only set names a node serves until label rights are cleared.
pub const TEST_PREFIX: &str = "test.";

/// Why a set file is refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// Not the expected JSON.
    #[error("salted set file: {0}")]
    Format(String),
    /// A real (non-test) set: refused until its rights are cleared.
    #[error("salted set {0}: only test.* sets are served until label rights are cleared")]
    NotTest(String),
    /// Members out of order (the address lookup needs `(address, from)` order).
    #[error("salted set {0}: memberships are not strictly ascending by address and start")]
    NotSorted(String),
    /// Too many members.
    #[error("salted set {0}: more than {MAX_MEMBERS} memberships")]
    TooLarge(String),
    /// Two files name the same content hash.
    #[error("salted set {0} is loaded twice")]
    Duplicate(String),
}

/// The salted sets a node serves, by content hash.
pub struct SaltedLabelStore {
    sets: BTreeMap<Hash32, SaltedDatedSet>,
    limits: Caches,
}

impl SaltedLabelStore {
    /// A store of `files`, each `{"set": "0x<borsh DatedLabelSet>", "secret": "0x<32 bytes>"}`.
    /// The set's own coordinator tools validated its labels; here only the serving rules are
    /// checked (name, order, size).
    pub fn from_files(files: &[Value]) -> Result<Self, StoreError> {
        let mut sets = BTreeMap::new();
        for f in files {
            let bad = |m: &str| StoreError::Format(m.to_string());
            let o = f.as_object().ok_or(bad("not an object"))?;
            if o.len() != 2 {
                return Err(bad("exactly `set` and `secret`"));
            }
            let hexf = |k: &str| -> Result<Vec<u8>, StoreError> {
                let s = f[k]
                    .as_str()
                    .and_then(|s| s.strip_prefix("0x"))
                    .ok_or(bad("hex fields are 0x strings"))?;
                hex::decode(s).map_err(|e| StoreError::Format(e.to_string()))
            };
            let set: DatedLabelSet =
                borsh::from_slice(&hexf("set")?).map_err(|e| StoreError::Format(e.to_string()))?;
            let secret: [u8; 32] = hexf("secret")?
                .try_into()
                .map_err(|_| bad("`secret` is 32 bytes"))?;
            if !set.name.starts_with(TEST_PREFIX) {
                return Err(StoreError::NotTest(set.name));
            }
            if set.members.len() > MAX_MEMBERS {
                return Err(StoreError::TooLarge(set.name));
            }
            let sorted = set.members.windows(2).all(|w| {
                (w[0].label.address.as_str(), w[0].from) < (w[1].label.address.as_str(), w[1].from)
            });
            if !sorted {
                return Err(StoreError::NotSorted(set.name));
            }
            let name = set.name.clone();
            let s = SaltedDatedSet::new(set, &Hash32(secret));
            if sets.insert(s.content_hash(), s).is_some() {
                return Err(StoreError::Duplicate(name));
            }
        }
        Ok(Self {
            sets,
            limits: Caches::new(1, 1),
        })
    }

    /// The content hashes served.
    pub fn content_hashes(&self) -> Vec<Hash32> {
        self.sets.keys().copied().collect()
    }

    /// The answer body for `address` in the set `set`.
    pub fn answer(&self, set: &Hash32, address: &str) -> Result<Value, ApiError> {
        let s = self.sets.get(set).ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_label_set",
                "no salted label set with this content hash",
            )
        })?;
        let rows = s.rows_of(address);
        if rows.is_empty() {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "no_label_row",
                "this set lists no membership for this address (the server's word, not a proof)",
            ));
        }
        let answers: Vec<Value> = rows
            .iter()
            .filter_map(|&i| s.answer(i))
            .map(|a| json::to_json(&a))
            .collect();
        Ok(j!({ "set": set.to_string(), "address": address, "answers": answers }))
    }
}

/// The salted-label route, to merge into [`crate::router_with`]'s `extra`.
pub fn routes(store: Arc<SaltedLabelStore>) -> Router {
    Router::new()
        .route("/v1/labels/{set}/{address}", get(label))
        .with_state(store)
}

async fn label(
    State(store): State<Arc<SaltedLabelStore>>,
    client: Option<Extension<ClientKey>>,
    path: Result<Path<(String, String)>, PathRejection>,
    RawQuery(q): RawQuery,
) -> Response {
    let client = client.map_or_else(|| "local".to_string(), |Extension(c)| c.0);
    reply(&store, &client, path, q).unwrap_or_else(IntoResponse::into_response)
}

fn reply(
    store: &SaltedLabelStore,
    client: &str,
    path: Result<Path<(String, String)>, PathRejection>,
    q: Option<String>,
) -> Result<Response, ApiError> {
    let Path((set, address)) = path.map_err(|e| bad_request(e.body_text()))?;
    if q.is_some_and(|q| !q.is_empty()) {
        return Err(bad_request(
            "GET /v1/labels/{set}/{address} takes no query parameters",
        ));
    }
    let (set, canonical) = crate::parse_hash(&set, "set")?;
    if !canonical {
        return Err(bad_request("set must be 0x and 64 lowercase hex digits"));
    }
    if address.is_empty() || address.len() > 128 || !address.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(bad_request(
            "address must be 1 to 128 visible ASCII characters",
        ));
    }
    let scopes: Vec<(String, u32)> = rate_scopes(client, FREE_PER_MINUTE)
        .into_iter()
        .map(|(k, n)| (format!("labels:{k}"), n))
        .collect();
    if !store.limits.take_attempts(&scopes) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many label rows from this address",
        )
        .with_header("retry-after", (60 / FREE_PER_MINUTE).max(1).to_string()));
    }
    let body = store.answer(&set, &address)?;
    let mut r = (StatusCode::OK, Json(body)).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(r)
}

#[cfg(test)]
#[path = "labels_serve_tests.rs"]
mod tests;
