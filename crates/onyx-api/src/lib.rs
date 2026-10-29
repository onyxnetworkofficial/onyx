//! Delivery (M7, ADR-0023): a query API that serves finalized rows with a
//! Merkle proof each, charges per call with x402 v2 while data is fresh, and
//! exposes a settlement endpoint whose output a contract checks with
//! `OnyxMerkle.verify` / `OnyxSettlement.verifyRow`.
//!
//! The server is not trusted for correctness (docs/spec/delivery.md): every
//! row comes with its proof and a verification bundle (chain, contract, root,
//! binding and the binding's inputs) that a client checks against
//! `getFinalizedRoot` itself ([`client::verify_response`]). Roots come only
//! from the canonical ledger ([`roots::RootSource`]), and rows are served only
//! after they hash to that root.
//!
//! Request order (M7-F3): parse (one canonical URL per resource, M7-L39-F6),
//! look up the shard and its root (cached), price, answer 402 if unpaid,
//! verify the payment, and only then read rows (cached by root), build the
//! body, settle, and respond. Settlement runs in its own task so the request
//! deadline cannot drop a payment that lands on chain (M7-L39-F1).

pub mod arrival;
pub mod cache;
pub mod catalog;
pub mod client;
pub mod contrib;
pub mod evm_tx;
pub mod gate;
pub mod indexer;
pub mod intents;
pub mod journal;
pub mod labels_serve;
pub mod ladder;
mod lease;
pub mod levels;
pub mod observation_book;
pub mod observations;
pub mod observe;
pub mod prepaid;
pub mod prepaid_chain;
pub mod prepaid_gate;
pub mod receipts;
pub mod reporter;
pub mod roots;
pub mod self_settle;
pub mod settle_check;
pub mod signed_serve;
pub mod submit;
pub mod track_serve;
pub mod wants;
pub mod x402;

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use borsh::BorshDeserialize;
use cache::{Caches, Loaded, RootMiss};
use catalog::{Catalog, ShardMeta};
use onyx_coordinator::cas::{Cas, CasExt};
use onyx_core::{keccak256, Hash32, Redistribution};
use onyx_merkle::MerkleTree;
use onyx_recipe_sdk::OutputRow;
use roots::{FinalRoot, LedgerKind, RootSource};
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::Semaphore;
use x402::{Facilitator, FacilitatorRequest, PaymentPayload, PaymentRequired, PaymentTerms};

/// How the server gets paid. `None` in [`ApiState`] serves everything free.
pub struct Payments {
    /// Network, asset and recipient.
    pub terms: PaymentTerms,
    /// Verifies and settles payments.
    pub facilitator: Arc<dyn Facilitator>,
}

/// Unix-seconds clock (injected so tests control decay).
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// Limits and switches (docs/ops/run-api.md lists the defaults).
#[derive(Clone, Debug)]
pub struct ApiConfig {
    /// Whole-request deadline (504 after it). At least 2.5 x
    /// `facilitator_timeout` ([`ApiConfig::validate`]), so verify plus settle
    /// fit inside it.
    pub request_timeout: Duration,
    /// Requests in flight before new ones get 503 `overloaded`.
    pub max_concurrency: usize,
    /// Deadline of one facilitator `verify` call; a `settle` gets
    /// [`SETTLE_PATIENCE`] times this in its own task.
    pub facilitator_timeout: Duration,
    /// Facilitator calls in flight, server-wide (M7-L39-F4); 503
    /// `facilitator_busy` beyond.
    pub max_facilitator_calls: usize,
    /// Payment attempts per client per minute (same burst); 429 beyond.
    pub payment_attempts_per_minute: u32,
    /// Largest export, in rows (413 beyond).
    pub export_max_rows: u64,
    /// Largest export body, in bytes (413 beyond; M7-L47-F2). The 413 comes
    /// before any 402 when the rows are already in memory (and always when
    /// free); a paid export whose rows are not loaded yet gets its 402 first
    /// and the 413 after verify, settling nothing and refunding the unit.
    pub export_max_bytes: u64,
    /// Free exports per client per minute (same burst); 429 beyond
    /// (M7-L47-F2). Paid exports are limited by the payment rate instead.
    pub free_exports_per_minute: u32,
    /// A response holds its in-flight permit until it is written, at most
    /// this long; a body not written by then fails and closes the connection
    /// (M7-L47-F2, the `lease` module).
    pub send_timeout: Duration,
    /// How long a finalized root is cached.
    pub root_ttl: Duration,
    /// Serve shards pinned to a `test.*` label set (dev and demo only).
    pub allow_test_labels: bool,
    /// Label-set rights (`ONYX_RIGHTS`, ROADMAP L14): in `Fallback`, every
    /// label-derived shard ([`catalog::ShardMeta::label_derived`]) is refused
    /// with 403 `label_rights_withheld`. Default `Cleared`.
    pub rights: catalog::LabelRights,
    /// Settled responses kept for retries (M7-L40-F5).
    pub settled_entries: usize,
    /// Bytes of settled responses kept for retries (M7-L41-F3).
    pub settled_bytes: usize,
    /// Verified payments per payer per hour that may end without settling
    /// (a 404 after verify, a refused settle) before that payer gets 429
    /// `payer_limited` (M7-L40-F2/F3).
    pub unsettled_per_hour: u32,
    /// Accept a payment network outside [`x402::TESTNET_NETWORKS`]
    /// (M7-L47-F4). Off by default; this project is testnet-only and never
    /// sets it.
    pub allow_mainnet: bool,
    /// Reverse proxies whose `X-Forwarded-For` is believed (M7-L48-F2).
    /// When the peer is in one of these ranges, the client is the
    /// right-most `X-Forwarded-For` address outside them; otherwise the
    /// header is ignored and the client is the peer. Empty by default.
    pub trusted_proxies: Vec<Cidr>,
    /// How far past `now + maxTimeoutSeconds` an authorization's
    /// `validBefore` may lie (OPS-L3-F4). x402 clients sign `validBefore =
    /// now + maxTimeoutSeconds`; the slack covers clock skew. A longer-lived
    /// authorization is refused before verify: its intent would otherwise
    /// stay open until it expired.
    pub authorization_slack: Duration,
    /// How long a root lookup that found no final root, or failed, is
    /// remembered (AUD-1-F4): requests for such a shard reach the node at
    /// most once per this interval.
    pub root_miss_ttl: Duration,
    /// Root lookups per client per minute that miss every cache and reach
    /// the node (AUD-1-F4); 429 beyond. Cached answers cost nothing.
    pub root_lookups_per_minute: u32,
    /// Free `/v1/observations/{metric}/attempts` answers per client per
    /// minute (same burst); 429 beyond (ATT-R1-F5).
    pub free_attempts_per_minute: u32,
    /// The wait between two reads of the authorization check after an
    /// unknown settle ([`settle_check`], M7L3-R1-1): about one block, so a
    /// node that was a block behind has caught up before the next read.
    /// Never more than [`ApiState::settle_check_budget`]; counted once in
    /// the stop budget.
    pub settle_recheck_wait: Duration,
}

/// An address range, `addr/prefix` (M7-L48-F2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Whether `ip` is in the range (IPv4-mapped IPv6 counts as IPv4).
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(a)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(net) & mask == u32::from(a) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(a)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(net) & mask == u128::from(a) & mask
            }
            _ => false,
        }
    }
}

impl std::str::FromStr for Cidr {
    type Err = ConfigError;
    /// `10.0.0.0/8`, `2001:db8::/32`, or a bare address (a single host).
    fn from_str(s: &str) -> Result<Self, ConfigError> {
        let bad = || ConfigError(format!("bad trusted proxy range {s:?}"));
        let (a, p) = s
            .trim()
            .split_once('/')
            .map_or((s.trim(), None), |(a, p)| (a, Some(p)));
        let addr: IpAddr = a.parse().map_err(|_| bad())?;
        let addr = addr.to_canonical();
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match p {
            None => max,
            Some(p) => p.parse::<u8>().ok().filter(|p| *p <= max).ok_or_else(bad)?,
        };
        // A /0 trusts every peer, so any client could pick its own key by
        // sending `X-Forwarded-For` (AUD-2-F8).
        if prefix == 0 {
            return Err(ConfigError(format!(
                "trusted proxy range {s:?} is /0: it would trust every peer"
            )));
        }
        Ok(Cidr { addr, prefix })
    }
}

/// The client address of a request from `peer` (M7-L48-F2): the peer
/// itself, unless it is a trusted proxy; then the right-most
/// `X-Forwarded-For` entry that is not a trusted proxy. A malformed entry
/// before such an address is found, or none at all, leaves the peer: a
/// client cannot pick its own key by spoofing the header.
pub fn client_ip(peer: IpAddr, forwarded_for: &[&str], trusted: &[Cidr]) -> IpAddr {
    let is_trusted = |ip: IpAddr| trusted.iter().any(|c| c.contains(ip));
    if !is_trusted(peer) {
        return peer;
    }
    let hops = forwarded_for
        .iter()
        .flat_map(|h| h.split(','))
        .map(str::trim)
        .collect::<Vec<_>>();
    for hop in hops.iter().rev() {
        match forwarded_ip(hop) {
            Ok(ip) if is_trusted(ip) => continue,
            Ok(ip) => return ip,
            Err(_) => return peer,
        }
    }
    peer
}

/// One `X-Forwarded-For` entry as an address (M7-L50-F4): a bare address,
/// `a.b.c.d:port`, `[v6]` or `[v6]:port` (proxies that append the peer's
/// socket address). The port is dropped; anything else is malformed.
fn forwarded_ip(hop: &str) -> Result<IpAddr, ()> {
    if let Ok(ip) = hop.parse::<IpAddr>() {
        return Ok(ip);
    }
    if let Ok(sa) = hop.parse::<SocketAddr>() {
        return Ok(sa.ip());
    }
    hop.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .and_then(|h| h.parse::<std::net::Ipv6Addr>().ok())
        .map(IpAddr::V6)
        .ok_or(())
}

/// Wider IPv6 buckets: a /56 gets this many times the per-client rate, a /48
/// this many times again (M7-L40-F2).
pub const PREFIX_RATE_MULTIPLIER: u32 = 4;

/// A settle waits this many facilitator timeouts in its own task: longer
/// than any request, so a payment that lands on chain is recorded for the
/// retry even after the request gave up (M7-L39-F1).
pub const SETTLE_PATIENCE: u32 = 4;

/// The authorization check after an unknown settle ([`settle_check`]) gets
/// this fraction of the settle patience, each time it runs (twice at most).
pub const SETTLE_CHECK_SHARE: u32 = 2;

/// Seconds an authorization must outlive the settle budget to be accepted
/// (OPS-L4-F1): a settle mined after `validBefore` is refused by the token.
pub const SETTLE_MARGIN_SECS: u64 = 5;

/// `Retry-After` (seconds) of a 503 `payment_pending`: the identical retry of
/// a paid request whose self-settle ended unknown (SOAK-1). About one
/// resolver pass (the reporter interval, 30 s by default).
pub const PAYMENT_PENDING_RETRY_SECS: u64 = 30;

/// The configuration cannot work.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid API configuration: {0}")]
pub struct ConfigError(pub String);

impl ApiConfig {
    /// Refuse configurations that break the payment flow: the request deadline
    /// must cover verify and settle (2 x facilitator timeout) plus half a
    /// timeout of margin (M7-L39-F1), and every limit must be non-zero.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let need = self.facilitator_timeout * 2 + self.facilitator_timeout / 2;
        if self.request_timeout < need {
            return Err(ConfigError(format!(
                "request_timeout {:?} must be at least 2.5 x facilitator_timeout ({need:?})",
                self.request_timeout
            )));
        }
        if self.facilitator_timeout.is_zero()
            || self.max_concurrency == 0
            || self.max_facilitator_calls == 0
            || self.payment_attempts_per_minute == 0
            || self.export_max_bytes == 0
            || self.free_exports_per_minute == 0
            || self.send_timeout.is_zero()
            || self.settled_entries == 0
            || self.settled_bytes == 0
            || self.unsettled_per_hour == 0
            || self.root_miss_ttl.is_zero()
            || self.root_lookups_per_minute == 0
            || self.free_attempts_per_minute == 0
        {
            return Err(ConfigError("timeouts and limits must be non-zero".into()));
        }
        Ok(())
    }
}

impl Default for ApiConfig {
    fn default() -> Self {
        ApiConfig {
            request_timeout: Duration::from_secs(12),
            max_concurrency: 256,
            facilitator_timeout: Duration::from_secs(4),
            max_facilitator_calls: 32,
            payment_attempts_per_minute: 30,
            export_max_rows: 100_000,
            export_max_bytes: 32 << 20,
            free_exports_per_minute: 6,
            send_timeout: Duration::from_secs(60),
            root_ttl: Duration::from_secs(30),
            allow_test_labels: false,
            rights: catalog::LabelRights::Cleared,
            settled_entries: cache::SETTLED_ENTRIES,
            settled_bytes: cache::SETTLED_BYTES,
            unsettled_per_hour: 20,
            allow_mainnet: false,
            trusted_proxies: Vec::new(),
            authorization_slack: Duration::from_secs(300),
            root_miss_ttl: Duration::from_secs(5),
            root_lookups_per_minute: 120,
            free_attempts_per_minute: 30,
            // Base mines a block every 2 s.
            settle_recheck_wait: Duration::from_secs(2),
        }
    }
}

/// Something that reports the level-3 post log on `/v1/health` (ROADMAP
/// C62): the level-3 store implements it; this file names no level-3 code
/// (the signed-money fence in scripts/check-boundaries.sh).
pub trait Level3Health: Send + Sync {
    /// JSON for the `level3` field.
    fn level3_health(&self) -> Value;
}

/// Shared server state. Build it with [`ApiState::new`].
pub struct ApiState {
    /// Shard metadata, markets and prices.
    pub catalog: RwLock<Catalog>,
    /// Finalized roots.
    pub roots: Arc<dyn RootSource>,
    /// Row batches, keyed by root.
    pub cas: Arc<dyn Cas + Send + Sync>,
    /// x402 payments, or free.
    pub payments: Option<Payments>,
    /// Unix-seconds clock.
    pub clock: Clock,
    /// Public base URL, used in the x402 `resource.url` (no trailing slash).
    pub public_url: String,
    /// Limits and switches.
    pub config: ApiConfig,
    /// Roots, row batches, settled payments and rate limits.
    pub caches: Arc<Caches>,
    /// Contributor pass ("add data, get data", ADR-0025); `None` honours no
    /// proofs. Set with [`ApiState::with_contributors`].
    pub contributors: Option<contrib::ContributorPolicy>,
    /// Usage receipts of settled payments, for `OnyxRevenue.credit` (ADR-0025).
    pub receipts: Arc<receipts::ReceiptLog>,
    /// The reporter posting those receipts, for `/v1/health` (REV-L4-F1).
    /// Set with [`ApiState::with_reporter`].
    pub reporter: Option<Arc<dyn reporter::ReporterHealth>>,
    /// The chain indexer that fills the catalog from `Finalized` events
    /// (ADR-0033), for `/v1/health`. Set with [`ApiState::with_indexer`].
    pub indexer: Option<Arc<dyn indexer::IndexerHealth>>,
    /// The level-3 post log's counts and stop for `/v1/health` (ROADMAP
    /// C62). Set with [`ApiState::with_level3`].
    pub level3: Option<Arc<dyn Level3Health>>,
    /// Settle tasks still running. A settle outlives its request (M7-L39-F1),
    /// so a clean stop waits for these after the last connection closes,
    /// or a payment that landed on chain would never reach its receipt
    /// (OPS-L1-F1).
    pub settle_tasks: Arc<SettleTasks>,
    /// Settle intents: every paid settle is written here before it is made,
    /// so its outcome is recoverable whatever happens (OPS-L2-F1, see
    /// [`intents`]). Set with [`ApiState::with_intent_log`].
    pub intents: Arc<intents::IntentLog>,
    /// Settles that ended with an unknown outcome since start (OPS-L2-F3);
    /// each is logged and left to the intent resolver.
    pub unknown_settles: Arc<std::sync::atomic::AtomicU64>,
    /// Serving checks per metric and their latest answers (ADR-0034
    /// Amendment 1). Add with [`ApiState::with_gate`]; run with
    /// [`gate::refresh`].
    pub gates: gate::Gates,
    /// Reads the token after a facilitator settle whose outcome is unknown
    /// (M7-F5 residual, [`settle_check`]): serve on proof of payment, re-send
    /// an unused authorization once, refuse one spent elsewhere. Set with
    /// [`ApiState::with_settle_check`]; without it an unknown settle stays
    /// unknown.
    pub settle_check: Option<Arc<dyn settle_check::SettleCheck>>,
    /// Prepaid balances (ROADMAP S7, ADR-0046 amendment 1): a paid answer may be paid with a
    /// voucher instead of x402. See [`ApiState::with_prepaid`].
    pub prepaid: Option<Arc<prepaid_gate::PrepaidState>>,
    /// Observe on the first paid query (ADR-0049, NORTH-STAR NS10): a level-2 subject no round
    /// settled yet is offered for a prepaid hold. See [`ApiState::with_observe`].
    pub observe: Option<Arc<observe::ObserveState>>,
    inflight: Arc<Semaphore>,
    facilitator_slots: Arc<Semaphore>,
}

impl ApiState {
    /// A server over `catalog`, `roots` and `cas`, paid through `payments`.
    /// Refuses a configuration that fails [`ApiConfig::validate`], and a
    /// payment network outside [`x402::TESTNET_NETWORKS`] unless
    /// `allow_mainnet` is set (M7-L47-F4).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        catalog: Catalog,
        roots: Arc<dyn RootSource>,
        cas: Arc<dyn Cas + Send + Sync>,
        payments: Option<Payments>,
        clock: Clock,
        public_url: impl Into<String>,
        config: ApiConfig,
    ) -> Result<Self, ConfigError> {
        config.validate()?;
        if let Some(p) = &payments {
            x402::check_network(&p.terms.network, config.allow_mainnet).map_err(ConfigError)?;
        }
        let caches = Arc::new(Caches::new(config.settled_entries, config.settled_bytes));
        Ok(ApiState {
            catalog: RwLock::new(catalog),
            roots,
            cas,
            payments,
            clock,
            public_url: public_url.into(),
            inflight: Arc::new(Semaphore::new(config.max_concurrency)),
            facilitator_slots: Arc::new(Semaphore::new(config.max_facilitator_calls)),
            config,
            caches,
            contributors: None,
            receipts: Arc::new(receipts::ReceiptLog::new()),
            reporter: None,
            indexer: None,
            level3: None,
            settle_tasks: Arc::new(SettleTasks::default()),
            intents: Arc::new(intents::IntentLog::new()),
            unknown_settles: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            gates: gate::Gates::default(),
            settle_check: None,
            prepaid: None,
            observe: None,
        })
    }

    /// Keep the settle intents in the journal at `path` (OPS-L2-F1), bound
    /// to one chain and `OnyxRevenue`, loading those it already holds (a
    /// settle cut by a crash or a kill): run an
    /// [`intents::IntentResolver`] over [`ApiState::intents`] and
    /// [`ApiState::receipts`] to turn them into receipts or drop them.
    pub fn with_intent_log(
        mut self,
        path: impl AsRef<std::path::Path>,
        bind: &journal::Binding,
    ) -> std::io::Result<Self> {
        self.intents = Arc::new(intents::IntentLog::open(path, bind)?);
        Ok(self)
    }

    /// Report `reporter`'s status on `/v1/health`: what is in flight, since
    /// when a `credit` is stuck, and its last error (REV-L4-F1).
    pub fn with_reporter(mut self, reporter: Arc<dyn reporter::ReporterHealth>) -> Self {
        self.reporter = Some(reporter);
        self
    }

    /// Report `indexer`'s cursor and the shards it knows on `/v1/health`
    /// (ADR-0033): served, pending, not held, or refused and why.
    pub fn with_indexer(mut self, indexer: Arc<dyn indexer::IndexerHealth>) -> Self {
        self.indexer = Some(indexer);
        self
    }

    /// Report the level-3 post log on `/v1/health` (ROADMAP C62): pending
    /// and refused posts, the oldest pending post's age on this host's clock
    /// and whether level 3 is stopped, so a watch alerts before the 600 s
    /// breaker trips (THREATS C10-3).
    pub fn with_level3(mut self, level3: Arc<dyn Level3Health>) -> Self {
        self.level3 = Some(level3);
        self
    }

    /// Keep the usage receipts in the journal at `path` (REV-11), bound to
    /// one chain and `OnyxRevenue` (REV-L3-F4), loading those it already
    /// holds: a receipt recorded before a paid answer survives a crash. A self-settling server shares its facilitator's log
    /// instead ([`self_settle::SelfSettle::persist`], then set
    /// [`ApiState::receipts`] to its `receipts()`).
    pub fn with_receipt_log(
        mut self,
        path: impl AsRef<std::path::Path>,
        bind: &journal::Binding,
    ) -> std::io::Result<Self> {
        self.receipts = Arc::new(receipts::ReceiptLog::open(path, bind)?);
        Ok(self)
    }

    /// Serve `g`'s metric only for shards and rows it cleared (ADR-0034
    /// Amendment 1); run [`gate::refresh`] after the catalog changes.
    pub fn with_gate(mut self, g: Arc<dyn gate::ServingGate>) -> Result<Self, ConfigError> {
        self.gates.add(g).map_err(ConfigError)?;
        Ok(self)
    }

    /// After a facilitator settle whose outcome is unknown, ask `check`
    /// what became of the authorization before re-sending it or giving up
    /// (M7-F5 residual, [`settle_check`]).
    pub fn with_settle_check(mut self, check: Arc<dyn settle_check::SettleCheck>) -> Self {
        self.settle_check = Some(check);
        self
    }

    /// Honour contributor proofs under `policy` (ADR-0025 item 3).
    pub fn with_contributors(mut self, policy: contrib::ContributorPolicy) -> Self {
        self.contributors = Some(policy);
        self
    }

    /// Take prepaid vouchers ([`prepaid_gate::PREPAID_VOUCHER`]) for paid answers, beside x402
    /// (ROADMAP S7). Answers keep their x402 price; a voucher must add exactly that price.
    pub fn with_prepaid(mut self, prepaid: Arc<prepaid_gate::PrepaidState>) -> Self {
        self.prepaid = Some(prepaid);
        self
    }

    /// Offer rounds for level-2 subjects no round settled yet, paid by a prepaid hold (ADR-0049).
    /// Needs payments and prepaid ([`Self::with_prepaid`]) first; refused otherwise.
    pub fn with_observe(
        mut self,
        observe: Arc<observe::ObserveState>,
    ) -> Result<Self, ConfigError> {
        if self.payments.is_none() || self.prepaid.is_none() {
            return Err(ConfigError(
                "observing on payment needs payments and prepaid balances".into(),
            ));
        }
        self.observe = Some(observe);
        Ok(self)
    }
}

/// The `/v1` router. Serve it with
/// `into_make_service_with_connect_info::<SocketAddr>()` so payment attempts
/// are rate limited per client address (without it, all clients share one).
pub fn router(state: Arc<ApiState>) -> Router {
    router_with(state, Router::new())
}

/// [`router`] plus `extra` routes (stateless after `with_state`), merged
/// before the guard so they share its concurrency limit, deadline and client
/// key. `extra` is opaque here: the track-record routes join this way, so no
/// file on the pricing, payment or settlement path names them (ROADMAP C6,
/// THREATS TR-11). `extra` must not set a fallback.
pub fn router_with(state: Arc<ApiState>, extra: Router) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/openapi.json", get(openapi))
        .route("/v1/proofs/{round}/{shard}/{index}", get(proof))
        .route("/v1/metrics/{metric}", get(metric))
        .route("/v1/export/{round}/{shard}", get(export))
        .route("/v1/settlement/{market}", get(settlement))
        .route("/v1/served/{metric}", get(served))
        .route("/v1/observations/{metric}", get(observation))
        .route(
            "/v1/observations/{metric}/attempts",
            get(observation_attempts),
        )
        .route("/v1/tickets/{ticket}", get(ticket))
        .route("/v1/estimates/{metric}", get(estimate))
        .with_state(state.clone())
        .merge(extra)
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn_with_state(state, guard))
}

/// The client a request came from (`local` without connect info): an IPv4
/// address, or an IPv6 /64 (one end site; rotating addresses inside it does
/// not buy fresh rate-limit buckets, M7-L39-F4).
#[derive(Clone, Debug)]
pub struct ClientKey(pub String);

/// Rate-limit and replay key of an address: IPv4 as is (IPv4-mapped IPv6
/// too), other IPv6 by its /64 prefix.
pub fn client_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(a) => a.to_string(),
        IpAddr::V6(a) => match a.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = a.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
    }
}

/// Rate-limit scopes of a client key (M7-L40-F2): the key itself, and for an
/// IPv6 /64 also its /56 and /48, each with a larger budget, so one
/// allocation rotating /64s shares the wider buckets.
pub fn rate_scopes(client: &str, per_minute: u32) -> Vec<(String, u32)> {
    let mut v = vec![(client.to_string(), per_minute)];
    let Some(prefix) = client.strip_suffix("::/64") else {
        return v;
    };
    let seg: Vec<u16> = prefix
        .split(':')
        .filter_map(|h| u16::from_str_radix(h, 16).ok())
        .collect();
    if seg.len() == 4 {
        let m = PREFIX_RATE_MULTIPLIER;
        v.push((
            format!(
                "{:x}:{:x}:{:x}:{:x}::/56",
                seg[0],
                seg[1],
                seg[2],
                seg[3] & 0xff00
            ),
            per_minute.saturating_mul(m),
        ));
        v.push((
            format!("{:x}:{:x}:{:x}::/48", seg[0], seg[1], seg[2]),
            per_minute.saturating_mul(m).saturating_mul(m),
        ));
    }
    v
}

/// One concurrency limit and one deadline for the whole server (M7-F3; the
/// permit lasts until the body is written, M7-L47-F2), plus
/// the client key for rate limiting (behind a trusted proxy, from
/// `X-Forwarded-For`: [`client_ip`]). A middleware rather than tower layers:
/// `Router::layer` would give every route its own semaphore.
///
/// A HEAD request never pays (M7-L50-F2): axum answers HEAD with the GET
/// handler and strips the body, so a settled HEAD would spend the payment
/// on nothing. With `PAYMENT-SIGNATURE` it is refused before any verify or
/// settle; without it, HEAD still shows the 402 terms.
async fn guard(State(state): State<Arc<ApiState>>, mut req: Request, next: Next) -> Response {
    if req.method() == Method::HEAD && req.headers().contains_key(x402::PAYMENT_SIGNATURE) {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_payment",
            "payments are taken only with GET; HEAD never settles",
        )
        .into_response();
    }
    // The same for a contributor proof (ADR-0025): HEAD would use the
    // one-time proof and a read of the address's budget for no body.
    if req.method() == Method::HEAD && req.headers().contains_key(contrib::CONTRIBUTOR_HEADER) {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "contributor proofs are honoured only with GET",
        )
        .into_response();
    }
    let forwarded: Vec<&str> = req
        .headers()
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    let key = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or_else(
            || "local".to_string(),
            |c| {
                client_key(client_ip(
                    c.0.ip(),
                    &forwarded,
                    &state.config.trusted_proxies,
                ))
            },
        );
    req.extensions_mut().insert(ClientKey(key));
    let Ok(permit) = state.inflight.clone().try_acquire_owned() else {
        return ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "overloaded",
            "too many requests in flight",
        )
        .with_header("retry-after", "1".into())
        .into_response();
    };
    let r = match tokio::time::timeout(state.config.request_timeout, next.run(req)).await {
        Ok(r) => r,
        Err(_) => ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            "the request took too long",
        )
        .into_response(),
    };
    // The permit is held until the body is written, not only until the
    // handler returns (M7-L47-F2).
    let (parts, body) = r.into_parts();
    Response::from_parts(
        parts,
        lease::leased(body, permit, state.config.send_timeout),
    )
}

/// A JSON error with a stable `code` (docs/spec/delivery.md lists them).
/// Never cached (`no-store`).
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    headers: Vec<(HeaderName, String)>,
    /// Top-level body fields beside `error` (a level-2 402's `limits`).
    fields: Vec<(&'static str, Value)>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            message: message.into(),
            headers: Vec::new(),
            fields: Vec::new(),
        }
    }

    fn with_field(mut self, name: &'static str, value: Value) -> Self {
        self.fields.push((name, value));
        self
    }

    fn with_header(mut self, name: &'static str, value: String) -> Self {
        self.headers.push((HeaderName::from_static(name), value));
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": { "code": self.code, "message": self.message } });
        for (k, v) in self.fields {
            body[k] = v;
        }
        let mut r = (self.status, Json(body)).into_response();
        r.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        for (k, v) in self.headers {
            if let Ok(v) = HeaderValue::from_str(&v) {
                r.headers_mut().insert(k, v);
            }
        }
        r
    }
}

impl From<ApiError> for Box<Response> {
    fn from(e: ApiError) -> Box<Response> {
        Box::new(e.into_response())
    }
}

/// Handler result: both arms are finished responses (the early one boxed).
type Reply = Result<Response, Box<Response>>;

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", message)
}

fn internal(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
}

fn row_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "row_not_found", "index out of range")
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn nonzero(h: Hash32) -> Value {
    if h == Hash32::ZERO {
        Value::Null
    } else {
        Value::String(h.to_string())
    }
}

/// A decimal path integer, and whether it was written canonically (no sign,
/// no leading zeros).
fn parse_u64(s: &str, what: &str) -> Result<(u64, bool), ApiError> {
    let n = s
        .parse::<u64>()
        .ok()
        .filter(|_| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| bad_request(format!("{what} must be a decimal integer")))?;
    Ok((n, s == n.to_string()))
}

/// A 32-byte hash, and whether it was written canonically: `0x` and 64
/// lowercase hex digits (M7-F8).
fn parse_hash(s: &str, what: &str) -> Result<(Hash32, bool), ApiError> {
    let digits = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    let mut a = [0u8; 32];
    if digits.len() != 64 || hex::decode_to_slice(digits, &mut a).is_err() {
        return Err(bad_request(format!("{what} must be 0x and 64 hex digits")));
    }
    let h = Hash32(a);
    Ok((h, s == h.to_string()))
}

/// 308 to the canonical spelling (`target`: path and query) of the same
/// resource.
fn redirect(target: String) -> Response {
    (
        StatusCode::PERMANENT_REDIRECT,
        [
            (header::LOCATION, target),
            (header::CACHE_CONTROL, "public, max-age=86400".to_string()),
        ],
    )
        .into_response()
}

/// Percent-encode a query value: everything but unreserved characters and
/// `:` is `%XX` (uppercase), so each value has exactly one spelling.
fn enc(v: &str) -> String {
    let mut o = String::with_capacity(v.len());
    for b in v.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b':') {
            o.push(char::from(b));
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

/// The one URL of a resource: `path` plus the canonical query.
fn canonical(path: String, query: &[(&str, String)]) -> String {
    if query.is_empty() {
        return path;
    }
    let q: Vec<String> = query
        .iter()
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect();
    format!("{path}?{}", q.join("&"))
}

/// Answer with a redirect unless the request was spelled exactly `target`
/// (byte for byte, so percent-encoded digits or reordered queries redirect).
fn require_canonical(uri: &Uri, target: &str) -> Result<(), Box<Response>> {
    let got = uri.path_and_query().map_or(uri.path(), |p| p.as_str());
    if got == target {
        Ok(())
    } else {
        Err(Box::new(redirect(target.to_string())))
    }
}

/// A metric or market name: lowercase letters, digits, `.`, `_`, `-` (so its
/// path segment needs no encoding and has one spelling).
fn check_name(s: &str, what: &str) -> Result<(), ApiError> {
    if !s.is_empty()
        && s.len() <= 128
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
    {
        Ok(())
    } else {
        Err(bad_request(format!(
            "{what} must be lowercase letters, digits, '.', '_' or '-'"
        )))
    }
}

/// Query of the content-addressable routes: `?root=` pins the root. Unknown
/// parameters are refused (M7-L39-F6).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinQuery {
    /// Expected root; the response is `immutable` only when it matches.
    pub root: Option<String>,
}

/// The pinned root, if any; must be canonical (it is part of a cache key).
fn pinned(root: Option<String>) -> Result<Option<Hash32>, ApiError> {
    root.map(|r| match parse_hash(&r, "root")? {
        (h, true) => Ok(h),
        (_, false) => Err(bad_request("root must be 0x and 64 lowercase hex digits")),
    })
    .transpose()
}

/// True when the URL names the root it is served from (content-addressed:
/// cacheable forever). A different pinned root is 409 `root_mismatch`.
fn check_pin(pin: Option<Hash32>, root: &FinalRoot) -> Result<bool, ApiError> {
    match pin {
        None => Ok(false),
        Some(p) if p == root.root => Ok(true),
        Some(_) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "root_mismatch",
            format!("the finalized root of this shard is {}", root.root),
        )),
    }
}

/// The x402 `resource.url`: the public base plus the canonical target.
fn resource_url(state: &ApiState, target: &str) -> String {
    format!("{}{}", state.public_url, target)
}

fn meta_of(state: &ApiState, round: u64, shard: &Hash32) -> Result<ShardMeta, ApiError> {
    state
        .catalog
        .read()
        .map_err(|_| internal("catalog lock"))?
        .shard(round, shard)
        .cloned()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown_shard", "no such shard"))
}

/// Serve-time checks that need no root and no row: rights, test labels, and
/// that the catalog's binding inputs derive the shard id.
fn admit(state: &ApiState, meta: &ShardMeta) -> Result<(), ApiError> {
    admit_rights(&meta.rights)?;
    if meta.test_labels && !state.config.allow_test_labels {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "test_labels",
            "this shard is pinned to a test label set; it is served only in dev mode",
        ));
    }
    // ROADMAP L14, THREATS L8-5: before any root read or 402.
    if state.config.rights == catalog::LabelRights::Fallback && meta.label_derived() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "label_rights_withheld",
            "this server runs with ONYX_RIGHTS=fallback: metrics derived from entity label sets (company flows, balances, reserves) are not served until the label sets' data rights are cleared",
        ));
    }
    if meta.binding.shard_id() != meta.shard {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "catalog_mismatch",
            "the catalog's binding inputs do not derive this shard id",
        ));
    }
    Ok(())
}

/// 451 unless the source's rights allow redistribution without terms
/// (shards and level-2 observations alike).
fn admit_rights(rights: &onyx_core::DataRights) -> Result<(), ApiError> {
    match rights.redistribution {
        Redistribution::Allowed => Ok(()),
        Redistribution::Restricted => Err(ApiError::new(
            StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS,
            "restricted_data",
            format!(
                "license {} restricts redistribution (terms {}); terms acceptance is not offered yet",
                rights.license, rights.terms
            ),
        )),
        Redistribution::Forbidden => Err(ApiError::new(
            StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS,
            "forbidden_data",
            "this source may not be redistributed",
        )),
    }
}

/// The serving check of a shard at `root` (ADR-0034 Amendment 1), before
/// any 402: `Ok(None)` when none applies.
fn gate_check(
    state: &ApiState,
    meta: &ShardMeta,
    root: &FinalRoot,
) -> Result<Option<gate::Cleared>, ApiError> {
    gate::cleared(&state.gates, meta, &root.root).map_err(|r| match r {
        gate::Refusal::Unavailable(g) => ApiError::new(
            StatusCode::FORBIDDEN,
            "serving_check_unavailable",
            format!("this shard needs the serving check {g}, which this server does not run"),
        ),
        gate::Refusal::Pending => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "serving_check_pending",
            "the serving check has not judged this shard's finalized root yet",
        )
        .with_header("retry-after", "15".to_string()),
        gate::Refusal::Failed(why) => ApiError::new(
            StatusCode::FORBIDDEN,
            "serving_check_failed",
            format!("the serving check refused this shard: {why}"),
        ),
    })
}

/// 403 `not_cross_checked` (THREATS RES-1): the row's entity has no
/// independent cross-check, so it is not served.
fn not_cross_checked(why: &str) -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "not_cross_checked",
        format!("this row is not served: {why}"),
    )
}

/// The `servingCheck` field for row `index`, or 403 for a refused row.
fn row_serving(c: Option<&gate::Cleared>, index: usize) -> Result<Option<Value>, ApiError> {
    c.map(|c| c.row(index).map_err(|why| not_cross_checked(&why)))
        .transpose()
}

/// The finalized root, from the cache or the root source.
async fn root_of(
    state: &ApiState,
    client: &ClientKey,
    round: u64,
    shard: &Hash32,
) -> Result<FinalRoot, ApiError> {
    let cached = |state: &ApiState| -> Option<Result<FinalRoot, ApiError>> {
        if let Some(r) = state.caches.root(round, shard) {
            return Some(Ok(r));
        }
        state
            .caches
            .root_miss(round, shard)
            .map(|m| Err(miss_error(&m)))
    };
    if let Some(r) = cached(state) {
        return r;
    }
    // One node lookup per shard at a time (AUD-1-F4); the others read its
    // cached result.
    let flight = state.caches.root_flight(round, shard);
    let result = {
        let _one = flight.lock().await;
        match cached(state) {
            Some(r) => r,
            None => fetch_root(state, client, round, shard).await,
        }
    };
    drop(flight);
    state.caches.end_root_flight(round, shard);
    result
}

/// A root lookup that reaches the node: charged to the client's root-lookup
/// budget first (AUD-1-F4), its answer cached either way.
async fn fetch_root(
    state: &ApiState,
    client: &ClientKey,
    round: u64,
    shard: &Hash32,
) -> Result<FinalRoot, ApiError> {
    let per_minute = state.config.root_lookups_per_minute;
    let scopes: Vec<(String, u32)> = rate_scopes(&client.0, per_minute)
        .into_iter()
        .map(|(k, n)| (format!("root:{k}"), n))
        .collect();
    if !state.caches.take_attempts(&scopes) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many uncached root lookups from this address",
        )
        .with_header("retry-after", (60 / per_minute.max(1)).max(1).to_string()));
    }
    let ttl = state.config.root_miss_ttl;
    match state.roots.finalized(round, shard).await {
        Ok(Some(r)) => {
            state
                .caches
                .put_root(round, *shard, r.clone(), state.config.root_ttl);
            Ok(r)
        }
        Ok(None) => {
            state
                .caches
                .put_root_miss(round, *shard, RootMiss::NotFinal, ttl);
            Err(miss_error(&RootMiss::NotFinal))
        }
        Err(e) => {
            let m = RootMiss::Unavailable(e.to_string());
            state.caches.put_root_miss(round, *shard, m.clone(), ttl);
            Err(miss_error(&m))
        }
    }
}

fn miss_error(m: &RootMiss) -> ApiError {
    match m {
        RootMiss::NotFinal => ApiError::new(
            StatusCode::NOT_FOUND,
            "not_finalized",
            "shard has no finalized root",
        ),
        RootMiss::Unavailable(e) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "root_source_unavailable",
            e.clone(),
        ),
    }
}

/// The binding of `root` with `leaf_count` rows; when the ledger recorded a
/// binding, it must be this one (else the catalog or the rows are wrong).
fn check_binding(meta: &ShardMeta, root: &FinalRoot, leaf_count: u64) -> Result<Hash32, ApiError> {
    let b = meta.binding.binding(root.root, leaf_count);
    if root.binding.is_some_and(|on_chain| on_chain != b) {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "binding_mismatch",
            "the ledger's binding differs from this shard's binding inputs",
        ));
    }
    Ok(b)
}

/// Catalog entry, admission checks and root of a shard: everything needed to
/// price a request without reading a row.
async fn target(
    state: &ApiState,
    client: &ClientKey,
    round: u64,
    shard: &Hash32,
) -> Result<(ShardMeta, FinalRoot), ApiError> {
    let meta = meta_of(state, round, shard)?;
    admit(state, &meta)?;
    let root = root_of(state, client, round, shard).await?;
    if let Some(n) = meta.leaf_count {
        check_binding(&meta, &root, n)?;
    }
    Ok((meta, root))
}

/// Rows of `root` with their tree, from the cache or the CAS, checked against
/// the root, the catalog's row count and the ledger's binding.
fn rows_of(state: &ApiState, meta: &ShardMeta, root: &FinalRoot) -> Result<Arc<Loaded>, ApiError> {
    let l = match state.caches.tree(&root.root) {
        Some(l) => l,
        None => {
            let rows = state.cas.rows(&root.root).ok_or_else(|| {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "rows_unavailable",
                    "rows for the finalized root are not stored here",
                )
            })?;
            let tree = MerkleTree::from_rows(&rows);
            if tree.root() != root.root {
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "rows_mismatch",
                    "stored rows do not hash to the finalized root",
                ));
            }
            let l = Arc::new(Loaded::new(rows, tree));
            state.caches.put_tree(root.root, l.clone());
            l
        }
    };
    let n = l.tree.leaf_count();
    if meta.leaf_count.is_some_and(|m| m != n) {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "rows_mismatch",
            "the catalog's row count differs from the stored rows",
        ));
    }
    check_binding(meta, root, n)?;
    Ok(l)
}

/// What a client is charged for `rows` rows: their [`price`], or 0 when the
/// server takes no payments.
fn charge(
    state: &ApiState,
    meta: &ShardMeta,
    root: &FinalRoot,
    rows: u64,
) -> Result<u64, ApiError> {
    if state.payments.is_none() {
        return Ok(0);
    }
    price(state, meta, root, rows)
}

/// Price of exporting `rows` rows: the dataset price (ADR-0025 item 4).
fn export_price(
    state: &ApiState,
    meta: &ShardMeta,
    root: &FinalRoot,
    rows: u64,
) -> Result<u64, ApiError> {
    state
        .catalog
        .read()
        .map(|c| c.export_price(meta, root.finalized_unix, (state.clock)(), rows))
        .map_err(|_| internal("catalog lock"))
}

/// What a request with an `ONYX-CONTRIBUTOR` proof pays for something priced
/// `price` (ADR-0025 item 3): `None` without a proof or a policy; the
/// discounted price for a valid, unused proof from an address with accepted
/// work in the window; otherwise 403 (bad proof, not a contributor) or 429
/// (the client's proof-check budget, or the address's read budget, is spent).
///
/// Order (REV-L1-F1, REV-L2-F1/F2): the signature is checked locally; a
/// proof already used is refused before anything is charged, so replaying it
/// costs its signer nothing; then the sending client's own per-IP budget is
/// charged, so throwaway keys cost the sender; an address recently found not
/// to be a contributor is refused from memory; any other lookup takes one
/// unit of a server-wide source budget (503 when spent) before the source is
/// asked. Only an address the source confirms (cached
/// [`cache::CONTRIBUTOR_STATUS_TTL`]) gets a read budget, in its own LRU that
/// unproven keys never enter; the proof is marked used right after its read
/// is charged.
async fn contributor_price(
    state: &ApiState,
    headers: &HeaderMap,
    client: &str,
    resource: &str,
    price: u64,
) -> Result<Option<u64>, Box<Response>> {
    let (Some(policy), Some(h)) = (
        state.contributors.as_ref(),
        headers.get(contrib::CONTRIBUTOR_HEADER),
    ) else {
        return Ok(None);
    };
    let now = (state.clock)();
    let refused = |code: &'static str, m: &str| -> Box<Response> {
        ApiError::new(StatusCode::FORBIDDEN, code, m).into()
    };
    let limited = |m: &'static str, per_minute: u32| -> Box<Response> {
        ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", m)
            .with_header("retry-after", (60 / per_minute.max(1)).max(1).to_string())
            .into()
    };
    let ledger = contrib::ledger_label(state.roots.ledger());
    let (who, key) = h
        .to_str()
        .map_err(|_| "malformed proof")
        .and_then(|h| contrib::verify(h, resource, &ledger, now, policy.max_skew_secs))
        .map_err(|m| refused("invalid_contributor_proof", m))?;
    if policy.proof_used(&key) {
        return Err(refused(
            "invalid_contributor_proof",
            "this proof was already used",
        ));
    }
    let checks = policy.checks_per_minute;
    let scopes: Vec<(String, u32)> = rate_scopes(client, checks)
        .into_iter()
        .map(|(k, n)| (format!("contrib-check:{k}"), n))
        .collect();
    if !state.caches.take_attempts(&scopes) {
        return Err(limited(
            "too many contributor proofs from this client",
            checks,
        ));
    }
    let not_contributor = || {
        refused(
            "not_a_contributor",
            "no accepted work from this address in the window",
        )
    };
    let confirmed = if state.caches.contributor_confirmed(&who) {
        false
    } else if policy.known_not_contributor(&who, now) {
        return Err(not_contributor());
    } else {
        if !policy.take_source_call(now) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "contributor_checks_busy",
                "too many contributor checks right now",
            )
            .with_header("retry-after", "1".into())
            .into());
        }
        match policy.source.is_contributor(who, now).await {
            Ok(true) => true,
            Ok(false) => {
                policy.remember_not_contributor(who, now);
                return Err(not_contributor());
            }
            Err(e) => {
                return Err(ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "root_source_unavailable",
                    format!("contributor status: {e}"),
                )
                .into())
            }
        }
    };
    let per_minute = policy.reads_per_minute;
    if !state
        .caches
        .take_contributor_read(who, 1, per_minute, policy.rows_per_day, confirmed)
    {
        return Err(limited(
            "too many contributor reads from this address",
            per_minute,
        ));
    }
    if !policy.use_proof(key, now) {
        return Err(refused(
            "invalid_contributor_proof",
            "this proof was already used",
        ));
    }
    Ok(Some(policy.discounted(price)))
}

/// Price of `rows` rows (M7-F10: an unreadable catalog is an error, never free).
fn price(state: &ApiState, meta: &ShardMeta, root: &FinalRoot, rows: u64) -> Result<u64, ApiError> {
    state
        .catalog
        .read()
        .map(|c| c.price(meta, root.finalized_unix, (state.clock)(), rows))
        .map_err(|_| internal("catalog lock"))
}

/// Everything a client needs to check a response against the ledger without
/// trusting this server (M7-F2, docs/spec/delivery.md §Verification): where
/// to call `getFinalizedRoot`, what it must return, and the inputs that
/// recompute the shard id and the binding.
fn bundle(meta: &ShardMeta, root: &FinalRoot, leaf_count: u64, ledger: LedgerKind) -> Value {
    let b = &meta.binding;
    let (kind, chain_id, contract) = match ledger {
        LedgerKind::OffChain => ("off-chain", Value::Null, Value::Null),
        LedgerKind::Chain { chain_id, contract } => {
            ("chain", json!(chain_id), json!(hex0x(&contract)))
        }
    };
    json!({
        "ledger": kind,
        "chainId": chain_id,
        "contract": contract,
        "call": roots::GET_FINALIZED_ROOT,
        "round": meta.round,
        "shard": meta.shard.to_string(),
        "root": root.root.to_string(),
        "leafCount": leaf_count,
        "binding": b.binding(root.root, leaf_count).to_string(),
        "bindingInputs": {
            "recipeId": b.recipe_id.to_string(),
            "runtimeHash": b.runtime_hash.to_string(),
            "labelSetHash": b.label_set_hash.to_string(),
            "sourceHash": b.source_hash.to_string(),
            "schemaId": b.schema_id.to_string(),
            "rightsHash": b.rights_hash.to_string(),
        },
        "shardInputs": {
            "paramsHash": b.params_hash.to_string(),
            "registryRoot": b.registry_root.to_string(),
        },
        // Preimages a client hashes itself (M7-L40-F1): `keccak256(sourceRef)`
        // must be `sourceHash` (it pins the source and the snapshot, which
        // binds `asOf`), and `keccak256(rights)` must be `rightsHash` (it
        // binds `license` and `attribution`).
        "sourceRef": hex0x(&borsh::to_vec(&meta.source).unwrap_or_default()),
        "rights": hex0x(&borsh::to_vec(&meta.rights).unwrap_or_default()),
        "finalizedAt": root.finalized_at,
        "finalizedUnix": root.finalized_unix,
        "tx": root.tx,
    })
}

/// Index of the row whose key is `subject` (the UTF-8 bytes of the row key,
/// as recipes write it: `btc-entity-balance` keys rows by the entity id).
/// One lookup in the per-root key index (M7-L39-F3).
fn find_subject(l: &Loaded, subject: &str) -> Option<usize> {
    l.by_key.get(subject.as_bytes()).copied()
}

fn row_body(
    meta: &ShardMeta,
    root: &FinalRoot,
    l: &Loaded,
    index: usize,
    ledger: LedgerKind,
) -> Result<Value, ApiError> {
    let proof = l.tree.proof(index as u64).ok_or_else(row_not_found)?;
    let n = l.tree.leaf_count();
    Ok(json!({
        "round": meta.round,
        "shard": meta.shard.to_string(),
        "index": index,
        "row": hex0x(&l.rows[index]),
        "leafCount": n,
        "proof": {
            "index": proof.index,
            "leafCount": proof.leaf_count,
            "siblings": proof.siblings.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        },
        "root": root.root.to_string(),
        "finalizedAt": root.finalized_at,
        "recipeId": nonzero(meta.binding.recipe_id),
        "labelSet": nonzero(meta.binding.label_set_hash),
        "tx": root.tx,
        "license": meta.rights.license,
        "attribution": meta.rights.attribution,
        "snapshotUnix": meta.snapshot_unix,
        "proofLevel": meta.proof_level.number(),
        "ladder": row_ladder(meta, root, index, &l.rows[index])
            .map(|x| ladder::to_json(&x, ladder::ROW_RECORD)),
        "verify": bundle(meta, root, n, ledger),
    }))
}

/// A row's one-step ladder (ADR-0048): the shard's level (the catalog's,
/// floored by the registry's attestation when one is pinned) on the shard's
/// finalized root. A shard's level is attested once, so a row never rises on
/// this node.
fn row_ladder(
    meta: &ShardMeta,
    root: &FinalRoot,
    index: usize,
    row: &[u8],
) -> Option<onyx_core::Ladder> {
    onyx_core::Ladder::new(
        onyx_core::ladder::value_key(
            &onyx_core::ladder::row_key(meta.round, &meta.shard, index as u64),
            &keccak256(row),
        ),
        onyx_core::ladder::Step {
            level: meta.proof_level,
            evidence: root.root,
            at: root.finalized_unix,
        },
    )
    .ok()
}

fn etag_matches(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"))
}

/// Cache policy (M7-F1): a URL that pins its root is content-addressed and
/// `immutable`; any other success must revalidate (`no-cache` + ETag). Paid
/// responses are `private`. Errors are `no-store` ([`ApiError`]).
fn cache_control(private: bool, immutable: bool) -> HeaderValue {
    HeaderValue::from_static(match (private, immutable) {
        (false, true) => "public, max-age=31536000, immutable",
        (true, true) => "private, max-age=31536000, immutable",
        (false, false) => "public, no-cache",
        (true, false) => "private, no-cache",
    })
}

fn respond(
    body: Value,
    etag: &str,
    private: bool,
    immutable: bool,
    payment: Option<&str>,
) -> Response {
    let mut r = Json(body).into_response();
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(etag) {
        h.insert(header::ETAG, v);
    }
    h.insert(header::CACHE_CONTROL, cache_control(private, immutable));
    if let Some(v) = payment.and_then(|p| HeaderValue::from_str(p).ok()) {
        h.insert(HeaderName::from_static(x402::PAYMENT_RESPONSE), v);
    }
    r
}

fn not_modified(etag: &str, private: bool, immutable: bool) -> Response {
    let mut r = StatusCode::NOT_MODIFIED.into_response();
    if let Ok(v) = HeaderValue::from_str(etag) {
        r.headers_mut().insert(header::ETAG, v);
    }
    r.headers_mut()
        .insert(header::CACHE_CONTROL, cache_control(private, immutable));
    r
}

/// Whether a contributor proof may lower a request's price.
#[derive(Clone, Copy)]
enum Pass {
    /// One row (a proof, a metric, a settlement value).
    Row,
    /// An export: never (REV-L1-F5).
    Never,
}

/// Whether a request is charged, and the verified payment if it is.
enum Gate {
    Free,
    /// A contributor's free read of fresh data: private, no payment.
    Contributor,
    /// Paid with a prepaid voucher (ROADMAP S7): taken and journalled in [`finish`], before
    /// the body leaves.
    Prepaid {
        voucher: prepaid::Voucher,
        price: u64,
        /// The request key ([`payment_key`] over the header): its identical retry is a replay.
        key: Hash32,
    },
    Paid {
        req: Box<FacilitatorRequest>,
        claim: cache::ClaimGuard,
        /// EIP-3009 payer and nonce, for the usage receipt.
        payer: [u8; 20],
        nonce: [u8; 32],
        amount: u64,
        /// The authorization's `validAfter`: the token refuses any use before it.
        valid_after: u64,
        /// The authorization's `validBefore`: after it (and a grace) an
        /// unused authorization can never move money (OPS-L2-F1 intents).
        valid_before: u64,
        /// The longest authorization life this server accepted (seconds):
        /// bounds the span the intent's use is searched in (OPS-L4-F1).
        max_life: u64,
        /// The facilitator slot taken for verify, held through the settle
        /// (OPS-L2-F2); none when nothing is sent or read (a proven retry).
        slot: Option<tokio::sync::OwnedSemaphorePermit>,
        /// A new payment, or the identical retry of one.
        attempt: Attempt,
    },
}

/// What a paid request's settle does.
enum Attempt {
    /// A new payment: a new intent, then the settle.
    Fresh,
    /// The identical retry of a paid request whose settle ended unknown or
    /// refused, its intent still open (M7L3-R1-1): no new intent and no new
    /// settle; the authorization check decides (it re-sends only an unused
    /// authorization, at most [`intents::MAX_RESENDS`] times in all).
    Retry,
    /// The identical retry of a paid request whose intent the resolver
    /// recovered (M7L3-R2-1): served from that proof (the transaction that
    /// moved the payment into `OnyxRevenue`; the receipt is on disk), with
    /// no settle and no check.
    Proven,
}

/// Settled-payment key: the exact header bytes, for the exact resource, from
/// the same client (M7-L39-F5: a leaked header is not a bearer token).
fn payment_key(sig: &[u8], resource: &str, client: &str) -> Hash32 {
    let mut b = sig.to_vec();
    b.push(0);
    b.extend_from_slice(resource.as_bytes());
    b.push(0);
    b.extend_from_slice(client.as_bytes());
    keccak256(&b)
}

fn prepaid_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "prepaid_unavailable",
        "the server cannot check or record prepaid balances right now; nothing was charged",
    )
    .with_header("retry-after", "5".into())
}

fn facilitator_down(message: impl Into<String>) -> Response {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "facilitator_unavailable",
        message,
    )
    .with_header("retry-after", "5".into())
    .into_response()
}

fn replay(c: cache::CachedResponse) -> Box<Response> {
    Box::new(respond(
        c.body,
        &c.etag,
        true,
        c.immutable,
        Some(&c.payment_response),
    ))
}

/// What a 402 offers besides its price: the resource's description and,
/// for level 2, the known limits of what is sold (ROADMAP B4,
/// [`observations::limits_json`]), in the body and in `extensions`.
struct Offer<'a> {
    description: &'a str,
    limits: Option<Value>,
}

impl<'a> Offer<'a> {
    fn of(description: &'a str) -> Self {
        Offer {
            description,
            limits: None,
        }
    }
}

/// Phase one of the x402 v2 `exact` flow (spec §6.1): 402 challenge, replay
/// of an already settled identical request from the same client (M7-F5,
/// M7-L39-F5), waiting for a settlement of it still in flight (M7-L39-F1),
/// rate limit (M7-F4), the payment must be for exactly this resource and
/// these requirements, local EIP-3009 checks, then facilitator `verify` under
/// a deadline and a server-wide cap (M7-L39-F4). No row has been read yet.
async fn authorize(
    state: &ApiState,
    headers: &HeaderMap,
    client: &str,
    resource: &str,
    offer: Offer<'_>,
    price: u64,
    pass: Pass,
) -> Result<Gate, Box<Response>> {
    let Offer {
        description,
        limits,
    } = offer;
    if state.payments.is_none() || price == 0 {
        return Ok(Gate::Free);
    }
    // REV-L1-F5: a contributor proof is a bearer token its signer can hand to
    // anyone, so it never buys a whole shard; on an export the header is
    // ignored and the export is paid (free exports keep their own budget).
    let pass_price = match pass {
        Pass::Row => contributor_price(state, headers, client, resource, price).await?,
        Pass::Never => None,
    };
    let price = match pass_price {
        Some(0) => return Ok(Gate::Contributor),
        Some(p) => p,
        None => price,
    };
    let Some(pay) = state.payments.as_ref() else {
        return Ok(Gate::Free);
    };
    // ROADMAP S7: a prepaid voucher instead of an x402 payment. Its account is read here (async);
    // the voucher is taken and journalled in `finish`, once the answer's book is known.
    if let Some(h) = headers.get(prepaid_gate::PREPAID_VOUCHER) {
        let Some(pp) = state.prepaid.as_ref() else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "prepaid_not_offered",
                "this server takes x402 payments only",
            )
            .into());
        };
        let Some(voucher) = h.to_str().ok().and_then(prepaid_gate::parse_voucher) else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_prepaid",
                "malformed ONYX-PREPAID-VOUCHER header",
            )
            .into());
        };
        if pp.refresh(voucher.buyer, (state.clock)()).await.is_err() {
            return Err(prepaid_unavailable().into());
        }
        let key = payment_key(h.as_bytes(), resource, client);
        return Ok(Gate::Prepaid {
            voucher,
            price,
            key,
        });
    }
    let requirements = pay.terms.requirements(price);
    let challenge = |error: &str| -> Box<Response> {
        let pr = PaymentRequired {
            x402_version: x402::X402_VERSION,
            error: Some(error.into()),
            resource: x402::ResourceInfo {
                url: resource.into(),
                description: Some(description.into()),
                mime_type: Some("application/json".into()),
            },
            accepts: vec![requirements.clone()],
            extensions: limits.as_ref().map(
                |l| json!({ (observations::LIMITS_EXTENSION): observations::limits_extension(l) }),
            ),
        };
        let mut e = ApiError::new(StatusCode::PAYMENT_REQUIRED, "payment_required", error)
            .with_header(x402::PAYMENT_REQUIRED, x402::encode_header(&pr));
        if let Some(l) = &limits {
            e = e.with_field("limits", l.clone());
        }
        e.into()
    };
    let Some(sig) = headers.get(x402::PAYMENT_SIGNATURE) else {
        return Err(challenge("PAYMENT-SIGNATURE header is required"));
    };
    let key = payment_key(sig.as_bytes(), resource, client);
    if let Some(c) = state.caches.replay_settled(&key) {
        return Err(replay(c));
    }
    // A failed write of the receipt log or of the facilitator's state means
    // a new payment's receipt might be lost: refuse new payments before
    // anything is verified (fail closed, REV-L3-F5). Replays of payments
    // already settled are still answered above.
    if state_write_errors(state) > 0 || state.receipts.is_full() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "receipt_log_unavailable",
            "the server cannot record payments durably right now; nothing was charged",
        )
        .into());
    }
    // Claim the payment atomically (M7-L40-F4): identical concurrent
    // requests do not both verify and settle; the second waits for the
    // first and replays its response. At most two rounds: a claim whose
    // owner ended without a response (refused, error) is claimed afresh.
    let mut claim = None;
    for _ in 0..2 {
        match state.caches.claim(key) {
            cache::Claimed::Mine(g) => {
                claim = Some(g);
                break;
            }
            cache::Claimed::Wait(mut rx) => {
                let _ = rx.wait_for(|done| *done).await;
                if let Some(c) = state.caches.replay_settled(&key) {
                    return Err(replay(c));
                }
            }
            cache::Claimed::Full => {
                return Err(Box::new(facilitator_down("too many settlements in flight")))
            }
        }
    }
    let Some(mut claim) = claim else {
        return Err(Box::new(facilitator_down("this payment is busy; retry")));
    };
    let per_minute = state.config.payment_attempts_per_minute;
    if !state.caches.take_attempts(&rate_scopes(client, per_minute)) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many payment attempts from this address",
        )
        .with_header("retry-after", (60 / per_minute.max(1)).max(1).to_string())
        .into());
    }
    let payload: PaymentPayload = match sig.to_str().ok().map(x402::decode_header) {
        Some(Ok(p)) => p,
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_payment",
                "malformed PAYMENT-SIGNATURE header",
            )
            .into())
        }
    };
    if payload.x402_version != x402::X402_VERSION {
        return Err(challenge("unsupported x402Version"));
    }
    // The client must have accepted exactly what we offer now (amount,
    // asset, recipient, network): a facilitator only checks the payload
    // against the requirements we hand it. Checked before the proof and
    // retry paths too (M7L3-R3-5): a price changed since the first request
    // is not served at the old price.
    if payload.accepted != requirements {
        return Err(challenge("payment does not match the requirements"));
    }
    // M7L3-R2-1: the identical retry of a paid request whose intent the
    // resolver recovered (the transfer into OnyxRevenue of the exact value
    // proven, the receipt written). Its verify would fail on a used nonce:
    // it is served from that proof instead, with no verify, settle or check,
    // within PROVEN_WINDOW_SECS of the recovery. No new unit is taken.
    if let Some((i, _)) = state.intents.proven(&key, (state.clock)()) {
        let requirements = requirements.clone();
        return Ok(Gate::Paid {
            req: Box::new(FacilitatorRequest {
                x402_version: x402::X402_VERSION,
                payment_payload: payload,
                payment_requirements: requirements,
            }),
            claim,
            payer: i.receipt.payer,
            nonce: i.receipt.nonce,
            amount: i.receipt.amount,
            valid_after: i.valid_after,
            valid_before: i.valid_before,
            max_life: 0,
            slot: None,
            attempt: Attempt::Proven,
        });
    }
    // M7L3-R1-1: the identical retry (same header bytes, resource and
    // client) of a paid request whose settle ended unknown or refused, while
    // its intent is open. Its verify would now fail on a used nonce, and a
    // new settle would be refused, though the payer may have paid: the
    // authorization check decides instead (served only on a proven transfer
    // into OnyxRevenue of the exact value). No new unit is taken: the first
    // request's stays spent.
    if state.checks_unknown_settles() {
        if let Some(i) = state.intents.awaiting(&key) {
            let Ok(slot) = state.facilitator_slots.clone().try_acquire_owned() else {
                return Err(Box::new(facilitator_down(
                    "too many payments being checked; retry shortly",
                )));
            };
            let requirements = requirements.clone();
            let max_life = requirements
                .max_timeout_seconds
                .saturating_add(state.config.authorization_slack.as_secs());
            return Ok(Gate::Paid {
                req: Box::new(FacilitatorRequest {
                    x402_version: x402::X402_VERSION,
                    payment_payload: payload,
                    payment_requirements: requirements,
                }),
                claim,
                payer: i.receipt.payer,
                nonce: i.receipt.nonce,
                amount: i.receipt.amount,
                valid_after: i.valid_after,
                valid_before: i.valid_before,
                max_life,
                slot: Some(slot),
                attempt: Attempt::Retry,
            });
        }
    } else if state.intents.awaiting(&key).is_some() {
        // SOAK-1 (L6 soak): without the authorization check (a
        // self-settling facilitator does its own, ADR-0026), the identical
        // retry of a paid request whose settle ended unknown used to take the
        // fresh path and get 409 `payment_in_use` "for another request ...
        // added no charge", though it is this very request and its payer
        // may have been charged: a client that believed it signed a new
        // authorization and paid twice. It is told to retry instead: the
        // resolver judges the intent from the chain every pass, and once the
        // transfer into OnyxRevenue is proven this retry is served from that
        // proof (M7L3-R2-1) with no new charge.
        return Err(Box::new(
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "payment_pending",
                "the settle of this identical request ended with an unknown outcome and is being resolved from the chain; retry this same request: it is served once the payment into OnyxRevenue is proven, and nothing more is charged",
            )
            .with_header("retry-after", PAYMENT_PENDING_RETRY_SECS.to_string())
            .into_response(),
        ));
    }
    // A payment is for one URL: it cannot buy a different row or shard.
    if payload.resource.as_ref().map(|r| r.url.as_str()) != Some(resource) {
        return Err(challenge("payment is not for this resource"));
    }
    let now = (state.clock)();
    let auth = match x402::exact_evm::precheck(&payload.payload, &requirements, now) {
        Ok(a) => a,
        Err(reason) => return Err(challenge(reason)),
    };
    // An authorization valid far into the future would keep its settle
    // intent open that long (OPS-L3-F4): x402 clients sign `validBefore =
    // now + maxTimeoutSeconds`, so anything later than that plus the slack
    // is refused before verify.
    let max_life = requirements
        .max_timeout_seconds
        .saturating_add(state.config.authorization_slack.as_secs());
    let latest = now.saturating_add(max_life);
    if auth.valid_before > latest {
        return Err(challenge(
            "invalid_exact_evm_payload_authorization_valid_before_too_late",
        ));
    }
    // ... and one that expires before our settle can be mined is refused
    // too (OPS-L4-F1): a settle that misses `validBefore` is refused by the
    // token. Bounded by `maxTimeoutSeconds`, so a client signing the usual
    // `now + maxTimeoutSeconds` always passes.
    let soonest = now.saturating_add(
        state
            .settle_budget()
            .as_secs()
            .saturating_add(SETTLE_MARGIN_SECS)
            .min(requirements.max_timeout_seconds),
    );
    if auth.valid_before < soonest {
        return Err(challenge(
            "invalid_exact_evm_payload_authorization_valid_before",
        ));
    }
    // A payer whose payments keep ending unsettled (probing 404s, refused
    // settles) is not sent to the facilitator again for a while
    // (M7-L40-F2/F3). The unit is taken now, under one lock, so concurrent
    // requests cannot overspend it; it comes back if the payment settles, if
    // the facilitator refuses it at verify, or if verify fails (M7-L41-F5,
    // M7-L42-F1/F2). It stays spent on a 404 after verify, a refused settle,
    // a settle that times out or fails (outcome unknown, M7-L47-F3) and an
    // abandoned request.
    let per_hour = state.config.unsettled_per_hour;
    if !state.caches.reserve_payer(&auth.from, per_hour) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "payer_limited",
            "too many verified payments from this payer ended without settling",
        )
        .with_header("retry-after", "600".into())
        .into());
    }
    claim.reserved(auth.from, per_hour);
    // One slot covers this payment's verify AND its settle (OPS-L2-F2): the
    // settle never queues for a slot outside its timeout, so a stop's settle
    // budget is a true bound.
    let Ok(slot) = state.facilitator_slots.clone().try_acquire_owned() else {
        claim.refund();
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "facilitator_busy",
            "too many payments being checked; retry shortly",
        )
        .with_header("retry-after", "1".into())
        .into());
    };
    let req = FacilitatorRequest {
        x402_version: x402::X402_VERSION,
        payment_payload: payload,
        payment_requirements: requirements.clone(),
    };
    match tokio::time::timeout(
        state.config.facilitator_timeout,
        pay.facilitator.verify(&req),
    )
    .await
    {
        Ok(Ok(v)) if v.is_valid => {
            // From here on, ending without a settle keeps the unit spent.
            Ok(Gate::Paid {
                req: Box::new(req),
                claim,
                payer: auth.from,
                nonce: auth.nonce,
                amount: price,
                valid_after: auth.valid_after,
                valid_before: auth.valid_before,
                max_life,
                slot: Some(slot),
                attempt: Attempt::Fresh,
            })
        }
        Ok(Ok(v)) => {
            // Refused by the facilitator (a replayed, already settled header;
            // an unfunded key): nothing was served or charged, so the unit
            // comes back and replays cannot drain the real payer's budget
            // (M7-L42-F2).
            claim.refund();
            Err(challenge(
                &v.invalid_reason.unwrap_or_else(|| "invalid payment".into()),
            ))
        }
        Ok(Err(e)) => {
            claim.refund();
            Err(Box::new(facilitator_down(e.to_string())))
        }
        Err(_) => {
            claim.refund();
            Err(Box::new(facilitator_down("facilitator verify timed out")))
        }
    }
}

/// How a settlement ended.
enum Settled {
    Ok(String),
    Refused(x402::SettleResponse),
    Down(String),
}

impl ApiState {
    /// The longest one settle may run: the facilitator patience
    /// ([`SETTLE_PATIENCE`] times `facilitator_timeout`), or a self-settling
    /// facilitator's own budget when that is longer (M8-L58-F2).
    pub fn settle_budget(&self) -> Duration {
        let patience = self.config.facilitator_timeout * SETTLE_PATIENCE;
        self.payments
            .as_ref()
            .and_then(|p| p.facilitator.settle_budget())
            .map_or(patience, |b| b.max(patience))
    }

    /// The longest one authorization check runs ([`settle_check`]),
    /// its re-reads included.
    pub fn settle_check_budget(&self) -> Duration {
        self.config.facilitator_timeout * SETTLE_PATIENCE / SETTLE_CHECK_SHARE
    }

    /// The wait before the check that follows a re-send, and between the
    /// re-reads of one check (M7L3-R1-1): [`ApiConfig::settle_recheck_wait`],
    /// at most [`ApiState::settle_check_budget`].
    pub fn settle_recheck_wait(&self) -> Duration {
        self.config
            .settle_recheck_wait
            .min(self.settle_check_budget())
    }

    /// True when an unknown settle is followed by the authorization check:
    /// a check is configured and the facilitator is not self-settling (which
    /// does its own, ADR-0026).
    fn checks_unknown_settles(&self) -> bool {
        self.settle_check.is_some()
            && self
                .payments
                .as_ref()
                .is_some_and(|p| !p.facilitator.credits_on_settle())
    }

    /// The longest one settle task runs: the settle, and with the
    /// authorization check up to two checks, one re-send and the wait
    /// before the second check (M7L3-R1-1).
    pub fn settle_task_budget(&self) -> Duration {
        if self.checks_unknown_settles() {
            self.settle_budget() * 2 + self.settle_check_budget() * 2 + self.settle_recheck_wait()
        } else {
            self.settle_budget()
        }
    }

    /// The longest a clean stop takes (OPS-L1-F1): requests in flight end
    /// within `request_timeout`, and the last settle one of them started
    /// ends within [`ApiState::settle_task_budget`] after that. A
    /// supervisor's stop timeout must be longer than this plus its own slack.
    pub fn stop_budget(&self) -> Duration {
        self.config.request_timeout + self.settle_task_budget()
    }
}

/// Counts settle tasks still running, so a stop can wait for them
/// (OPS-L1-F1). Each task holds a [`SettleGuard`]; dropping it (finish,
/// error, panic, cancellation) counts the task out.
#[derive(Debug, Default)]
pub struct SettleTasks {
    live: std::sync::atomic::AtomicUsize,
    idle: tokio::sync::Notify,
}

/// One running settle task (see [`SettleTasks`]).
#[derive(Debug)]
pub struct SettleGuard(Arc<SettleTasks>);

impl SettleTasks {
    /// Count a task in until the returned guard is dropped.
    pub fn enter(self: &Arc<Self>) -> SettleGuard {
        self.live.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        SettleGuard(self.clone())
    }

    /// Settle tasks running now.
    pub fn live(&self) -> usize {
        self.live.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Wait until no settle task runs, at most `limit`. True when none runs.
    pub async fn wait_idle(&self, limit: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            // Registered before the check, so a task ending in between
            // still wakes this wait.
            notified.as_mut().enable();
            if self.live() == 0 {
                return true;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.live() == 0;
            }
        }
    }
}

impl Drop for SettleGuard {
    fn drop(&mut self) {
        if self
            .0
            .live
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst)
            == 1
        {
            self.0.idle.notify_waiters();
        }
    }
}

/// Phase two of a prepaid answer: take the voucher and journal it off the async threads, then
/// release the body; refused or not durable, nothing leaves.
#[allow(clippy::too_many_arguments)]
async fn finish_prepaid(
    state: &Arc<ApiState>,
    voucher: prepaid::Voucher,
    price: u64,
    key: Hash32,
    bill: (u64, Hash32),
    body: Value,
    etag: &str,
    immutable: bool,
) -> Response {
    let Some(pp) = state.prepaid.clone() else {
        return internal("prepaid is not configured").into_response();
    };
    let now = (state.clock)();
    let taken =
        tokio::task::spawn_blocking(move || pp.charge(voucher, u128::from(price), bill, key, now))
            .await;
    match taken {
        Ok(Ok(c)) => {
            let mut r = respond(body, etag, true, immutable, None);
            if let Ok(v) =
                HeaderValue::from_str(&format!("total={}; spendable={}", c.total, c.spendable))
            {
                r.headers_mut()
                    .insert(HeaderName::from_static(prepaid_gate::PREPAID_RESPONSE), v);
            }
            r
        }
        Ok(Err(prepaid_gate::ChargeError::Refused(e))) => {
            let mut err = ApiError::new(
                StatusCode::PAYMENT_REQUIRED,
                "prepaid_refused",
                e.to_string(),
            );
            if let prepaid::PrepaidError::WrongTotal { last, price } = e {
                err = err
                    .with_field("last", json!(last.to_string()))
                    .with_field("price", json!(price.to_string()));
            }
            err.into_response()
        }
        Ok(Err(prepaid_gate::ChargeError::Unavailable)) | Err(_) => {
            prepaid_unavailable().into_response()
        }
    }
}

/// Phase two: settle and release the body only on success. Settlement runs
/// in its own task (M7-L39-F1): if the request deadline fires first, the task
/// still finishes and records the response, so the identical retry from the
/// same client gets it (and a retry arriving meanwhile waits for it).
async fn finish(
    state: &Arc<ApiState>,
    gate: Gate,
    bill: (u64, Hash32),
    body: Value,
    etag: String,
    immutable: bool,
) -> Response {
    let (req, mut claim, receipt, (valid_after, valid_before, max_life), slot, attempt) = match gate
    {
        Gate::Free => return respond(body, &etag, false, immutable, None),
        Gate::Contributor => return respond(body, &etag, true, immutable, None),
        Gate::Prepaid {
            voucher,
            price,
            key,
        } => return finish_prepaid(state, voucher, price, key, bill, body, &etag, immutable).await,
        Gate::Paid {
            req,
            claim,
            payer,
            nonce,
            amount,
            valid_after,
            valid_before,
            max_life,
            slot,
            attempt,
        } => (
            req,
            claim,
            receipts::Receipt {
                round: bill.0,
                shard: bill.1,
                amount,
                payer,
                nonce,
            },
            (valid_after, valid_before, max_life),
            slot,
            attempt,
        ),
    };
    let Some(pay) = state.payments.as_ref() else {
        return internal("payments are not configured").into_response();
    };
    // The intent is on disk BEFORE the settle call (OPS-L2-F1): whatever
    // happens next (a timeout, an error, a crash, a kill), the resolver can
    // tell from the chain whether this payment reached OnyxRevenue. If it
    // cannot be made durable, nothing is settled and the unit comes back.
    let intent = intents::Intent::new(
        receipt,
        valid_after,
        valid_before,
        (state.clock)(),
        max_life,
    );
    let in_use = || {
        ApiError::new(
            StatusCode::CONFLICT,
            "payment_in_use",
            "this authorization is already being settled for another request; this request added no charge",
        )
        .into_response()
    };
    let begun = match attempt {
        Attempt::Fresh => {
            intents::begin_off_thread(&state.intents, intent, Some(claim.key())).await
        }
        // The intent written for this very request (M7L3-R1-1): taken over,
        // unless it was resolved or taken meanwhile.
        Attempt::Retry => match state.intents.adopt(&claim.key(), &receipt) {
            Some(id) => Ok(id),
            None => return in_use(),
        },
        // The resolver's proof for this very request and receipt
        // (M7L3-R2-1): served like a settled payment (stored for its
        // replays first), with the transaction that paid. Its receipt is on
        // disk already. Unless it was served or expired meanwhile.
        // The proof is deleted from the journal before the answer leaves
        // (M7L3-R3-2): a restart cannot serve it again.
        Attempt::Proven => {
            let key = claim.key();
            let taken =
                intents::take_proven_off_thread(&state.intents, key, receipt, (state.clock)())
                    .await;
            let Some(tx) = taken else {
                if state.intents.write_errors() > 0 {
                    return ApiError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "settle_intent_unavailable",
                        "the server cannot record this payment durably right now; retry later",
                    )
                    .into_response();
                }
                return in_use();
            };
            let header = x402::encode_header(&x402::SettleResponse {
                success: true,
                error_reason: None,
                payer: Some(hex0x(&receipt.payer)),
                transaction: tx,
                network: pay.terms.network.clone(),
            });
            state.caches.put_settled(
                key,
                cache::CachedResponse {
                    body: body.clone(),
                    etag: etag.clone(),
                    payment_response: header.clone(),
                    immutable,
                },
            );
            claim.settled();
            return respond(body, &etag, true, immutable, Some(&header));
        }
    };
    let retry = matches!(attempt, Attempt::Retry);
    let intent_id = match begun {
        Ok(id) => id,
        Err(intents::BeginError::PayerBusy) => {
            claim.refund();
            return ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "payer_busy",
                "this payer has too many payments waiting to be resolved; nothing was charged",
            )
            .with_header("retry-after", "60".into())
            .into_response();
        }
        Err(intents::BeginError::InUse) => {
            claim.refund();
            return in_use();
        }
        Err(intents::BeginError::ClockSkew) => {
            claim.refund();
            return ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "clock_skew",
                "the server's clock disagrees with the chain's; nothing was charged",
            )
            .with_header("retry-after", "60".into())
            .into_response();
        }
        Err(_) => {
            claim.refund();
            return ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "settle_intent_unavailable",
                "the server cannot record this payment durably right now; nothing was charged",
            )
            .into_response();
        }
    };
    let mut guard = Some(intents::IntentGuard::new(state.intents.clone(), intent_id));
    let st = state.clone();
    let cached_body = body.clone();
    let cached_etag = etag.clone();
    // The claim moves into the task: whatever happens there (success,
    // refusal, timeout, panic), dropping it releases the key and wakes
    // waiting retries (M7-L40-F5). The guard counts the task until it ends,
    // so a stop waits for it (OPS-L1-F1). The facilitator slot taken at
    // verify moves in too, so the settle never queues (OPS-L2-F2); the
    // intent guard hands the intent to the resolver unless the task ends it.
    let running = st.settle_tasks.enter();
    let task = tokio::spawn(async move {
        let _running = running;
        let _slot = slot;
        let key = claim.key();
        let outcome = match st.payments.as_ref() {
            None => Settled::Down("payments are not configured".into()),
            Some(pay) => {
                // A self-settling facilitator bounds its own settle; never
                // cut it short, or its pending handling would not run
                // (M8-L58-F2). The facilitator's HTTP client waits as long
                // (OPS-L2-F1).
                let patience = st.settle_budget();
                let mut outcome = if retry {
                    // No new settle: the check below decides (M7L3-R1-1).
                    Err(
                        "the earlier settle of this authorization ended unknown or refused"
                            .to_string(),
                    )
                } else {
                    settle_call(pay, &req, bill, patience).await
                };
                // M7-F5 residual: before giving up on (or re-sending) a
                // settle whose outcome is unknown, ask the token.
                if let (Err(why), Some(check)) = (&outcome, &st.settle_check) {
                    if st.checks_unknown_settles() {
                        outcome = after_unknown(
                            &st,
                            pay,
                            check.as_ref(),
                            (&req, bill),
                            (&receipt, intent_id),
                            (valid_after, valid_before),
                            why.clone(),
                        )
                        .await;
                    }
                }
                match outcome {
                    Ok(s) if s.success => {
                        let header = x402::encode_header(&s);
                        // The receipt is on disk before anything else sees
                        // the payment as settled (REV-L3-F7): before the
                        // cache answers a waiting retry and before the
                        // response leaves. A self-settled payment credited
                        // its shard in the same transaction (ADR-0026): no
                        // receipt. Then the intent is done.
                        // The intent ends only once the receipt is durable
                        // (OPS-L3-F1): if the receipt log cannot keep it,
                        // the intent stays for the resolver, which records
                        // it once the log is healthy (after a restart).
                        let durable = pay.facilitator.credits_on_settle()
                            || receipts::record_off_thread(&st.receipts, receipt).await;
                        // Not durable: the request is served all the same,
                        // so its intent drops the request key (M7L3-R3-4):
                        // after a restart its identical retry is not served
                        // again.
                        if let Some(g) = guard.take() {
                            if durable {
                                g.ended().await;
                            } else {
                                g.served_without_receipt().await;
                            }
                        }
                        // Stored before the claim is released, so a waiting
                        // retry finds it.
                        st.caches.put_settled(
                            key,
                            cache::CachedResponse {
                                body: cached_body,
                                etag: cached_etag,
                                payment_response: header.clone(),
                                immutable,
                            },
                        );
                        claim.settled();
                        Settled::Ok(header)
                    }
                    // A refused settle is the payer's (no funds, a spent
                    // nonce): the unit stays spent. The intent stays for
                    // the resolver: if the money moved to us anyway it is
                    // still attributed; if not, it expires.
                    Ok(s) => Settled::Refused(s),
                    // A timeout, a transport error or an error page at
                    // settle: the facilitator may have sent the transfer
                    // before failing. The unit stays spent and nothing is
                    // stored as a success (M7-F5, M7-L47-F3, superseding
                    // M7-L42-F1's refund).
                    Err(why) => Settled::Down(format!(
                        "{why}; the settle outcome is unknown and will be resolved from the chain (THREATS.md OPS-L2-F1)"
                    )),
                }
            }
        };
        if matches!(outcome, Settled::Down(_)) {
            note_unknown(&st, &receipt);
        }
        // M7L3-R2-2: the intent goes back to the resolver BEFORE the claim
        // is released: releasing the claim wakes a waiting identical retry,
        // which must find the intent no settle owns (`awaiting`), not one
        // still marked as this task's.
        drop(guard);
        drop(claim);
        outcome
    });
    match task.await {
        Ok(Settled::Ok(header)) => respond(body, &etag, true, immutable, Some(&header)),
        Ok(Settled::Refused(s)) => ApiError::new(
            StatusCode::PAYMENT_REQUIRED,
            "payment_failed",
            s.error_reason
                .clone()
                .unwrap_or_else(|| "settlement failed".into()),
        )
        .with_header(x402::PAYMENT_RESPONSE, x402::encode_header(&s))
        .into_response(),
        Ok(Settled::Down(m)) => facilitator_down(m),
        Err(_) => {
            // A panicked settle task: the intent guard released the intent
            // to the resolver; count and log it like any unknown outcome.
            note_unknown(state, &receipt);
            internal("settlement task failed").into_response()
        }
    }
}

/// One facilitator settle within `patience`. `Err`: the outcome is unknown
/// (a timeout, a transport error or an error page).
async fn settle_call(
    pay: &Payments,
    req: &x402::FacilitatorRequest,
    bill: (u64, Hash32),
    patience: Duration,
) -> Result<x402::SettleResponse, String> {
    match tokio::time::timeout(patience, pay.facilitator.settle_for(req, bill)).await {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("facilitator settle timed out".into()),
    }
}

/// After a settle whose outcome is unknown (`why`), ask the token what
/// became of the authorization ([`settle_check`], M7-F5 residual): its
/// payment into `OnyxRevenue` is a settled payment (served, with that
/// transaction); a use paying someone else or a cancel is the payer's
/// refusal; an unused one is sent once more (its nonce lets it move money at
/// most once) and, about one block later, checked again if that does not
/// succeed (M7L3-R1-1 (a)). A re-send the token refused as used means the
/// first settle landed: the second check then reads the token's logs even
/// when a lagging node still reads the authorization unused (M7L3-R1-1
/// (b)). Each check reads again while a use is not found or the node fails,
/// within its budget. Anything else stays unknown (`Err`).
async fn after_unknown(
    st: &ApiState,
    pay: &Payments,
    check: &dyn settle_check::SettleCheck,
    (req, bill): (&x402::FacilitatorRequest, (u64, Hash32)),
    (receipt, intent_id): (&receipts::Receipt, u64),
    (valid_after, valid_before): (u64, u64),
    mut why: String,
) -> Result<x402::SettleResponse, String> {
    let payer = Some(hex0x(&receipt.payer));
    let network = pay.terms.network.clone();
    // The settle was made after this moment; the chain's clock is within
    // the allowed skew of ours (the intent was refused otherwise).
    let earliest = (st.clock)()
        .saturating_sub(intents::MAX_CLOCK_SKEW_SECS)
        .max(valid_after);
    let decided = |r: CheckRead| match r {
        CheckRead::Ours(tx) => Some(Ok(x402::SettleResponse {
            success: true,
            error_reason: None,
            payer: payer.clone(),
            transaction: tx,
            network: network.clone(),
        })),
        CheckRead::NotOurs => Some(Ok(x402::SettleResponse {
            success: false,
            error_reason: Some("invalid_transaction_state".into()),
            payer: payer.clone(),
            transaction: String::new(),
            network: network.clone(),
        })),
        CheckRead::Unused => None,
        CheckRead::Undecided(m) => Some(Err(m)),
    };
    let span = (earliest, valid_before);
    match decided(read_use(st, check, receipt, span, false).await) {
        Some(Ok(s)) => return Ok(s),
        Some(Err(m)) => return Err(format!("{why}; {m}")),
        None => {}
    }
    // Unused at the latest block. A re-send needs time for its own settle
    // before `validBefore` (the token refuses it after).
    let patience = st.settle_budget();
    let time_left = valid_before.saturating_sub((st.clock)());
    if time_left <= patience.as_secs().saturating_add(SETTLE_MARGIN_SECS) {
        return Err(format!(
            "{why}; the authorization is still unused (not re-sent: it expires too soon)"
        ));
    }
    // At most MAX_RESENDS re-sends per authorization, its identical retries
    // included (M7L3-R2-5): a retry takes no new payer unit.
    if !st.intents.take_resend(intent_id) {
        return Err(format!(
            "{why}; the authorization is still unused (not re-sent: already re-sent {} times)",
            intents::MAX_RESENDS
        ));
    }
    let refused_as_used = match settle_call(pay, req, bill, patience).await {
        Ok(s) if s.success => return Ok(s),
        Ok(s) => {
            why = format!(
                "re-send refused ({})",
                s.error_reason.as_deref().unwrap_or("no reason")
            );
            refused_as_used(s.error_reason.as_deref())
        }
        Err(e) => {
            why = format!("re-send: {e}");
            false
        }
    };
    // About one block, so a node one block behind has caught up (a).
    tokio::time::sleep(st.settle_recheck_wait()).await;
    match decided(read_use(st, check, receipt, span, refused_as_used).await) {
        Some(Ok(s)) => Ok(s),
        Some(Err(m)) => Err(format!("{why}; {m}")),
        None => Err(format!(
            "{why}; the authorization is still unused (not re-sent again)"
        )),
    }
}

/// Whether a refused settle's reason says the authorization was already
/// used (the token's `authorization is used or canceled`, x402's
/// `invalid_transaction_state`), or gives no reason.
fn refused_as_used(reason: Option<&str>) -> bool {
    reason.is_none_or(|r| {
        let r = r.to_ascii_lowercase();
        r.contains("transaction_state") || r.contains("nonce") || r.contains("used")
    })
}

/// What one authorization check found.
enum CheckRead {
    Ours(String),
    NotOurs,
    Unused,
    Undecided(String),
}

/// One authorization check of `receipt`'s authorization, used in `span`
/// (chain time): `authorizationState`, then (used) the token's logs; with
/// `used` (the token refused a re-send as used), an unused answer is a
/// lagging node's and the logs are read anyway. A use not found yet or a
/// node error is read again after [`ApiState::settle_recheck_wait`], within
/// [`ApiState::settle_check_budget`] (M7L3-R1-1).
async fn read_use(
    st: &ApiState,
    check: &dyn settle_check::SettleCheck,
    receipt: &receipts::Receipt,
    (earliest, latest): (u64, u64),
    used: bool,
) -> CheckRead {
    let (from, nonce, value) = (receipt.payer, receipt.nonce, u128::from(receipt.amount));
    let deadline = tokio::time::Instant::now() + st.settle_check_budget();
    let wait = st.settle_recheck_wait();
    loop {
        let read = tokio::time::timeout_at(deadline, async {
            match check.use_of(from, nonce, value, earliest, latest).await {
                Ok(arrival::UseOf::Unused) if used => {
                    check.logged_use(from, nonce, value, earliest, latest).await
                }
                r => r,
            }
        })
        .await;
        let last = match read {
            Ok(Ok(arrival::UseOf::Ours(tx))) => return CheckRead::Ours(tx),
            Ok(Ok(arrival::UseOf::NotOurs)) => return CheckRead::NotOurs,
            Ok(Ok(arrival::UseOf::Unused)) => return CheckRead::Unused,
            Ok(Ok(arrival::UseOf::NotFound)) => {
                "the authorization is used but its transfer was not found yet".to_string()
            }
            Ok(Err(e)) => format!("authorizationState check failed: {e}"),
            Err(_) => return CheckRead::Undecided("authorizationState check timed out".into()),
        };
        if tokio::time::Instant::now() + wait >= deadline {
            return CheckRead::Undecided(last);
        }
        tokio::time::sleep(wait).await;
    }
}

/// Count and log a settle whose outcome is unknown (OPS-L2-F3): the payer,
/// nonce, round, shard and amount, so an operator can follow it; the intent
/// resolver settles the question from the chain.
fn note_unknown(state: &ApiState, r: &receipts::Receipt) {
    state
        .unknown_settles
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    eprintln!(
        "onyx-api: settle outcome unknown: payer 0x{} nonce 0x{} round {} shard {} amount {}; kept as an intent for the resolver",
        hex::encode(r.payer),
        hex::encode(r.nonce),
        r.round,
        r.shard,
        r.amount
    );
}

/// Failed durable writes of payment state: the receipt log's and the
/// facilitator's own (a self-settle's pending settles).
fn state_write_errors(state: &ApiState) -> u64 {
    state.receipts.write_errors()
        + state.intents.write_errors()
        + state
            .payments
            .as_ref()
            .map_or(0, |p| p.facilitator.state_write_errors())
}

async fn health(State(state): State<Arc<ApiState>>) -> Response {
    let (levels, withheld) = state
        .catalog
        .read()
        .map(|c| {
            let fallback = state.config.rights == catalog::LabelRights::Fallback;
            let mut levels = std::collections::BTreeMap::<u8, usize>::new();
            let mut withheld = 0usize;
            for m in c.shards() {
                if fallback && m.label_derived() {
                    withheld += 1;
                } else {
                    *levels.entry(m.proof_level.number()).or_default() += 1;
                }
            }
            let levels = levels
                .into_iter()
                .map(|(l, n)| (l.to_string(), json!(n)))
                .collect::<serde_json::Map<_, _>>();
            (levels, withheld)
        })
        .unwrap_or_default();
    let (chain_id, contract) = match state.roots.ledger() {
        LedgerKind::OffChain => (Value::Null, Value::Null),
        LedgerKind::Chain { chain_id, contract } => (json!(chain_id), json!(hex0x(&contract))),
    };
    let mut r = Json(json!({
        "status": "ok",
        "x402Version": x402::X402_VERSION,
        "roots": state.roots.kind(),
        "chainId": chain_id,
        "contract": contract,
        "payments": state.payments.as_ref().map(|p| p.terms.network.clone()),
        "payTo": state.payments.as_ref().map(|p| p.terms.pay_to.clone()),
        "contributorPass": state.contributors.is_some(),
        "receiptsPending": state.receipts.pending(),
        // REV-11: "file" when receipts survive a restart; write errors mean
        // the journal closed and the log runs in memory until a restart.
        "receiptLog": if state.receipts.is_persistent() { "file" } else { "memory" },
        "receiptsDropped": state.receipts.dropped(),
        "receiptWriteErrors": state.receipts.write_errors(),
        // REV-L3-F6, REV-L4-F6: the facilitator's own journals (a
        // self-settle's pending settles and its receipt log).
        "pendingSettleWriteErrors": state
            .payments
            .as_ref()
            .map_or(0, |p| p.facilitator.state_write_errors()),
        // OPS-L2-F1, F3: every paid settle is an intent until its outcome is
        // known; the resolver turns the unknown ones into receipts (the
        // payment reached OnyxRevenue) or drops them (expired unused).
        "settles": {
            "intentsOpen": state.intents.open_count(),
            "intentLog": if state.intents.is_persistent() { "file" } else { "memory" },
            "intentWriteErrors": state.intents.write_errors(),
            "recovered": state.intents.recovered(),
            "expiredUnused": state.intents.expired(),
            "cancelled": state.intents.cancelled(),
            "clockSkewSeconds": state.intents.skew(),
            "unknownOutcomes": state.unknown_settles.load(std::sync::atomic::Ordering::Relaxed),
            "inFlight": state.settle_tasks.live(),
        },
        // REV-L3-F5: false while any write failed; paid requests get 503.
        "acceptingPayments": state.payments.is_some()
            && state_write_errors(&state) == 0
            && !state.receipts.is_full(),
        // REV-L4-F1: the reporter's credits in flight and a stuck one.
        "reporter": state.reporter.as_ref().map(|r| {
            let st = r.reporter_status();
            json!({
                "posts": st.posts,
                "applied": st.applied,
                "inFlight": st.in_flight,
                "stuckSince": st.stuck_since,
                "writeErrors": st.write_errors,
                "lastError": st.last_error,
            })
        }),
        // ADR-0033: the chain indexer's cursor and every shard it knows.
        "indexer": state.indexer.as_ref().map(|i| i.indexer_health()),
        // ROADMAP C62: the level-3 post log (null without a level-3 store).
        "level3": state.level3.as_ref().map(|l| l.level3_health()),
        // L16: shards served per proof level ("4": 2), so an operator sees
        // the registry's attested levels take effect; shards ONYX_RIGHTS=
        // fallback withholds (403) are counted apart (review L16-R6).
        "shardLevels": levels,
        "shardsWithheld": withheld,
        // ADR-0034 Amendment 1: each serving check's latest run.
        "servingChecks": state.gates.health(),
        // ROADMAP L14: "fallback" serves no label-derived shard.
        "rights": state.config.rights.as_str(),
        // ADR-0049: paid observation tickets (null when observing on payment is off).
        "observe": state.observe.as_ref().map(|o| o.health((state.clock)())),
    }))
    .into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// `/v1/served/{metric}` (ROADMAP D3, THREATS RES-10): the shards a gated
/// metric's serving check cleared, in snapshot order, with round, shard id,
/// range and root, so a client can fetch each link through `/v1/proofs` and
/// re-run the check itself (`onyx_reserve_serve::client`). Free: it carries
/// no row, only identifiers and ranges the chain already holds. Not trusted
/// by the client (it checks the list's shape before fetching, and a list
/// that leaves out or adds a shard fails its check). Refused like the rows
/// it lists: 403 `label_rights_withheld` under `ONYX_RIGHTS=fallback`
/// (before anything is read); a shard that fails the serve-time checks
/// (rights, test labels, catalog binding) is left out, not the whole list
/// (review D3-R1-5). Free, so each client has its own budget (the free
/// export one, `free_exports_per_minute`): 429 `rate_limited` beyond.
async fn served(
    State(state): State<Arc<ApiState>>,
    Extension(client): Extension<ClientKey>,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    served_reply(&state, &client, path).unwrap_or_else(|e| e.into_response())
}

fn served_reply(
    state: &ApiState,
    client: &ClientKey,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path(metric) = path.map_err(path_err)?;
    check_name(&metric, "metric")?;
    let per_minute = state.config.free_exports_per_minute;
    let scopes: Vec<(String, u32)> = rate_scopes(&client.0, per_minute)
        .into_iter()
        .map(|(k, n)| (format!("served:{k}"), n))
        .collect();
    if !state.caches.take_attempts(&scopes) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many chain lists from this address",
        )
        .with_header("retry-after", (60 / per_minute.max(1)).max(1).to_string()));
    }
    let catalog = state.catalog.read().map_err(|_| internal("catalog lock"))?;
    let withheld = state.config.rights == catalog::LabelRights::Fallback
        && (gate::known_metric(&metric)
            || catalog
                .shards()
                .any(|m| m.metric == metric && m.label_derived()));
    if withheld {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "label_rights_withheld",
            "this server runs with ONYX_RIGHTS=fallback: metrics derived from entity label sets (company flows, balances, reserves) are not served until the label sets' data rights are cleared",
        ));
    }
    let Some((name, shards)) = gate::served(&state.gates, catalog.shards(), &metric) else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "no_serving_check",
            "this server runs no serving check for this metric",
        ));
    };
    let mut list = Vec::with_capacity(shards.len());
    for s in shards.iter().filter(|s| admit(state, &s.meta).is_ok()) {
        list.push(json!({
            "round": s.meta.round,
            "shard": s.meta.shard.to_string(),
            "from": gate::snapshot_start(&s.meta.source.snapshot),
            "to": s.meta.as_of,
            "root": s.root.to_string(),
        }));
    }
    let mut r = Json(json!({
        "metric": metric,
        "gate": name,
        "shards": list,
    }))
    .into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(r)
}

/// The API's own description (docs/api/openapi.json, OpenAPI 3.1; pre-public loop PP6), compiled
/// in, so it is always the description of this build. Free; revalidated by ETag.
pub const OPENAPI_JSON: &str = include_str!("../../../docs/api/openapi.json");

async fn openapi(headers: HeaderMap) -> Response {
    let etag = format!(
        "\"openapi-{}\"",
        hex::encode(&keccak256(OPENAPI_JSON.as_bytes()).0[..8])
    );
    if etag_matches(&headers, &etag) {
        return not_modified(&etag, false, false);
    }
    let mut r = (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        OPENAPI_JSON,
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        r.headers_mut().insert(header::ETAG, v);
    }
    r.headers_mut()
        .insert(header::CACHE_CONTROL, cache_control(false, false));
    r
}

async fn not_found() -> Response {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such route").into_response()
}

async fn method_not_allowed() -> Response {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "this method is not allowed on this route; the Allow header lists the methods it takes",
    )
    .into_response()
}

fn path_err(e: PathRejection) -> ApiError {
    bad_request(e.body_text())
}

fn query_err(e: QueryRejection) -> ApiError {
    bad_request(e.body_text())
}

fn paid(state: &ApiState, price: u64) -> bool {
    state.payments.is_some() && price > 0
}

async fn proof(
    State(state): State<Arc<ApiState>>,
    Extension(client): Extension<ClientKey>,
    path: Result<Path<(String, String, String)>, PathRejection>,
    query: Result<Query<PinQuery>, QueryRejection>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    proof_reply(&state, &client, path, query, &headers, &uri)
        .await
        .unwrap_or_else(|e| *e)
}

async fn proof_reply(
    state: &Arc<ApiState>,
    client: &ClientKey,
    path: Result<Path<(String, String, String)>, PathRejection>,
    query: Result<Query<PinQuery>, QueryRejection>,
    headers: &HeaderMap,
    uri: &Uri,
) -> Reply {
    let Path((round, shard, index)) = path.map_err(path_err)?;
    let (round, _) = parse_u64(&round, "round")?;
    let (shard, _) = parse_hash(&shard, "shard")?;
    let (index, _) = parse_u64(&index, "index")?;
    let Query(q) = query.map_err(query_err)?;
    let pin = pinned(q.root)?;
    let target_url = canonical(
        format!("/v1/proofs/{round}/{shard}/{index}"),
        &pin.map(|p| ("root", p.to_string()))
            .into_iter()
            .collect::<Vec<_>>(),
    );
    require_canonical(uri, &target_url)?;
    let (meta, root) = target(state, client, round, &shard).await?;
    let immutable = check_pin(pin, &root)?;
    if meta.leaf_count.is_some_and(|n| index >= n) {
        return Err(row_not_found().into());
    }
    // ADR-0034 Amendment 1: a gated shard and row are cleared before any 402.
    let cleared = gate_check(state, &meta, &root)?;
    let serving = match usize::try_from(index) {
        Ok(i) if cleared.as_ref().is_some_and(|c| i < c.rows.len()) => {
            row_serving(cleared.as_ref(), i)?
        }
        _ if cleared.is_some() => return Err(row_not_found().into()),
        _ => None,
    };
    let price = price(state, &meta, &root, 1)?;
    let etag = format!("\"{}:{index}\"", root.root);
    if etag_matches(headers, &etag) {
        return Ok(not_modified(&etag, paid(state, price), immutable));
    }
    let gate = authorize(
        state,
        headers,
        &client.0,
        &resource_url(state, &target_url),
        Offer::of("One Onyx row with its Merkle proof"),
        price,
        Pass::Row,
    )
    .await?;
    let l = rows_of(state, &meta, &root)?;
    let i = usize::try_from(index)
        .ok()
        .filter(|i| *i < l.rows.len())
        .ok_or_else(row_not_found)?;
    let mut body = row_body(&meta, &root, &l, i, state.roots.ledger())?;
    if let Some(v) = serving {
        body["servingCheck"] = v;
    }
    Ok(finish(state, gate, (meta.round, meta.shard), body, etag, immutable).await)
}

/// Query of `/v1/metrics/{metric}`. Unknown parameters are refused.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricQuery {
    /// Row key, e.g. `entity:okx`.
    pub subject: String,
    /// Latest `as_of` to consider (height or time, per metric).
    pub as_of: Option<u64>,
    /// Oldest `as_of` the client accepts (PLAN q14 a, M7-L47-F5): shards
    /// that end before it are never served, and when no finalized shard is
    /// new enough the answer is 404 before any 402, so nothing is charged
    /// for an answer the client would refuse.
    pub min_as_of: Option<u64>,
}

/// Most shards a metric query looks at, newest first (M7-L39-F3).
pub const MAX_METRIC_CANDIDATES: usize = 16;

async fn metric(
    State(state): State<Arc<ApiState>>,
    Extension(client): Extension<ClientKey>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<MetricQuery>, QueryRejection>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    metric_reply(&state, &client, path, query, &headers, &uri)
        .await
        .unwrap_or_else(|e| *e)
}

/// A metric answer's shard, root, rows, row index and `servingCheck` field.
type MetricRow = (ShardMeta, FinalRoot, Arc<Loaded>, usize, Option<Value>);

/// Newest shard among `candidates` that has a row for `subject` (M7-F9) and
/// costs exactly `quote` now (M7-L48-F1): shards not finalized yet, without
/// the subject, or charged anything other than the quote are skipped, so a
/// row is never served at a price other than the one paid. A shard whose
/// rows are missing here is an error (never a silently older answer).
async fn find_metric_row(
    state: &ApiState,
    client: &ClientKey,
    candidates: &[ShardMeta],
    subject: &str,
    quote: u64,
) -> Result<Option<MetricRow>, ApiError> {
    for meta in candidates {
        admit(state, meta)?;
        let root = match root_of(state, client, meta.round, &meta.shard).await {
            Ok(r) => r,
            Err(e) if e.code == "not_finalized" => continue,
            Err(e) => return Err(e),
        };
        // A shard the serving check refused (or has not judged) is skipped
        // like one not final (ADR-0034 Amendment 1).
        let cleared = match gate_check(state, meta, &root) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // Priced before any row is read: a dearer shard (one finalized
        // after the quoted one, M7-L48-F1) is never read for this request.
        if charge(state, meta, &root, 1)? != quote {
            continue;
        }
        let l = rows_of(state, meta, &root)?;
        if let Some(i) = find_subject(&l, subject) {
            let serving = row_serving(cleared.as_ref(), i)?;
            return Ok(Some((meta.clone(), root, l, i, serving)));
        }
    }
    Ok(None)
}

/// Metric lookups price from the newest finalized candidate shard, before
/// any row is read and without looking at the subject: an unpaid request
/// learns nothing about whether the subject exists (M7-L39-F3). After
/// payment is verified the subject is looked up; if it is missing, nothing
/// is settled (404, no charge). Only shards that cost exactly the quote at
/// serve time are searched (M7-L48-F1): a row in an older shard at another
/// price is a 404 here and is reached with its own `as_of`.
async fn metric_reply(
    state: &Arc<ApiState>,
    client: &ClientKey,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<MetricQuery>, QueryRejection>,
    headers: &HeaderMap,
    uri: &Uri,
) -> Reply {
    let Path(metric) = path.map_err(path_err)?;
    check_name(&metric, "metric")?;
    let Query(q) = query.map_err(query_err)?;
    let mut qs = vec![("subject", q.subject.clone())];
    if let Some(a) = q.as_of {
        qs.push(("as_of", a.to_string()));
    }
    if let Some(m) = q.min_as_of {
        qs.push(("min_as_of", m.to_string()));
    }
    let target_url = canonical(format!("/v1/metrics/{metric}"), &qs);
    require_canonical(uri, &target_url)?;
    let (candidates, oldest): (Vec<ShardMeta>, Option<ShardMeta>) = {
        let c = state.catalog.read().map_err(|_| internal("catalog lock"))?;
        let candidates = c
            .for_metric(&metric, q.as_of)
            .into_iter()
            .filter(|m| q.min_as_of.is_none_or(|floor| m.as_of >= floor))
            .take(MAX_METRIC_CANDIDATES)
            .cloned()
            .collect();
        let oldest = c.for_metric(&metric, None).last().copied().cloned();
        (candidates, oldest)
    };
    if candidates.is_empty() {
        // THREATS SOAK-5: the metric exists but every shard is newer than
        // `as_of`; `unknown_metric` read like a wrong metric name. The oldest
        // `as_of` is no secret: a 402 or 404 at any `as_of` already tells it.
        // The shard it points to is admitted first (E10 review): a metric
        // this server withholds (fallback rights, test labels, restricted
        // data) answers that refusal at any `as_of`, as it does at a served
        // one, and never points the client at a shard it would refuse.
        if let (Some(oldest), Some(a)) = (oldest, q.as_of) {
            if a < oldest.as_of {
                admit(state, &oldest)?;
                return Err(ApiError::new(
                    StatusCode::NOT_FOUND,
                    "no_shard_at_as_of",
                    format!(
                        "every shard of this metric is newer than as_of; the oldest is as_of {}",
                        oldest.as_of
                    ),
                )
                .into());
            }
        }
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_metric",
            "no data for this metric and as_of",
        )
        .into());
    }
    // The quote: newest finalized candidate (root lookups only, cached).
    // Its position bounds the later lookup (M7-L47-F1): a newer shard that
    // finalizes between the quote and the lookup must never be served at the
    // older shard's (possibly free) price.
    let mut quote = None;
    let mut refused = None;
    for (idx, meta) in candidates.iter().enumerate() {
        admit(state, meta)?;
        match root_of(state, client, meta.round, &meta.shard).await {
            Ok(r) => match gate_check(state, meta, &r) {
                Ok(c) => {
                    quote = Some((idx, meta.clone(), r, c));
                    break;
                }
                // ADR-0034 Amendment 1: the newest shard the serving check
                // cleared is the quote; a refused one is skipped.
                Err(e) => {
                    refused.get_or_insert(e);
                    continue;
                }
            },
            Err(e) if e.code == "not_finalized" => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let Some((qidx, qmeta, qroot, qcleared)) = quote else {
        return Err(refused
            .unwrap_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "not_finalized",
                    "no finalized shard for this metric yet",
                )
            })
            .into());
    };
    // A row the serving check refuses (no cross-check) is refused before
    // any 402: which entities are cross-checked is public policy, not data.
    if let Some(Err(why)) = qcleared
        .as_ref()
        .and_then(|c| c.by_key(q.subject.as_bytes()))
        .map(|r| &r.cross_check)
    {
        return Err(not_cross_checked(why).into());
    }
    let price = charge(state, &qmeta, &qroot, 1)?;
    let charged = price > 0;
    let priced = &candidates[qidx..];
    let unknown_subject = || -> Box<Response> {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_subject",
            "no row for this subject at the quoted price",
        )
        .into()
    };
    // Free data: the subject is public, so a 304 may name its row. Paid
    // data: no conditional answer before payment (its ETag would reveal
    // whether the subject exists).
    if !charged {
        let Some((meta, root, l, i, serving)) =
            find_metric_row(state, client, priced, &q.subject, price).await?
        else {
            return Err(unknown_subject());
        };
        let etag = format!("\"{}:{i}\"", root.root);
        if etag_matches(headers, &etag) {
            return Ok(not_modified(&etag, false, false));
        }
        let body = metric_body(&metric, &q.subject, &meta, &root, &l, i, state, serving)?;
        return Ok(respond(body, &etag, false, false, None));
    }
    let gate = authorize(
        state,
        headers,
        &client.0,
        &resource_url(state, &target_url),
        Offer::of("One Onyx metric row with its Merkle proof"),
        price,
        Pass::Row,
    )
    .await?;
    let Some((meta, root, l, i, serving)) =
        find_metric_row(state, client, priced, &q.subject, price).await?
    else {
        return Err(unknown_subject());
    };
    let etag = format!("\"{}:{i}\"", root.root);
    let body = metric_body(&metric, &q.subject, &meta, &root, &l, i, state, serving)?;
    Ok(finish(state, gate, (meta.round, meta.shard), body, etag, false).await)
}

#[allow(clippy::too_many_arguments)]
fn metric_body(
    metric: &str,
    subject: &str,
    meta: &ShardMeta,
    root: &FinalRoot,
    l: &Loaded,
    i: usize,
    state: &ApiState,
    serving: Option<Value>,
) -> Result<Value, ApiError> {
    let mut body = row_body(meta, root, l, i, state.roots.ledger())?;
    if let Some(v) = serving {
        body["servingCheck"] = v;
    }
    body["metric"] = json!(metric);
    body["subject"] = json!(subject);
    body["asOf"] = json!(meta.as_of);
    Ok(body)
}

async fn export(
    State(state): State<Arc<ApiState>>,
    Extension(client): Extension<ClientKey>,
    path: Result<Path<(String, String)>, PathRejection>,
    query: Result<Query<PinQuery>, QueryRejection>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    export_reply(&state, &client, path, query, &headers, &uri)
        .await
        .unwrap_or_else(|e| *e)
}

async fn export_reply(
    state: &Arc<ApiState>,
    client: &ClientKey,
    path: Result<Path<(String, String)>, PathRejection>,
    query: Result<Query<PinQuery>, QueryRejection>,
    headers: &HeaderMap,
    uri: &Uri,
) -> Reply {
    let Path((round, shard)) = path.map_err(path_err)?;
    let (round, _) = parse_u64(&round, "round")?;
    let (shard, _) = parse_hash(&shard, "shard")?;
    let Query(q) = query.map_err(query_err)?;
    let pin = pinned(q.root)?;
    let target_url = canonical(
        format!("/v1/export/{round}/{shard}"),
        &pin.map(|p| ("root", p.to_string()))
            .into_iter()
            .collect::<Vec<_>>(),
    );
    require_canonical(uri, &target_url)?;
    let (meta, root) = target(state, client, round, &shard).await?;
    let immutable = check_pin(pin, &root)?;
    // ADR-0034 Amendment 1: an export of a gated shard needs every row
    // cleared (a refused row would otherwise leave in the export).
    let cleared = gate_check(state, &meta, &root)?;
    let serving = match &cleared {
        Some(c) => {
            let rows: Vec<Value> = (0..c.rows.len())
                .map(|i| {
                    c.row(i).map_err(|why| {
                        not_cross_checked(&format!("row {i}: {why}; use /v1/metrics per subject"))
                    })
                })
                .collect::<Result<_, _>>()?;
            Some(json!({"gate": c.gate, "rows": rows}))
        }
        None => None,
    };
    // Priced per row (an export is every row). The catalog's row count lets
    // this happen before any row is read; without it, rows are read once.
    let n = match meta.leaf_count {
        Some(n) => n,
        None => rows_of(state, &meta, &root)?.tree.leaf_count(),
    };
    if n > state.config.export_max_rows {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "export_too_large",
            format!(
                "{n} rows; exports are capped at {} (use /v1/proofs)",
                state.config.export_max_rows
            ),
        )
        .into());
    }
    let price = export_price(state, &meta, &root, n)?;
    let etag = format!("\"{}:all\"", root.root);
    if etag_matches(headers, &etag) {
        return Ok(not_modified(&etag, paid(state, price), immutable));
    }
    if !paid(state, price) {
        // Free exports are the cheapest way to make the server read and send
        // a whole shard: they get their own per-client budget (M7-L47-F2).
        let per_minute = state.config.free_exports_per_minute;
        let scopes: Vec<(String, u32)> = rate_scopes(&client.0, per_minute)
            .into_iter()
            .map(|(k, n)| (format!("export:{k}"), n))
            .collect();
        if !state.caches.take_attempts(&scopes) {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many free exports from this address",
            )
            .with_header("retry-after", (60 / per_minute.max(1)).max(1).to_string())
            .into());
        }
        check_export_bytes(state, &*rows_of(state, &meta, &root)?)?;
    } else if let Some(l) = state.caches.tree(&root.root) {
        // Rows already in memory: refuse an oversized export before the 402.
        check_export_bytes(state, &l)?;
    }
    let mut gate = authorize(
        state,
        headers,
        &client.0,
        &resource_url(state, &target_url),
        Offer::of("All rows of an Onyx shard"),
        price,
        Pass::Never,
    )
    .await?;
    let l = rows_of(state, &meta, &root)?;
    if let Err(e) = check_export_bytes(state, &l) {
        // Found only now (rows were not cached before the 402): nothing was
        // settled, so the payer's unit comes back.
        if let Gate::Paid { claim, .. } = &mut gate {
            claim.refund();
        }
        return Err(e);
    }
    let mut body = json!({
        "round": meta.round,
        "shard": meta.shard.to_string(),
        "root": root.root.to_string(),
        "leafCount": l.tree.leaf_count(),
        "rows": l.rows.iter().map(|r| hex0x(r)).collect::<Vec<_>>(),
        "finalizedAt": root.finalized_at,
        "recipeId": nonzero(meta.binding.recipe_id),
        "labelSet": nonzero(meta.binding.label_set_hash),
        "tx": root.tx,
        "license": meta.rights.license,
        "attribution": meta.rights.attribution,
        "proofLevel": meta.proof_level.number(),
        "verify": bundle(&meta, &root, l.tree.leaf_count(), state.roots.ledger()),
    });
    if let Some(v) = serving {
        body["servingCheck"] = v;
    }
    Ok(finish(state, gate, (meta.round, meta.shard), body, etag, immutable).await)
}

/// Upper bound of an export's JSON size: each row is `"0x<hex>",`, plus
/// the fixed fields and the verification bundle.
fn export_bytes(l: &Loaded) -> u64 {
    const FIXED: u64 = 8 * 1024;
    l.rows
        .iter()
        .map(|r| 2 * r.len() as u64 + 5)
        .fold(FIXED, u64::saturating_add)
}

/// 413 `export_too_large` for a body over `export_max_bytes` (M7-L47-F2).
fn check_export_bytes(state: &ApiState, l: &Loaded) -> Result<(), Box<Response>> {
    let bytes = export_bytes(l);
    if bytes > state.config.export_max_bytes {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "export_too_large",
            format!(
                "about {bytes} bytes; exports are capped at {} (use /v1/proofs)",
                state.config.export_max_bytes
            ),
        )
        .into());
    }
    Ok(())
}

async fn settlement(
    State(state): State<Arc<ApiState>>,
    Extension(client): Extension<ClientKey>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    settlement_reply(&state, &client, path, &headers, &uri)
        .await
        .unwrap_or_else(|e| *e)
}

async fn settlement_reply(
    state: &Arc<ApiState>,
    client: &ClientKey,
    path: Result<Path<String>, PathRejection>,
    headers: &HeaderMap,
    uri: &Uri,
) -> Reply {
    let Path(market) = path.map_err(path_err)?;
    check_name(&market, "market")?;
    if uri.query().is_some() {
        return Err(bad_request("this route takes no query parameters").into());
    }
    let target_url = format!("/v1/settlement/{market}");
    require_canonical(uri, &target_url)?;
    let m = state
        .catalog
        .read()
        .map_err(|_| internal("catalog lock"))?
        .market(&market)
        .cloned()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown_market", "no such market"))?;
    let (meta, root) = target(state, client, m.round, &m.shard).await?;
    // ADR-0034 Amendment 1: a gated market row must be cleared.
    if let Some(c) = gate_check(state, &meta, &root)? {
        match c.by_key(m.subject.as_bytes()).map(|r| &r.cross_check) {
            Some(Ok(_)) => {}
            Some(Err(why)) => return Err(not_cross_checked(why).into()),
            None => {
                return Err(
                    not_cross_checked("the serving check has no answer for this row").into(),
                )
            }
        }
    }
    // A market settles only on a proof level its policy accepts (ADR-0027,
    // L2R-F8): by default levels 1 and 3, never a level-2 or weaker shard.
    if !state
        .catalog
        .read()
        .map_err(|_| internal("catalog lock"))?
        .settles_on(&meta)
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "proof_level_not_accepted",
            "the market's data is below the proof level markets settle on",
        )
        .into());
    }
    let price = price(state, &meta, &root, 1)?;
    let etag = format!(
        "\"{}:{}:settlement\"",
        root.root,
        keccak256(m.subject.as_bytes())
    );
    if etag_matches(headers, &etag) {
        return Ok(not_modified(&etag, paid(state, price), false));
    }
    let gate = authorize(
        state,
        headers,
        &client.0,
        &resource_url(state, &target_url),
        Offer::of("Settlement value with its Merkle proof"),
        price,
        Pass::Row,
    )
    .await?;
    let l = rows_of(state, &meta, &root)?;
    let i = find_subject(&l, &m.subject).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_subject",
            "the market's row is not in the shard",
        )
    })?;
    let p = l.tree.proof(i as u64).ok_or_else(|| internal("proof"))?;
    let value = OutputRow::try_from_slice(&l.rows[i])
        .map(|o| hex0x(&o.value))
        .map_err(|_| internal("the market's row does not decode"))?;
    // Exactly the arguments of OnyxSettlement.verifyRow(id, shard, row, index,
    // leafCount, siblings) and OnyxMerkle.verify(root, row, index, leafCount, siblings).
    let body = json!({
        "market": market,
        "subject": m.subject,
        "value": value,
        "round": m.round,
        "shard": m.shard.to_string(),
        "row": hex0x(&l.rows[i]),
        "index": p.index,
        "leafCount": p.leaf_count,
        "siblings": p.siblings.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "root": root.root.to_string(),
        "finalizedAt": root.finalized_at,
        "recipeId": nonzero(meta.binding.recipe_id),
        "labelSet": nonzero(meta.binding.label_set_hash),
        "tx": root.tx,
        "verifyCall": "OnyxSettlement.verifyRow(uint64,bytes32,bytes,uint64,uint64,bytes32[])",
        "proofLevel": meta.proof_level.number(),
        "verify": bundle(&meta, &root, l.tree.leaf_count(), state.roots.ledger()),
    });
    Ok(finish(state, gate, (m.round, m.shard), body, etag, false).await)
}

/// Query of `/v1/observations/{metric}`. Unknown parameters are refused.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationQuery {
    /// The subject's text, e.g. `us.treasury.federal_debt:2026-10-08`.
    pub subject: String,
    /// A fixed feed's window end; without it, the newest round of the
    /// subject (a dated feed has one round per subject).
    pub window_end: Option<u64>,
    /// Without `window_end`: the newest round whose window ends at or before
    /// this unix time (L2API-R3-F3). Not combined with `window_end`.
    pub as_of: Option<u64>,
    /// Oldest window end the client accepts (L2API-R4-F2, as a shard's
    /// `min_as_of`, M7-L47-F5): an older round is a 404 before any 402, so a
    /// client is never charged for a round its own freshness floor refuses.
    pub min_window_end: Option<u64>,
    /// `all`: the answer also carries every published round of the fact
    /// that settled no value (`attempts`, SEC-R2-F1). `value` (the default)
    /// leaves them out. Same price either way.
    pub outcome: Option<String>,
}

async fn observation(
    State(state): State<Arc<ApiState>>,
    Extension(client): Extension<ClientKey>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<ObservationQuery>, QueryRejection>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    observation_reply(&state, &client, path, query, &headers, &uri)
        .await
        .unwrap_or_else(|e| *e)
}

/// A level-2 value with its round's transcript (ADR-0027 amendment 4). The
/// rights are checked first, then the record is looked up (a settled fact is
/// public on chain, so its existence is not a secret), priced from its
/// template-fixed decay start (`FeedTemplate::decay_start`, L2API-R3-F1), and
/// paid like a row. Billed to the revenue book
/// `(OBSERVATION_ROUND, registry fact key)`.
async fn observation_reply(
    state: &Arc<ApiState>,
    client: &ClientKey,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<ObservationQuery>, QueryRejection>,
    headers: &HeaderMap,
    uri: &Uri,
) -> Reply {
    let Path(metric) = path.map_err(path_err)?;
    check_name(&metric, "metric")?;
    let Query(q) = query.map_err(query_err)?;
    if q.subject.is_empty() || q.subject.len() > 256 {
        return Err(bad_request("subject must be 1 to 256 bytes").into());
    }
    let mut qs = vec![("subject", q.subject.clone())];
    if q.window_end.is_some() && q.as_of.is_some() {
        return Err(bad_request("give window_end or as_of, not both").into());
    }
    if let Some(w) = q.window_end {
        qs.push(("window_end", w.to_string()));
    }
    if let Some(a) = q.as_of {
        qs.push(("as_of", a.to_string()));
    }
    if let Some(m) = q.min_window_end {
        qs.push(("min_window_end", m.to_string()));
    }
    let all = match q.outcome.as_deref() {
        None | Some("value") => false,
        Some("all") => true,
        Some(_) => return Err(bad_request("outcome must be value or all").into()),
    };
    if all {
        // `value` is the default and has one spelling: no parameter.
        qs.push(("outcome", "all".into()));
    }
    let target_url = canonical(format!("/v1/observations/{metric}"), &qs);
    require_canonical(uri, &target_url)?;
    let found = {
        let c = state.catalog.read().map_err(|_| internal("catalog lock"))?;
        // Copy only the template and the one record, never the whole store
        // (AUD-2-F4): a request costs the same at 1 round or 50,000.
        let full = c.observations(&metric).ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_metric",
                "no level-2 data for this metric",
            )
        })?;
        admit_rights(&full.template.rights)?;
        let feed = full.head();
        match full.record(&q.subject, q.window_end, q.as_of).cloned() {
            None => None,
            Some(record) => {
                if q.min_window_end
                    .is_some_and(|floor| record.checked.spec.window_end < floor)
                {
                    return Err(ApiError::new(
                        StatusCode::NOT_FOUND,
                        "too_old",
                        "no settled round for this subject at or after min_window_end",
                    )
                    .into());
                }
                let attempts = if all {
                    full.attempts_of(&record.checked.fact)
                } else {
                    Vec::new()
                };
                Some((feed, record, attempts))
            }
        }
    };
    let Some((feed, record, attempts)) = found else {
        // ADR-0049: a subject no round settled yet may be offered for a paid round.
        if q.window_end.is_none() && q.as_of.is_none() && !all {
            if let Some(r) = observe::reply(state, &metric, &q.subject, headers).await {
                return r;
            }
        }
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_subject",
            "no settled round for this subject",
        )
        .into());
    };
    let price = if state.payments.is_some() {
        state
            .catalog
            .read()
            .map(|c| c.observation_price(&metric, record.checked.settled_unix, (state.clock)()))
            .map_err(|_| internal("catalog lock"))?
    } else {
        0
    };
    // A settled result never changes: the URL is content-addressed when it
    // names one round (a dated subject, or an explicit window end).
    let immutable = feed.template.is_dated() || q.window_end.is_some();
    // The ETag names the fact, never the result: a result hash lets anyone
    // who can guess the value confirm it (L2API-R1-F5). One fact has one
    // settled result, so the fact identifies the content.
    let etag = format!(
        "\"obs-{}{}\"",
        keccak256(
            &[
                b"onyx-observation-etag-v1".as_slice(),
                &record.checked.registry_fact.0
            ]
            .concat()
        ),
        if all { "-all" } else { "" }
    );
    // Paid data: no conditional answer before payment (as M7-L39-F3).
    let charged = paid(state, price);
    if !charged && etag_matches(headers, &etag) {
        return Ok(not_modified(&etag, false, immutable));
    }
    let gate = authorize(
        state,
        headers,
        &client.0,
        &resource_url(state, &target_url),
        Offer {
            description: "One Onyx level-2 value with its round transcript",
            // ROADMAP B4: the buyer sees the known limits before paying.
            limits: Some(observations::limits_json(false)),
        },
        price,
        Pass::Row,
    )
    .await?;
    let body = if all {
        feed.body_all(&record, &attempts)
    } else {
        feed.body(&record)
    };
    Ok(finish(
        state,
        gate,
        (
            observations::OBSERVATION_ROUND,
            record.checked.registry_fact,
        ),
        body,
        etag,
        immutable,
    )
    .await)
}

/// `/v1/tickets/{ticket}` (ADR-0049): a paid observation ticket's state; once observed, the
/// paid answer. The ticket is the bearer's secret: nobody else learns what it asked.
async fn ticket(
    State(state): State<Arc<ApiState>>,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let Path(t) = match path.map_err(path_err) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    observe::ticket_reply(&state, &t).unwrap_or_else(|e| e.into_response())
}

/// Query of `/v1/observations/{metric}/attempts`. Unknown parameters are
/// refused.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptsQuery {
    /// The subject's text.
    pub subject: String,
    /// A fixed feed's window end; without it, the subject's newest window
    /// with published rounds.
    pub window_end: Option<u64>,
}

async fn observation_attempts(
    State(state): State<Arc<ApiState>>,
    Extension(client): Extension<ClientKey>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<AttemptsQuery>, QueryRejection>,
    uri: Uri,
) -> Response {
    observation_attempts_reply(&state, &client, path, query, &uri).unwrap_or_else(|e| *e)
}

/// Every published round of a fact that settled no value (SEC-R2-F1,
/// delivery.md §Observations), free. A fact WITH a served value is refused
/// here (409): its rounds come with the paid value
/// (`/v1/observations/{metric}?..&outcome=all`). A disagreeing round's
/// reveals may hold the value a later round sells (ATT-R1-F2), so the
/// reveals are served only once the fact is closed for good (a fixed fact
/// with a round: [`observations::ObservationFeed::fact_closed`]); before that each round is
/// served sealed: commits, who revealed and the outcome, no observation, page
/// hash or salt. Each client has its own budget of free answers (ATT-R1-F5),
/// and the rounds are copied before the catalog lock is released.
fn observation_attempts_reply(
    state: &Arc<ApiState>,
    client: &ClientKey,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<AttemptsQuery>, QueryRejection>,
    uri: &Uri,
) -> Reply {
    let Path(metric) = path.map_err(path_err)?;
    check_name(&metric, "metric")?;
    let Query(q) = query.map_err(query_err)?;
    if q.subject.is_empty() || q.subject.len() > 256 {
        return Err(bad_request("subject must be 1 to 256 bytes").into());
    }
    let mut qs = vec![("subject", q.subject.clone())];
    if let Some(w) = q.window_end {
        qs.push(("window_end", w.to_string()));
    }
    let target_url = canonical(format!("/v1/observations/{metric}/attempts"), &qs);
    require_canonical(uri, &target_url)?;
    let per_minute = state.config.free_attempts_per_minute;
    let scopes: Vec<(String, u32)> = rate_scopes(&client.0, per_minute)
        .into_iter()
        .map(|(k, n)| (format!("attempts:{k}"), n))
        .collect();
    if !state.caches.take_attempts(&scopes) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many free attempts answers from this address",
        )
        .with_header("retry-after", (60 / per_minute.max(1)).max(1).to_string())
        .into());
    }
    let (feed, attempts, closed) = {
        let c = state.catalog.read().map_err(|_| internal("catalog lock"))?;
        // Copy the template and this fact's rounds only (AUD-2-F4).
        let full = c.observations(&metric).ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_metric",
                "no level-2 data for this metric",
            )
        })?;
        admit_rights(&full.template.rights)?;
        let settled = || {
            ApiError::new(
                StatusCode::CONFLICT,
                "settled_value",
                "this fact settled a value: its rounds come with it (outcome=all)",
            )
        };
        let Some(fact) = full.attempt_fact(&q.subject, q.window_end) else {
            if full.record(&q.subject, q.window_end, None).is_some() {
                return Err(settled().into());
            }
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_subject",
                "no published round without a value for this subject",
            )
            .into());
        };
        if full.has_value(&fact) {
            return Err(settled().into());
        }
        (
            full.head(),
            full.attempts_of(&fact),
            full.fact_closed(&fact),
        )
    };
    let body = feed.attempts_body(&attempts, closed);
    let mut r = Json(body).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(r)
}

/// Query of `/v1/estimates/{metric}`. Unknown parameters are refused.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EstimateQuery {
    /// The subject's text, as `/v1/observations` takes it.
    pub subject: String,
}

async fn estimate(
    State(state): State<Arc<ApiState>>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<EstimateQuery>, QueryRejection>,
    uri: Uri,
) -> Response {
    estimate_reply(&state, path, query, &uri).unwrap_or_else(|e| *e)
}

/// An estimate (ADR-0050, docs/spec/estimates.md): a value Onyx did not
/// check, with its method and its error band. Its level is `E`, not a rung of
/// the proof ladder, so `proofLevel` and `ladder` are null (R-EST-4), and the
/// error is always present, `unknown` included (R-EST-3). Free on testnet: an
/// estimate carries no proof cost (spec open question 2). Only `estimate.`
/// metrics are served here, and no other route serves one (R-EST-2).
fn estimate_reply(
    state: &Arc<ApiState>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<EstimateQuery>, QueryRejection>,
    uri: &Uri,
) -> Reply {
    let Path(metric) = path.map_err(path_err)?;
    check_name(&metric, "metric")?;
    let Query(q) = query.map_err(query_err)?;
    if q.subject.is_empty() || q.subject.len() > 256 {
        return Err(bad_request("subject must be 1 to 256 bytes").into());
    }
    let target_url = canonical(
        format!("/v1/estimates/{metric}"),
        &[("subject", q.subject.clone())],
    );
    require_canonical(uri, &target_url)?;
    let c = state.catalog.read().map_err(|_| internal("catalog lock"))?;
    if !onyx_core::is_estimate_id(&metric) || !c.has_estimates(&metric) {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_metric",
            "no estimates for this metric",
        )
        .into());
    }
    let e = c.estimate(&metric, &q.subject).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_subject",
            "no estimate for this subject",
        )
    })?;
    let error = match &e.error {
        onyx_core::ErrorBand::Interval {
            lo,
            hi,
            coverage_ppm,
        } => json!({
            "kind": "interval",
            "lo": onyx_core::estimate::value_text(lo),
            "hi": onyx_core::estimate::value_text(hi),
            "coveragePpm": coverage_ppm,
        }),
        onyx_core::ErrorBand::Unknown => json!({ "kind": "unknown" }),
    };
    let hash = e.hash();
    let body = json!({
        "metric": e.metric.0,
        "subject": q.subject,
        "asOf": e.as_of,
        "value": onyx_core::estimate::value_text(&e.value),
        "level": "E",
        "proofLevel": null,
        "ladder": null,
        "checked": false,
        "method": e.method,
        "methodHash": hex0x(&e.method_hash.0),
        "error": error,
        "inputs": e.inputs.iter().map(|h| hex0x(&h.0)).collect::<Vec<_>>(),
        "estimateHash": hex0x(&hash.0),
        "note": "Estimate, not checked: never a proof level, never part of a checked total.",
    });
    Ok(respond(
        body,
        &format!("\"{}\"", hex0x(&hash.0)),
        false,
        false,
        None,
    ))
}

#[cfg(test)]
mod openapi_check;
#[cfg(test)]
mod tests;
