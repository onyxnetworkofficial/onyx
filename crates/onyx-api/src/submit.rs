//! Data submissions (ROADMAP S1, ADR-0038, docs/spec/submissions.md).
//!
//! - `POST /v1/submit/check`: checks a submission ([`onyx_intake::Submission`]),
//!   classifies it ([`onyx_intake::classify`]) and, when the fetch policy
//!   allows it (a rights-cleared host on the level-2 route), pulls the endpoint
//!   once through a [`PageFetch`] and returns a preview of the value and the
//!   body's Keccak-256. Nothing is recorded.
//! - `POST /v1/submit`: records an accepted or held submission in an
//!   append-only journal ([`crate::journal`]) as "pending review" (or "held
//!   for rights review") and returns its id. A refused one is not recorded.
//! - `GET /v1/submit/{id}`: that submission's status only, plus the observers' public evidence
//!   once an approved submission was fed ([`feed`]). No listing.
//!
//! Limits: a body cap ([`MAX_BODY`]), per-client budgets per minute and per
//! UTC day on both POSTs (an IPv6 /48 shares one client's budget), global
//! budgets on submissions (per minute and per day) and on pulls, and a cap on
//! recorded submissions ([`SubmitLimits::max_recorded`]). The same content
//! sent twice is two entries; no answer says whether it was seen before. The journal keeps the submission, its
//! route, hash and receipt time; never the client's address (rate limits are
//! in memory only). Nothing is published from here: a reviewer reads the
//! journal.
//!
//! The real transport, [`HttpsFetch`] (feature `http-client`), resolves the
//! host itself and refuses every address that is not public
//! ([`onyx_intake::is_public_ip`]: private, loopback, link-local, metadata),
//! connects only to the addresses it checked, speaks https only, follows no
//! redirect, reads at most [`onyx_intake::MAX_BODY_BYTES`] and gives up after
//! [`onyx_intake::FETCH_TIMEOUT_SECS`]. Tests use fixture fetchers.

use crate::cache::Caches;
use crate::journal::Binding;
#[cfg(test)]
use crate::journal::Journal;

#[path = "submit_feed.rs"]
pub mod feed;
#[path = "submit_review.rs"]
pub mod review;
use crate::x402::BoxFuture;
use crate::{rate_scopes, ApiError, ClientKey};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use onyx_core::keccak256;
use onyx_intake::{fetch_policy, preview, FetchPolicy, Outcome, Route, Submission, CLEARED_HOSTS};
use review::ReviewError;
use serde_json::json;
#[cfg(test)]
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path as FsPath;
use std::sync::{Arc, Mutex};

/// Largest request body, in bytes, including JSON escaping of an 8 KiB sample.
pub const MAX_BODY: usize = 64 * 1024;
/// Maximum ledger workers, including work whose HTTP handler was cancelled.
pub const MAX_LEDGER_WORKERS: usize = 16;
/// The journal's kind.
pub const JOURNAL_KIND: &str = "onyx-submissions";
/// The journal's binding: submissions belong to no chain or contract.
pub const BINDING: Binding = Binding {
    chain_id: 0,
    contract: [0; 20],
};
/// Domain tag of a submission id.
pub const ID_TAG: &[u8] = b"onyx-submission-id:v1";
/// Status of a recorded submission on an accepted route.
pub const PENDING_REVIEW: &str = "pending_review";
/// Status of a recorded submission held for a rights review.
pub const HELD_RIGHTS_REVIEW: &str = "held_rights_review";
/// Status of a recorded keyed-source submission (held for a rights review of
/// the source's terms; the key never reaches the server).
pub const HELD_KEYED_SOURCE_REVIEW: &str = "held_keyed_source_review";

/// Why a pull failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// The host resolved to an address that is not public (or to none).
    #[error("refused: {0}")]
    Refused(String),
    /// The source answered with a redirect; redirects are never followed.
    #[error("the source answered {0} (a redirect); redirects are not followed")]
    Redirect(u16),
    /// Any other non-2xx status.
    #[error("the source answered {0}")]
    Status(u16),
    /// The body is over the cap.
    #[error("the body is over {0} bytes")]
    TooLarge(usize),
    /// The deadline passed.
    #[error("no answer within {0} s")]
    Timeout(u64),
    /// Connection, TLS or protocol failure.
    #[error("unreachable: {0}")]
    Unreachable(String),
}

impl FetchError {
    /// The stable code.
    pub fn code(&self) -> &'static str {
        match self {
            FetchError::Refused(_) => "address_refused",
            FetchError::Redirect(_) => "redirect_refused",
            FetchError::Status(_) => "source_status",
            FetchError::TooLarge(_) => "body_too_large",
            FetchError::Timeout(_) => "timeout",
            FetchError::Unreachable(_) => "unreachable",
        }
    }
}

/// How an endpoint is pulled. `host` is the checked, lower-case host of `url`.
pub trait PageFetch: Send + Sync {
    /// GET `url` once.
    fn get<'a>(&'a self, url: &'a str, host: &'a str)
        -> BoxFuture<'a, Result<Vec<u8>, FetchError>>;
}

/// The status of a status code: 2xx is fine, 3xx a refused redirect, the rest
/// an error.
pub fn check_status(status: u16) -> Result<(), FetchError> {
    match status {
        200..=299 => Ok(()),
        300..=399 => Err(FetchError::Redirect(status)),
        s => Err(FetchError::Status(s)),
    }
}

/// Budgets and caps. Per-client budgets are per minute and per UTC day, per
/// client key (an IPv4 address or an IPv6 /64); its /56 and /48 share the
/// same budget, not a larger one ([`crate::rate_scopes`], S1-R1-2). With the
/// defaults, one IPv4 address or one /48 records at most 10 a day, and the
/// server at most 500 a day: filling the 10,000 cap takes 20 days and at least
/// 50 addresses or /48s a day.
#[derive(Clone, Copy, Debug)]
pub struct SubmitLimits {
    /// `POST /v1/submit/check` per client per minute.
    pub check_per_minute: u32,
    /// `POST /v1/submit` per client per minute.
    pub submit_per_minute: u32,
    /// `GET /v1/submit/{id}` per client per minute.
    pub status_per_minute: u32,
    /// Submissions recorded per minute, all clients together.
    pub global_submit_per_minute: u32,
    /// Pulls per minute, all clients together.
    pub global_fetch_per_minute: u32,
    /// Most submissions the journal holds; then 503 `intake_full` until a
    /// operator migrates or archives the ledger (rejecting retains history).
    pub max_recorded: usize,
    /// `POST /v1/submit/check` per client per UTC day.
    pub check_per_day: u32,
    /// `POST /v1/submit` per client per UTC day.
    pub submit_per_day: u32,
    /// Submissions recorded per UTC day, all clients together.
    pub global_submit_per_day: u32,
}

impl Default for SubmitLimits {
    fn default() -> Self {
        SubmitLimits {
            check_per_minute: 6,
            submit_per_minute: 3,
            status_per_minute: 30,
            global_submit_per_minute: 10,
            global_fetch_per_minute: 20,
            max_recorded: 10_000,
            check_per_day: 60,
            submit_per_day: 10,
            global_submit_per_day: 500,
        }
    }
}

/// The submissions store: the journal, the id index, the budgets and the
/// fetcher.
pub struct SubmitStore {
    ledger: Mutex<review::Ledger>,
    /// The feed ledger beside the journal, read once at open ([`feed`]).
    feed: feed::FeedLedger,
    workers: Arc<tokio::sync::Semaphore>,
    budgets: Caches,
    /// Daily counts per scope key (S1-R1-2), reset at each UTC day.
    daily: Mutex<Daily>,
    fetcher: Option<Arc<dyn PageFetch>>,
    allow: Vec<String>,
    limits: SubmitLimits,
}

/// Most distinct daily keys kept; a new key past it is refused (fail closed).
pub const MAX_DAILY_KEYS: usize = 200_000;

#[derive(Default)]
struct Daily {
    day: u64,
    counts: BTreeMap<String, u32>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The id of the journal entry `seq` holding a submission with content hash
/// `hash` (hex with `0x`): `sub_` and 16 bytes of Keccak-256 in hex.
pub fn submission_id(seq: u64, hash: &str) -> String {
    let mut b = ID_TAG.to_vec();
    b.extend_from_slice(&seq.to_be_bytes());
    b.extend_from_slice(hash.as_bytes());
    format!("sub_{}", hex::encode(&keccak256(&b).0[..16]))
}

fn is_id(s: &str) -> bool {
    s.strip_prefix("sub_")
        .is_some_and(|h| h.len() == 32 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

impl SubmitStore {
    /// Open (or create) the journal at `path`. `fetcher` is the transport for
    /// check pulls (`None`: never pull; the check says so).
    pub fn open(
        path: impl AsRef<FsPath>,
        fetcher: Option<Arc<dyn PageFetch>>,
        limits: SubmitLimits,
    ) -> std::io::Result<SubmitStore> {
        let feed = feed::FeedLedger::load(&feed::ledger_path(path.as_ref()))?;
        let ledger = review::Ledger::open(path, limits.max_recorded)?;
        Ok(SubmitStore {
            ledger: Mutex::new(ledger),
            feed,
            workers: Arc::new(tokio::sync::Semaphore::new(MAX_LEDGER_WORKERS)),
            budgets: Caches::new(1, 1),
            daily: Mutex::new(Daily::default()),
            fetcher,
            allow: CLEARED_HOSTS.iter().map(|h| (*h).to_string()).collect(),
            limits,
        })
    }

    /// Replace the pull allowlist (tests; must stay a subset of the
    /// rights-cleared hosts in production). Sorted and de-duplicated here.
    pub fn with_allowlist(mut self, hosts: &[&str]) -> Self {
        let mut v: Vec<String> = hosts.iter().map(|h| h.to_ascii_lowercase()).collect();
        v.sort();
        v.dedup();
        self.allow = v;
        self
    }

    /// Number of recorded submissions.
    pub fn recorded(&self) -> usize {
        self.ledger
            .lock()
            .map(|l| l.recorded())
            .unwrap_or(self.limits.max_recorded)
    }

    /// One attempt from the client's per-minute bucket. Unlike the paid
    /// routes, an IPv6 /56 and /48 get the same budget as one client, not a
    /// multiple of it (S1-R1-2): a /48 rotating its /64s cannot outrun the
    /// global budget.
    fn take(&self, scope: &str, client: &str, per_minute: u32, what: &str) -> Result<(), ApiError> {
        let scopes: Vec<(String, u32)> = rate_scopes(client, per_minute)
            .into_iter()
            .map(|(k, _)| (format!("{scope}:{k}"), per_minute))
            .collect();
        if self.budgets.take_attempts(&scopes) {
            return Ok(());
        }
        Err(limited(per_minute, what))
    }

    /// One unit from each (key, quota) for today, all or none (S1-R1-2):
    /// the client's scopes (its address, and for IPv6 its /56 and /48, each
    /// with the client's own quota) and optionally the server's.
    fn take_day(
        &self,
        scope: &str,
        client: &str,
        per_day: u32,
        global: Option<u32>,
        what: &str,
    ) -> Result<(), ApiError> {
        let mut keys: Vec<(String, u32)> = rate_scopes(client, per_day)
            .into_iter()
            .map(|(k, _)| (format!("{scope}:{k}"), per_day))
            .collect();
        if let Some(g) = global {
            keys.push((format!("{scope}:all"), g));
        }
        let now = now_secs();
        let day = now / 86_400;
        let over = || {
            ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "daily_quota",
                format!("today's quota of {what} is used up; try again after 00:00 UTC"),
            )
            .with_header("retry-after", (86_400 - now % 86_400).to_string())
        };
        let mut d = self.daily.lock().map_err(|_| over())?;
        if d.day != day {
            d.day = day;
            d.counts.clear();
        }
        let new = keys
            .iter()
            .filter(|(k, _)| !d.counts.contains_key(k))
            .count();
        if d.counts.len() + new > MAX_DAILY_KEYS {
            return Err(over());
        }
        if keys
            .iter()
            .any(|(k, n)| d.counts.get(k).copied().unwrap_or(0) >= *n)
        {
            return Err(over());
        }
        for (k, _) in keys {
            *d.counts.entry(k).or_insert(0) += 1;
        }
        Ok(())
    }

    fn take_global(&self, scope: &str, per_minute: u32, what: &str) -> Result<(), ApiError> {
        if self
            .budgets
            .take_attempts(&[(format!("{scope}:all"), per_minute)])
        {
            return Ok(());
        }
        Err(limited(per_minute, what))
    }
}

fn limited(per_minute: u32, what: &str) -> ApiError {
    ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        format!("too many {what}; try again in a minute"),
    )
    .with_header("retry-after", (60 / per_minute.max(1)).max(1).to_string())
}

/// The submission routes, to pass to [`crate::router_with`] (behind the API's
/// guard: one concurrency limit, the deadline, the client key).
pub fn routes(store: Arc<SubmitStore>) -> Router {
    Router::new()
        .route("/v1/submit/check", post(check))
        .route("/v1/submit", post(submit))
        .route("/v1/submit/{id}", get(status))
        .with_state(store)
}

async fn body_of(body: Body) -> Result<Submission, ApiError> {
    let bytes = axum::body::to_bytes(body, MAX_BODY).await.map_err(|_| {
        ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body_too_large",
            format!("a submission is at most {MAX_BODY} bytes"),
        )
    })?;
    Submission::parse(&bytes)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.code(), e.to_string()))
}

fn no_store(mut r: Response) -> Response {
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn client_of(client: Option<Extension<ClientKey>>) -> String {
    client.map_or_else(|| "local".to_string(), |Extension(c)| c.0)
}

fn hash_hex(s: &Submission) -> String {
    format!("0x{}", hex::encode(s.hash().0))
}

async fn check(
    State(store): State<Arc<SubmitStore>>,
    client: Option<Extension<ClientKey>>,
    body: Body,
) -> Response {
    check_inner(&store, &client_of(client), body)
        .await
        .map_or_else(IntoResponse::into_response, no_store)
}

async fn check_inner(store: &SubmitStore, client: &str, body: Body) -> Result<Response, ApiError> {
    store.take(
        "submit-check",
        client,
        store.limits.check_per_minute,
        "checks from this address",
    )?;
    store.take_day(
        "submit-check-day",
        client,
        store.limits.check_per_day,
        None,
        "checks from this address",
    )?;
    let s = body_of(body).await?;
    let route = s.route();
    let allow: Vec<&str> = store.allow.iter().map(String::as_str).collect();
    let policy = fetch_policy(&s, &route, &allow);
    let mut out = json!({
        "valid": true,
        "hash": hash_hex(&s),
        "route": route.to_json_explained(),
        "fetch": policy.to_json(),
        "preview": null,
        "fetchError": null,
    });
    if let FetchPolicy::Pull { url, host } = &policy {
        match &store.fetcher {
            None => {
                out["fetch"] = json!({
                    "pull": false,
                    "reason": "no_transport",
                    "text": "This server has no fetcher configured, so it pulls nothing.",
                });
            }
            Some(f) => {
                store.take_global(
                    "submit-fetch",
                    store.limits.global_fetch_per_minute,
                    "pulls on this server",
                )?;
                match f.get(url, host).await {
                    Ok(bytes) => {
                        out["preview"] = preview(&bytes, s.json_pointer.as_deref()).to_json()
                    }
                    Err(e) => {
                        out["fetchError"] = json!({ "code": e.code(), "message": e.to_string() })
                    }
                }
            }
        }
    }
    Ok((StatusCode::OK, Json(out)).into_response())
}

async fn submit(
    State(store): State<Arc<SubmitStore>>,
    client: Option<Extension<ClientKey>>,
    body: Body,
) -> Response {
    submit_inner(store, &client_of(client), body)
        .await
        .map_or_else(IntoResponse::into_response, no_store)
}

fn refused(route: &Route) -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "refused",
        "this submission cannot be taken; the route says why",
    )
    .with_field("route", route.to_json_explained())
}

async fn submit_inner(
    store: Arc<SubmitStore>,
    client: &str,
    body: Body,
) -> Result<Response, ApiError> {
    store.take(
        "submit",
        client,
        store.limits.submit_per_minute,
        "submissions from this address",
    )?;
    let s = body_of(body).await?;
    let route = s.route();
    if route.outcome == Outcome::Refused {
        return Err(refused(&route));
    }
    let status = route.status();
    let hash = hash_hex(&s);
    // The same content again is a new entry with a new id (S1-R1-8): the
    // answer never says whether someone else already sent it, and the daily
    // quotas price a repeat like any other submission. A reviewer groups
    // entries by `hash`.
    let permit = ledger_permit(&store)?;
    let st = store.clone();
    let client = client.to_owned();
    let id = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut ledger = st
            .ledger
            .lock()
            .map_err(|_| review_api_error(ReviewError::Unavailable))?;
        if ledger.recorded() >= st.limits.max_recorded {
            return Err(review_api_error(ReviewError::Full));
        }
        st.take_global(
            "submit-all",
            st.limits.global_submit_per_minute,
            "submissions on this server",
        )?;
        st.take_day(
            "submit-day",
            &client,
            st.limits.submit_per_day,
            Some(st.limits.global_submit_per_day),
            "submissions",
        )?;
        ledger.record(s, now_secs()).map_err(review_api_error)
    })
    .await
    .map_err(|_| crate::internal("submission worker failed"))??;
    let body =
        json!({ "id": id, "status": status, "hash": hash, "route": route.to_json_explained() });
    Ok((StatusCode::CREATED, Json(body)).into_response())
}

async fn status(
    State(store): State<Arc<SubmitStore>>,
    client: Option<Extension<ClientKey>>,
    Path(id): Path<String>,
) -> Response {
    let client = client_of(client);
    let r = (|| {
        store.take(
            "submit-status",
            &client,
            store.limits.status_per_minute,
            "status reads from this address",
        )?;
        let not_found = || {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "no submission with this id",
            )
        };
        if !is_id(&id) {
            return Err(not_found());
        }
        Ok(())
    })();
    if let Err(e) = r {
        return e.into_response();
    }
    let permit = match ledger_permit(&store) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let lookup_id = id.clone();
    let worker = store.clone();
    let item = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        worker.review_item(&lookup_id)
    })
    .await;
    let r = match item {
        Ok(Ok(item)) => {
            let mut body = json!({"id": id, "status": item.status});
            if let Some(check) = store.feed.public_check(&item.id, &item.status, &item.hash) {
                body["check"] = check;
            }
            Ok((StatusCode::OK, Json(body)).into_response())
        }
        Ok(Err(e)) => Err(review_api_error(e)),
        Err(_) => Err(crate::internal("submission lookup failed")),
    };
    r.map_or_else(IntoResponse::into_response, no_store)
}

fn ledger_permit(store: &SubmitStore) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    store.workers.clone().try_acquire_owned().map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "intake_busy",
            "submission storage is busy; try again later",
        )
        .with_header("retry-after", "5".into())
    })
}

fn review_api_error(error: ReviewError) -> ApiError {
    match error {
        ReviewError::Full => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "intake_full",
            "the review queue is full; try again later",
        )
        .with_header("retry-after", "3600".into()),
        ReviewError::NotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no submission with this id",
        ),
        _ => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "intake_unavailable",
            "submission storage is unavailable; try again later",
        ),
    }
}

/// The real transport (feature `http-client`): see the module docs.
#[cfg(feature = "http-client")]
pub use https::{HttpsFetch, PublicResolver, USER_AGENT};

#[cfg(feature = "http-client")]
mod https {
    use super::{check_status, FetchError, PageFetch};
    use crate::x402::BoxFuture;
    use http_body_util::BodyExt as _;
    use hyper_util::client::legacy::connect::dns::Name;
    use hyper_util::client::legacy::connect::HttpConnector;
    use onyx_intake::{is_public_ip, FETCH_TIMEOUT_SECS, MAX_BODY_BYTES};
    use std::net::SocketAddr;
    use std::task::{Context, Poll};
    use std::time::Duration;

    /// The User-Agent of a check pull: the project's name, nothing about
    /// the submitter or the operator.
    pub const USER_AGENT: &str = "onyx-submit-check/1";

    /// A resolver that answers only when every address of the name is public
    /// ([`is_public_ip`]); the connector then connects to exactly those.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct PublicResolver;

    impl tower_service::Service<Name> for PublicResolver {
        type Response = std::vec::IntoIter<SocketAddr>;
        type Error = std::io::Error;
        type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, name: Name) -> Self::Future {
            let host = name.as_str().to_string();
            Box::pin(async move {
                let addrs: Vec<SocketAddr> =
                    tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
                public_only(&host, addrs).map(Vec::into_iter)
            })
        }
    }

    /// `addrs` when there is at least one and every one is public.
    pub(super) fn public_only(
        host: &str,
        addrs: Vec<SocketAddr>,
    ) -> std::io::Result<Vec<SocketAddr>> {
        if addrs.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{host} has no address"),
            ));
        }
        if let Some(bad) = addrs.iter().find(|a| !is_public_ip(a.ip())) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{host} resolves to {}, which is not a public address",
                    bad.ip()
                ),
            ));
        }
        Ok(addrs)
    }

    type Client = hyper_util::client::legacy::Client<
        hyper_rustls::HttpsConnector<HttpConnector<PublicResolver>>,
        http_body_util::Empty<hyper::body::Bytes>,
    >;

    /// HTTPS GET with the public-address resolver, webpki roots, no redirects,
    /// a byte cap and one deadline.
    pub struct HttpsFetch {
        client: Client,
    }

    impl Default for HttpsFetch {
        fn default() -> Self {
            Self::new()
        }
    }

    impl HttpsFetch {
        /// A new transport.
        pub fn new() -> Self {
            let mut http = HttpConnector::new_with_resolver(PublicResolver);
            // The https connector below speaks TLS over this; it refuses http.
            http.enforce_http(false);
            http.set_connect_timeout(Some(Duration::from_secs(5)));
            let https = hyper_rustls::HttpsConnectorBuilder::new()
                .with_webpki_roots()
                .https_only()
                .enable_http1()
                .wrap_connector(http);
            let client =
                hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                    .pool_max_idle_per_host(0)
                    .build(https);
            HttpsFetch { client }
        }

        async fn fetch(&self, url: &str) -> Result<Vec<u8>, FetchError> {
            // The connector skips the resolver for an IP literal and would
            // connect to it directly: refuse every literal here (the URL
            // rules refuse them too, onyx_intake::HttpsUrl).
            let uri: hyper::Uri = url.parse().map_err(|e: hyper::http::uri::InvalidUri| {
                FetchError::Unreachable(e.to_string())
            })?;
            let host = uri.host().unwrap_or_default();
            if host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok()
            {
                return Err(FetchError::Refused(format!(
                    "{host} is an IP address, which is not a public address the check may name"
                )));
            }
            let req = hyper::Request::get(url)
                .header("user-agent", USER_AGENT)
                .header("accept", "application/json")
                .body(http_body_util::Empty::new())
                .map_err(|e| FetchError::Unreachable(e.to_string()))?;
            let r = self.client.request(req).await.map_err(|e| {
                // The resolver's refusal surfaces as a connect error.
                let text = format!("{e:?}");
                if text.contains("not a public address") || text.contains("has no address") {
                    FetchError::Refused(text_of(&text))
                } else {
                    FetchError::Unreachable(e.to_string())
                }
            })?;
            check_status(r.status().as_u16())?;
            let declared = r
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            if declared.is_some_and(|n| n > MAX_BODY_BYTES as u64) {
                return Err(FetchError::TooLarge(MAX_BODY_BYTES));
            }
            let bytes = http_body_util::Limited::new(r.into_body(), MAX_BODY_BYTES)
                .collect()
                .await
                .map_err(|_| FetchError::TooLarge(MAX_BODY_BYTES))?
                .to_bytes();
            Ok(bytes.to_vec())
        }
    }

    /// The resolver's sentence out of a connect error's debug text.
    fn text_of(debug: &str) -> String {
        for key in ["which is not a public address", "has no address"] {
            if let Some(end) = debug.find(key) {
                let start = debug[..end].rfind('"').map_or(0, |i| i + 1);
                return debug[start..end + key.len()].to_string();
            }
        }
        "the host's address is not public".into()
    }

    impl PageFetch for HttpsFetch {
        fn get<'a>(
            &'a self,
            url: &'a str,
            _host: &'a str,
        ) -> BoxFuture<'a, Result<Vec<u8>, FetchError>> {
            Box::pin(async move {
                tokio::time::timeout(Duration::from_secs(FETCH_TIMEOUT_SECS), self.fetch(url))
                    .await
                    .map_err(|_| FetchError::Timeout(FETCH_TIMEOUT_SECS))?
            })
        }
    }
}

#[cfg(test)]
#[path = "submit_tests.rs"]
mod tests;
