//! x402 protocol v2 over HTTP (ADR-0023), from the official spec
//! (`specs/x402-specification-v2.md` and `specs/transports-v2/http.md` in
//! x402-foundation/x402). Field names and header names follow the spec
//! exactly; every header value is base64 (standard alphabet) of JSON.
//!
//! The server never trusts the client: it checks that the payload accepted
//! exactly the requirements it offered, then asks a [`Facilitator`] to verify
//! (read-only) before serving and to settle after, the `exact` scheme's
//! default `authorization` flow (spec §6.1).

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Protocol version this server speaks.
pub const X402_VERSION: u32 = 2;
/// Server -> client: base64 `PaymentRequired`.
pub const PAYMENT_REQUIRED: &str = "payment-required";
/// Client -> server: base64 `PaymentPayload`.
pub const PAYMENT_SIGNATURE: &str = "payment-signature";
/// Server -> client: base64 `SettleResponse`.
pub const PAYMENT_RESPONSE: &str = "payment-response";
/// CAIP-2 id of Base Sepolia.
pub const BASE_SEPOLIA: &str = "eip155:84532";
/// Largest header value accepted from a client (bytes, before decoding).
pub const MAX_PAYMENT_HEADER: usize = 16 * 1024;

/// A future returned by the async traits in this crate.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The protected resource (spec §5.1 `ResourceInfo`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceInfo {
    /// URL of the protected resource.
    pub url: String,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// MIME type of the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// One acceptable way to pay (spec §5.1 `PaymentRequirements`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequirements {
    /// Payment scheme, e.g. `exact`.
    pub scheme: String,
    /// CAIP-2 network id.
    pub network: String,
    /// Amount in atomic token units, as a decimal string.
    pub amount: String,
    /// Token contract address.
    pub asset: String,
    /// Recipient address.
    pub pay_to: String,
    /// Maximum time allowed for payment completion.
    pub max_timeout_seconds: u64,
    /// Scheme-specific data (EIP-712 domain name and version for USDC).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

/// The 402 challenge (spec §5.1 `PaymentRequired`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequired {
    /// Always 2.
    pub x402_version: u32,
    /// Why payment is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The resource.
    pub resource: ResourceInfo,
    /// Acceptable payments.
    pub accepts: Vec<PaymentRequirements>,
    /// Spec §5.1.2 `extensions`: a map of extension id to `{info, schema}`.
    /// An Onyx level-2 offer carries `limits` (`info.limits`: the known
    /// limits of what is sold, ROADMAP B4, delivery.md §Level-2 limits;
    /// `observations::limits_extension`); absent on every other offer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<serde_json::Value>,
}

/// What the client sends (spec §5.2 `PaymentPayload`). `payload` is
/// scheme-specific and is passed to the facilitator unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentPayload {
    /// Protocol version.
    pub x402_version: u32,
    /// The resource the client paid for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<ResourceInfo>,
    /// The requirements the client accepted.
    pub accepted: PaymentRequirements,
    /// Scheme-specific authorization (EIP-3009 for `exact` on EVM).
    pub payload: serde_json::Value,
}

/// Facilitator `/verify` result (spec §5.4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResponse {
    /// True if the authorization is valid.
    pub is_valid: bool,
    /// Why not, if not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,
    /// Payer address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
}

/// Facilitator `/settle` result, also the `PAYMENT-RESPONSE` header (spec §5.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettleResponse {
    /// True if settled.
    pub success: bool,
    /// Why not, if not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    /// Payer address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    /// Transaction hash (empty if nothing was broadcast).
    pub transaction: String,
    /// CAIP-2 network id.
    pub network: String,
}

/// Body of `POST /verify` and `POST /settle` (spec §7.1, §7.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacilitatorRequest {
    /// Protocol version.
    pub x402_version: u32,
    /// What the client sent.
    pub payment_payload: PaymentPayload,
    /// What the server required.
    pub payment_requirements: PaymentRequirements,
}

/// Header codec error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    /// Too long, not base64, or not the expected JSON.
    #[error("malformed payment header: {0}")]
    Malformed(String),
}

/// Encode a value as a header: base64 of its JSON.
pub fn encode_header<T: Serialize>(value: &T) -> String {
    B64.encode(serde_json::to_vec(value).unwrap_or_default())
}

/// Decode a header: bounded size, base64, then JSON.
pub fn decode_header<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T, CodecError> {
    if value.len() > MAX_PAYMENT_HEADER {
        return Err(CodecError::Malformed("too long".into()));
    }
    let bytes = B64
        .decode(value.trim())
        .map_err(|e| CodecError::Malformed(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| CodecError::Malformed(e.to_string()))
}

/// Facilitator call error (transport or protocol), never a payment verdict.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum FacilitatorError {
    /// The facilitator could not be reached or answered garbage.
    #[error("facilitator unavailable: {0}")]
    Unavailable(String),
}

/// A payment facilitator (spec §7): verify is read-only, settle commits.
pub trait Facilitator: Send + Sync {
    /// `POST /verify`.
    fn verify<'a>(
        &'a self,
        req: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>>;
    /// `POST /settle`.
    fn settle<'a>(
        &'a self,
        req: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>>;
    /// Settle a payment for the row of `bill` (round, shard) that is being
    /// served. A facilitator that credits the shard itself in the same
    /// transaction (`self_settle::SelfSettle`, ADR-0026) uses it; the default
    /// ignores it and calls [`Facilitator::settle`].
    fn settle_for<'a>(
        &'a self,
        req: &'a FacilitatorRequest,
        bill: (u64, onyx_core::Hash32),
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        let _ = bill;
        self.settle(req)
    }
    /// True when a successful settle already credited the shard on chain, so
    /// the server records no usage receipt for it (ADR-0026).
    fn credits_on_settle(&self) -> bool {
        false
    }
    /// The longest a settle of this facilitator can take when it decides
    /// its own deadline (a self-settle waits for its transaction). The
    /// server waits at least this long, so the facilitator's own
    /// unknown-outcome handling always runs (M8-L58-F2). `None`: the
    /// server's facilitator timeout applies.
    fn settle_budget(&self) -> Option<std::time::Duration> {
        None
    }
    /// Disk writes of this facilitator's own state that failed (a
    /// self-settle's pending settles): reported on `/v1/health`, and the
    /// server refuses new paid requests while any failed (REV-L3-F5).
    fn state_write_errors(&self) -> u64 {
        0
    }
}

/// Local facilitator for tests and the demo server. It accepts a payload
/// whose `payload.authorization` names a funded `from` address and the exact
/// required value, and refuses a nonce it has already settled (the on-chain
/// EIP-3009 nonce rule, spec §10.1). It moves no money.
#[derive(Debug, Default)]
pub struct MockFacilitator {
    funded: BTreeSet<String>,
    used_nonces: Mutex<BTreeSet<String>>,
}

impl MockFacilitator {
    /// A facilitator that accepts payments from these addresses (lowercase hex).
    pub fn new(funded: impl IntoIterator<Item = String>) -> Self {
        MockFacilitator {
            funded: funded.into_iter().map(|a| a.to_lowercase()).collect(),
            used_nonces: Mutex::new(BTreeSet::new()),
        }
    }

    fn check(&self, req: &FacilitatorRequest) -> Result<String, &'static str> {
        if req.x402_version != X402_VERSION || req.payment_payload.x402_version != X402_VERSION {
            return Err("invalid_x402_version");
        }
        if req.payment_payload.accepted != req.payment_requirements {
            return Err("invalid_payment_requirements");
        }
        // The same EIP-3009 checks a real facilitator makes, signature included
        // (an EOA signature must recover to `from`).
        let auth = exact_evm::precheck(
            &req.payment_payload.payload,
            &req.payment_requirements,
            now_unix(),
        )?;
        let from = format!("0x{}", hex::encode(auth.from));
        if !self.funded.contains(&from) {
            return Err("insufficient_funds");
        }
        Ok(from)
    }

    fn nonce(req: &FacilitatorRequest) -> Option<String> {
        req.payment_payload.payload["authorization"]["nonce"]
            .as_str()
            .map(str::to_lowercase)
    }
}

impl Facilitator for MockFacilitator {
    fn verify<'a>(
        &'a self,
        req: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>> {
        let used = Self::nonce(req).is_some_and(|n| {
            self.used_nonces
                .lock()
                .map(|u| u.contains(&n))
                .unwrap_or(true)
        });
        let r = match (self.check(req), used) {
            (Ok(payer), false) => VerifyResponse {
                is_valid: true,
                invalid_reason: None,
                payer: Some(payer),
            },
            (Ok(payer), true) => VerifyResponse {
                is_valid: false,
                invalid_reason: Some("invalid_transaction_state".into()),
                payer: Some(payer),
            },
            (Err(reason), _) => VerifyResponse {
                is_valid: false,
                invalid_reason: Some(reason.into()),
                payer: None,
            },
        };
        Box::pin(async move { Ok(r) })
    }

    fn settle<'a>(
        &'a self,
        req: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        let network = req.payment_requirements.network.clone();
        let r = match (self.check(req), Self::nonce(req)) {
            (Ok(payer), Some(nonce)) => {
                let fresh = self
                    .used_nonces
                    .lock()
                    .map(|mut u| u.insert(nonce.clone()))
                    .unwrap_or(false);
                if fresh {
                    SettleResponse {
                        success: true,
                        error_reason: None,
                        payer: Some(payer),
                        transaction: format!(
                            "0x{}",
                            hex::encode(onyx_core::keccak256(nonce.as_bytes()).0)
                        ),
                        network,
                    }
                } else {
                    SettleResponse {
                        success: false,
                        error_reason: Some("invalid_transaction_state".into()),
                        payer: Some(payer),
                        transaction: String::new(),
                        network,
                    }
                }
            }
            (Ok(_), None) | (Err(_), _) => SettleResponse {
                success: false,
                error_reason: Some("invalid_payload".into()),
                payer: None,
                transaction: String::new(),
                network,
            },
        };
        Box::pin(async move { Ok(r) })
    }
}

/// HTTP transport for [`HttpFacilitator`]: POST a JSON body, return the
/// status code and body bytes. The concrete client (TLS) is supplied by the
/// binary that deploys against a real facilitator; tests use a fake.
pub trait HttpPost: Send + Sync {
    /// POST `body` as `application/json` to `url`.
    fn post<'a>(
        &'a self,
        url: &'a str,
        body: Vec<u8>,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), String>>;
}

/// Public x402.org facilitator (PLAN q-M7-1 a): supports Base Sepolia and
/// needs no account or API key. Used only by the live testnet run; tests
/// never call it.
pub const X402_ORG_FACILITATOR: &str = "https://x402.org/facilitator";

/// Environment variable that overrides the facilitator base URL.
pub const FACILITATOR_URL_ENV: &str = "ONYX_X402_FACILITATOR";

/// Networks this project is paid on without an explicit mainnet switch.
/// Onyx is testnet-only (CLAUDE.md hard limits): only Base Sepolia.
pub const TESTNET_NETWORKS: &[&str] = &[BASE_SEPOLIA];

/// Refuse a payment network outside [`TESTNET_NETWORKS`] unless
/// `allow_mainnet` is set (M7-L47-F4). The switch exists so the refusal is
/// explicit; this project never sets it.
pub fn check_network(network: &str, allow_mainnet: bool) -> Result<(), String> {
    if allow_mainnet || TESTNET_NETWORKS.contains(&network) {
        Ok(())
    } else {
        Err(format!(
            "network {network} is not a testnet this project accepts ({}); refusing without --allow-mainnet",
            TESTNET_NETWORKS.join(", ")
        ))
    }
}

/// Refuse a facilitator URL that is not `https://` (M7-L48-F4): verify and
/// settle carry payment authorizations and decide what is served. Plain
/// `http://` is allowed only to exactly `localhost`, `127.0.0.1` or `[::1]`
/// (a dev facilitator on this machine). The URL is parsed as a URI (M7-L50-F3):
/// it needs a host, no user info, and a numeric port if any; [`HyperPost`]
/// re-checks every request URL with this function. A base URL with a query or
/// fragment is refused (M7-L51-F3): `/verify` would land inside it.
pub fn check_facilitator_url(url: &str) -> Result<(), String> {
    let refuse = || {
        format!(
            "facilitator URL {url:?} must be https:// (http:// only to localhost, 127.0.0.1 or [::1])"
        )
    };
    if url.contains(['?', '#']) {
        return Err(refuse());
    }
    let uri: axum::http::Uri = url.parse().map_err(|_| refuse())?;
    let authority = uri.authority().ok_or_else(refuse)?;
    let host = authority.host();
    // User info (`a@b`) and empty hosts are never a facilitator.
    if authority.as_str().contains('@') || host.is_empty() {
        return Err(refuse());
    }
    // After the host, only `:port` with a number (no `:abc`, no `]x`).
    let rest = &authority.as_str()[host.len()..];
    if !rest.is_empty() && (!rest.starts_with(':') || authority.port_u16().is_none()) {
        return Err(refuse());
    }
    match uri.scheme_str() {
        Some("https") => Ok(()),
        Some("http") if is_loopback_host(host) => Ok(()),
        _ => Err(refuse()),
    }
}

/// Exactly `localhost`, `127.0.0.1` or `[::1]` (any letter case).
fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "[::1]"
}

/// Facilitator base URL for `network`: the explicit `override_url` when
/// given (from [`FACILITATOR_URL_ENV`] or config), else the default for the
/// network. The network must pass [`check_network`] and the URL
/// [`check_facilitator_url`]. Only Base Sepolia has a default; any other
/// network needs an explicit URL (never a mainnet default).
pub fn facilitator_url(
    network: &str,
    override_url: Option<&str>,
    allow_mainnet: bool,
) -> Result<String, String> {
    check_network(network, allow_mainnet)?;
    match override_url.map(str::trim).filter(|u| !u.is_empty()) {
        Some(u) => {
            check_facilitator_url(u)?;
            Ok(u.trim_end_matches('/').to_string())
        }
        None if network == BASE_SEPOLIA => Ok(X402_ORG_FACILITATOR.to_string()),
        None => Err(format!(
            "no default facilitator for {network}; set {FACILITATOR_URL_ENV}"
        )),
    }
}

/// [`facilitator_url`] with the override read from [`FACILITATOR_URL_ENV`]
/// through `env` (the server passes `|k| std::env::var(k).ok()`).
pub fn facilitator_url_from_env(
    network: &str,
    allow_mainnet: bool,
    env: impl Fn(&str) -> Option<String>,
) -> Result<String, String> {
    facilitator_url(network, env(FACILITATOR_URL_ENV).as_deref(), allow_mainnet)
}

/// A facilitator answer that is a payment decision.
trait Verdict {
    /// Whether it approves (a valid payment, a successful settle).
    fn approves(&self) -> bool;
}

impl Verdict for VerifyResponse {
    fn approves(&self) -> bool {
        self.is_valid
    }
}

impl Verdict for SettleResponse {
    fn approves(&self) -> bool {
        self.success
    }
}

/// A remote facilitator (by default the public x402.org facilitator on Base
/// Sepolia, [`X402_ORG_FACILITATOR`]), spoken to with the spec's JSON bodies
/// at `{base}/verify` and `{base}/settle`.
pub struct HttpFacilitator<T: HttpPost> {
    base: String,
    http: T,
}

impl<T: HttpPost> HttpFacilitator<T> {
    /// Facilitator at `base` (no trailing slash needed).
    pub fn new(base: impl Into<String>, http: T) -> Self {
        HttpFacilitator {
            base: base.into().trim_end_matches('/').to_string(),
            http,
        }
    }

    async fn call<R: for<'de> Deserialize<'de> + Verdict>(
        &self,
        path: &str,
        req: &FacilitatorRequest,
    ) -> Result<R, FacilitatorError> {
        let body =
            serde_json::to_vec(req).map_err(|e| FacilitatorError::Unavailable(e.to_string()))?;
        let url = format!("{}/{path}", self.base);
        let (status, bytes) = self
            .http
            .post(&url, body)
            .await
            .map_err(FacilitatorError::Unavailable)?;
        // A verdict (valid or not) comes back as JSON; anything that is not the
        // expected JSON is an outage, never a payment decision.
        let r: R = serde_json::from_slice(&bytes)
            .map_err(|e| FacilitatorError::Unavailable(format!("status {status}: {e}")))?;
        // A non-2xx answer counts only as a refusal (some facilitators send a
        // 400 with `isValid: false`); an approval under an error status is an
        // outage, never a payment (M7-L47-F3).
        if !(200..300).contains(&status) && r.approves() {
            return Err(FacilitatorError::Unavailable(format!(
                "status {status} with an approving body"
            )));
        }
        Ok(r)
    }
}

impl<T: HttpPost> Facilitator for HttpFacilitator<T> {
    fn verify<'a>(
        &'a self,
        req: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>> {
        Box::pin(self.call("verify", req))
    }

    fn settle<'a>(
        &'a self,
        req: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        Box::pin(self.call("settle", req))
    }
}

/// Largest facilitator answer read (a verdict is a few hundred bytes).
#[cfg(feature = "http-client")]
pub const MAX_FACILITATOR_BODY: usize = 64 * 1024;

/// Largest JSON-RPC answer read from a node (M8-L55-F5): a busy block from
/// `eth_getBlockByNumber` lists every transaction hash (about 69 bytes each)
/// and the settlement's `eth_getCode` is about 46 KB of hex.
#[cfg(feature = "http-client")]
pub const MAX_RPC_BODY: usize = 1024 * 1024;

/// Largest JSON-RPC answer the chain indexer reads (IDX-R1-2): an
/// `eth_getLogs` answer of [`crate::indexer::MAX_LOGS_PER_RANGE`] `Finalized`
/// logs (about 750 bytes of JSON each, 7.5 MB) fits; one block with more
/// (Base Sepolia's blocks hold 1.2 billion gas, 2026-10-12) is read for the
/// held shards only instead. One call at a time per indexer.
#[cfg(feature = "http-client")]
pub const MAX_LOGS_RPC_BODY: usize = 16 * 1024 * 1024;

/// HTTPS transport for [`HttpFacilitator`] (hyper with rustls and the
/// webpki roots, feature `http-client`; M7-L47-F4). HTTPS, or plain http
/// only to a loopback dev facilitator (every URL passes
/// [`check_facilitator_url`] before it is sent, M7-L50-F3); no
/// redirects, one deadline per call, and answers over
/// [`MAX_FACILITATOR_BODY`] are refused. [`HyperPost::for_rpc`] is the same
/// transport for a JSON-RPC node, with its own cap ([`MAX_RPC_BODY`]) and
/// errors that name the node (M8-L55-F5).
#[cfg(feature = "http-client")]
pub struct HyperPost {
    client: hyper_util::client::legacy::Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        http_body_util::Full<hyper::body::Bytes>,
    >,
    timeout: std::time::Duration,
    max_body: usize,
    peer: &'static str,
}

#[cfg(feature = "http-client")]
impl HyperPost {
    /// A facilitator client whose calls give up after `timeout`.
    pub fn new(timeout: std::time::Duration) -> Self {
        Self::with_limits(timeout, MAX_FACILITATOR_BODY, "facilitator")
    }

    /// The facilitator client a server uses: its calls give up only after
    /// the whole settle patience ([`crate::SETTLE_PATIENCE`] times
    /// `config.facilitator_timeout`), never before the server's own settle
    /// timeout (OPS-L2-F1). `verify` keeps its own shorter timeout, applied
    /// by the server around the call.
    pub fn for_facilitator(config: &crate::ApiConfig) -> Self {
        Self::new(config.facilitator_timeout * crate::SETTLE_PATIENCE)
    }

    /// A JSON-RPC node client whose calls give up after `timeout`; answers up
    /// to [`MAX_RPC_BODY`] (M8-L55-F5).
    pub fn for_rpc(timeout: std::time::Duration) -> Self {
        Self::with_limits(timeout, MAX_RPC_BODY, "node")
    }

    /// The chain indexer's node client: answers up to [`MAX_LOGS_RPC_BODY`]
    /// (IDX-R1-2).
    pub fn for_logs(timeout: std::time::Duration) -> Self {
        Self::with_limits(timeout, MAX_LOGS_RPC_BODY, "node")
    }

    fn with_limits(timeout: std::time::Duration, max_body: usize, peer: &'static str) -> Self {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            // Plain http is reachable only for URLs that pass
            // `check_facilitator_url` (loopback dev facilitators), checked
            // again in `send` (M7-L50-F3).
            .https_or_http()
            .enable_http1()
            .build();
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(https);
        HyperPost {
            client,
            timeout,
            max_body,
            peer,
        }
    }

    /// The largest answer this client reads.
    pub fn max_body(&self) -> usize {
        self.max_body
    }

    async fn send(&self, url: &str, body: Vec<u8>) -> Result<(u16, Vec<u8>), String> {
        use http_body_util::BodyExt as _;
        // The same URL rule for both peers; the error names this one
        // (M8-L56-F5).
        check_facilitator_url(url).map_err(|e| e.replacen("facilitator", self.peer, 1))?;
        let req = hyper::Request::post(url)
            .header("content-type", "application/json")
            .body(http_body_util::Full::new(hyper::body::Bytes::from(body)))
            .map_err(|e| e.to_string())?;
        let peer = self.peer;
        let r = self
            .client
            .request(req)
            .await
            .map_err(|e| format!("{peer} unreachable: {e}"))?;
        let status = r.status().as_u16();
        let bytes = http_body_util::Limited::new(r.into_body(), self.max_body)
            .collect()
            .await
            .map_err(|e| {
                format!(
                    "{peer} answer unreadable or over {} bytes: {e}",
                    self.max_body
                )
            })?
            .to_bytes();
        Ok((status, bytes.to_vec()))
    }
}

#[cfg(feature = "http-client")]
impl HttpPost for HyperPost {
    fn post<'a>(
        &'a self,
        url: &'a str,
        body: Vec<u8>,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), String>> {
        Box::pin(async move {
            tokio::time::timeout(self.timeout, self.send(url, body))
                .await
                .map_err(|_| format!("{} call timed out", self.peer))?
        })
    }
}

/// Where and in what to get paid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentTerms {
    /// CAIP-2 network id ([`BASE_SEPOLIA`] on testnet).
    pub network: String,
    /// Token contract (testnet USDC on Base Sepolia; configuration, not code).
    pub asset: String,
    /// Recipient address.
    pub pay_to: String,
    /// Payment timeout.
    pub max_timeout_seconds: u64,
    /// EIP-712 domain of the token, e.g. `{"name":"USDC","version":"2"}`.
    pub extra: Option<serde_json::Value>,
}

impl PaymentTerms {
    /// The single `exact` requirement for `amount` atomic units.
    pub fn requirements(&self, amount: u64) -> PaymentRequirements {
        PaymentRequirements {
            scheme: "exact".into(),
            network: self.network.clone(),
            amount: amount.to_string(),
            asset: self.asset.clone(),
            pay_to: self.pay_to.clone(),
            max_timeout_seconds: self.max_timeout_seconds,
            extra: self.extra.clone(),
        }
    }
}

/// The `exact` scheme on EVM: an EIP-3009 `transferWithAuthorization`
/// signed with EIP-712 over the token's domain (x402 `exact` EVM spec). The
/// server checks it locally before it spends a facilitator call (M7-F4).
pub mod exact_evm {
    use super::PaymentRequirements;
    use onyx_core::keccak256;
    use serde_json::{json, Value};

    /// `keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)")`.
    pub const DOMAIN_TYPEHASH: &str =
        "0x8b73c3c69bb8fe3d512ecc4cf759cc79239f7b179b0ffacaa9a75d522b39400f";
    /// `keccak256("TransferWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)")`.
    pub const TRANSFER_TYPEHASH: &str =
        "0x7c7c6cdb67a18743f49ec6fa9b35f50d52ed05cbed4cc592e13b44501c1a2267";

    /// A parsed EIP-3009 authorization.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Authorization {
        /// Payer.
        pub from: [u8; 20],
        /// Recipient.
        pub to: [u8; 20],
        /// Amount in atomic units.
        pub value: u128,
        /// Valid strictly after this unix time.
        pub valid_after: u64,
        /// Valid strictly before this unix time.
        pub valid_before: u64,
        /// Unique nonce (single use per payer, on chain).
        pub nonce: [u8; 32],
    }

    fn word_u128(v: u128) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[16..].copy_from_slice(&v.to_be_bytes());
        w
    }

    fn word_addr(a: &[u8; 20]) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[12..].copy_from_slice(a);
        w
    }

    fn fixed<const N: usize>(s: &str) -> Option<[u8; N]> {
        let b = hex::decode(s.strip_prefix("0x")?).ok()?;
        b.try_into().ok()
    }

    fn typehash(s: &str) -> [u8; 32] {
        fixed::<32>(s).unwrap_or([0u8; 32])
    }

    /// The EIP-155 chain id of a CAIP-2 `eip155:<id>` network.
    pub fn chain_id(network: &str) -> Option<u64> {
        network.strip_prefix("eip155:")?.parse().ok()
    }

    /// The EIP-712 digest the payer signs for `auth` under `req`'s token
    /// domain (`extra.name`, `extra.version`, chain id, `asset`).
    pub fn digest(auth: &Authorization, req: &PaymentRequirements) -> Option<[u8; 32]> {
        let name = req.extra.as_ref()?.get("name")?.as_str()?;
        let version = req.extra.as_ref()?.get("version")?.as_str()?;
        let chain = chain_id(&req.network)?;
        let asset = fixed::<20>(&req.asset.to_lowercase())?;
        let mut d = typehash(DOMAIN_TYPEHASH).to_vec();
        d.extend_from_slice(&keccak256(name.as_bytes()).0);
        d.extend_from_slice(&keccak256(version.as_bytes()).0);
        d.extend_from_slice(&word_u128(u128::from(chain)));
        d.extend_from_slice(&word_addr(&asset));
        let domain = keccak256(&d);
        let mut s = typehash(TRANSFER_TYPEHASH).to_vec();
        s.extend_from_slice(&word_addr(&auth.from));
        s.extend_from_slice(&word_addr(&auth.to));
        s.extend_from_slice(&word_u128(auth.value));
        s.extend_from_slice(&word_u128(u128::from(auth.valid_after)));
        s.extend_from_slice(&word_u128(u128::from(auth.valid_before)));
        s.extend_from_slice(&auth.nonce);
        let st = keccak256(&s);
        let mut m = vec![0x19, 0x01];
        m.extend_from_slice(&domain.0);
        m.extend_from_slice(&st.0);
        Some(keccak256(&m).0)
    }

    /// Recover the signer of `digest` from a 65-byte `r || s || v` signature
    /// (v in {27, 28} or {0, 1}); high-s is refused (EIP-2).
    pub fn recover(digest: &[u8; 32], sig: &[u8]) -> Option<[u8; 20]> {
        if sig.len() != 65 {
            return None;
        }
        let v = match sig[64] {
            27 | 0 => 0,
            28 | 1 => 1,
            _ => return None,
        };
        let id = secp256k1::ecdsa::RecoveryId::from_i32(v).ok()?;
        let rs = secp256k1::ecdsa::RecoverableSignature::from_compact(&sig[..64], id).ok()?;
        let mut normal = rs.to_standard();
        normal.normalize_s();
        if normal.serialize_compact()[32..] != sig[32..64] {
            return None;
        }
        let msg = secp256k1::Message::from_digest(*digest);
        let pk = secp256k1::Secp256k1::verification_only()
            .recover_ecdsa(&msg, &rs)
            .ok()?;
        let mut a = [0u8; 20];
        a.copy_from_slice(&keccak256(&pk.serialize_uncompressed()[1..]).0[12..]);
        Some(a)
    }

    fn parse(auth: &Value) -> Option<Authorization> {
        Some(Authorization {
            from: fixed::<20>(&auth["from"].as_str()?.to_lowercase())?,
            to: fixed::<20>(&auth["to"].as_str()?.to_lowercase())?,
            value: auth["value"].as_str()?.parse().ok()?,
            valid_after: auth["validAfter"].as_str()?.parse().ok()?,
            valid_before: auth["validBefore"].as_str()?.parse().ok()?,
            nonce: fixed::<32>(&auth["nonce"].as_str()?.to_lowercase())?,
        })
    }

    /// Local checks of an `exact` EVM payload against `req` at unix time
    /// `now`: well formed, the exact amount to the exact recipient, inside its
    /// validity window, and signed by `from` with a 65-byte EOA signature.
    /// Other signatures (smart-contract wallets, EIP-1271/6492) are refused
    /// here and never reach the facilitator (M7-L39-F4). Errors are x402
    /// reason codes.
    pub fn precheck(
        payload: &Value,
        req: &PaymentRequirements,
        now: u64,
    ) -> Result<Authorization, &'static str> {
        if req.scheme != "exact" {
            return Err("unsupported_scheme");
        }
        let auth = parse(&payload["authorization"]).ok_or("invalid_payload")?;
        if req.amount.parse::<u128>().ok() != Some(auth.value) {
            return Err("invalid_exact_evm_payload_authorization_value_mismatch");
        }
        let pay_to =
            fixed::<20>(&req.pay_to.to_lowercase()).ok_or("invalid_payment_requirements")?;
        if auth.to != pay_to {
            return Err("invalid_exact_evm_payload_recipient_mismatch");
        }
        if now <= auth.valid_after {
            return Err("invalid_exact_evm_payload_authorization_valid_after");
        }
        if now >= auth.valid_before {
            return Err("invalid_exact_evm_payload_authorization_valid_before");
        }
        let sig = payload["signature"]
            .as_str()
            .and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())
            .ok_or("invalid_payload")?;
        if sig.len() != 65 {
            return Err("invalid_exact_evm_payload_signature");
        }
        let digest = digest(&auth, req).ok_or("invalid_payment_requirements")?;
        if recover(&digest, &sig) != Some(auth.from) {
            return Err("invalid_exact_evm_payload_signature");
        }
        Ok(auth)
    }

    /// Sign `auth` with `secret` (a 32-byte secp256k1 key) and return the
    /// `exact` EVM payload. For tests and the demo only (test keys hold nothing).
    pub fn sign(
        secret: &[u8; 32],
        auth: &Authorization,
        req: &PaymentRequirements,
    ) -> Option<Value> {
        let key = secp256k1::SecretKey::from_slice(secret).ok()?;
        let digest = digest(auth, req)?;
        let (id, rs) = secp256k1::Secp256k1::signing_only()
            .sign_ecdsa_recoverable(&secp256k1::Message::from_digest(digest), &key)
            .serialize_compact();
        let mut sig = rs.to_vec();
        sig.push(27 + u8::try_from(id.to_i32()).ok()?);
        Some(json!({
            "signature": format!("0x{}", hex::encode(sig)),
            "authorization": {
                "from": format!("0x{}", hex::encode(auth.from)),
                "to": format!("0x{}", hex::encode(auth.to)),
                "value": auth.value.to_string(),
                "validAfter": auth.valid_after.to_string(),
                "validBefore": auth.valid_before.to_string(),
                "nonce": format!("0x{}", hex::encode(auth.nonce)),
            }
        }))
    }

    /// The EVM address of a secp256k1 secret key.
    pub fn address(secret: &[u8; 32]) -> Option<[u8; 20]> {
        let key = secp256k1::SecretKey::from_slice(secret).ok()?;
        let pk = key.public_key(&secp256k1::Secp256k1::signing_only());
        let mut a = [0u8; 20];
        a.copy_from_slice(&keccak256(&pk.serialize_uncompressed()[1..]).0[12..]);
        Some(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The spec's own example header (transports-v2/http.md) decodes into our
    /// type and back without losing a field.
    #[test]
    fn spec_example_payment_required_round_trips() {
        let spec_header = "eyJ4NDAyVmVyc2lvbiI6MiwiZXJyb3IiOiJQQVlNRU5ULVNJR05BVFVSRSBoZWFkZXIgaXMgcmVxdWlyZWQiLCJyZXNvdXJjZSI6eyJ1cmwiOiJodHRwczovL2FwaS5leGFtcGxlLmNvbS9wcmVtaXVtLWRhdGEiLCJkZXNjcmlwdGlvbiI6IkFjY2VzcyB0byBwcmVtaXVtIG1hcmtldCBkYXRhIiwibWltZVR5cGUiOiJhcHBsaWNhdGlvbi9qc29uIn0sImFjY2VwdHMiOlt7InNjaGVtZSI6ImV4YWN0IiwibmV0d29yayI6ImVpcDE1NTo4NDUzMiIsImFtb3VudCI6IjEwMDAwIiwiYXNzZXQiOiIweDAzNkNiRDUzODQyYzU0MjY2MzRlNzkyOTU0MWVDMjMxOGYzZENGN2UiLCJwYXlUbyI6IjB4MjA5NjkzQmM2YWZjMEM1MzI4YkEzNkZhRjAzQzUxNEVGMzEyMjg3QyIsIm1heFRpbWVvdXRTZWNvbmRzIjo2MCwiZXh0cmEiOnsibmFtZSI6IlVTREMiLCJ2ZXJzaW9uIjoiMiJ9fV19";
        let pr: PaymentRequired = decode_header(spec_header).expect("spec example decodes");
        assert_eq!(pr.x402_version, 2);
        assert_eq!(pr.accepts[0].network, BASE_SEPOLIA);
        assert_eq!(pr.accepts[0].amount, "10000");
        assert_eq!(pr.accepts[0].max_timeout_seconds, 60);
        let back: serde_json::Value =
            serde_json::from_slice(&B64.decode(encode_header(&pr)).expect("b64")).expect("json");
        let orig: serde_json::Value =
            serde_json::from_slice(&B64.decode(spec_header).expect("b64")).expect("json");
        assert_eq!(back, orig);
    }

    #[test]
    fn malformed_and_oversized_headers_are_refused() {
        assert!(decode_header::<PaymentPayload>("not base64!").is_err());
        assert!(decode_header::<PaymentPayload>(&B64.encode(b"{}")).is_err());
        let big = "A".repeat(MAX_PAYMENT_HEADER + 1);
        assert!(decode_header::<PaymentPayload>(&big).is_err());
        // Exactly MAX_PAYMENT_HEADER bytes still decode (mutation L4):
        // base64 of 12,288 JSON bytes is 16,384 characters.
        let json = format!("{{\"a\":\"{}\"}}", "x".repeat(12_288 - 8));
        let at = B64.encode(json.as_bytes());
        assert_eq!(at.len(), MAX_PAYMENT_HEADER);
        assert!(decode_header::<serde_json::Value>(&at).is_ok());
    }

    fn terms() -> PaymentTerms {
        PaymentTerms {
            network: BASE_SEPOLIA.into(),
            asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".into(),
            pay_to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
            max_timeout_seconds: 60,
            extra: Some(json!({"name": "USDC", "version": "2"})),
        }
    }

    fn secret(label: &str) -> [u8; 32] {
        onyx_core::keccak256(label.as_bytes()).0
    }

    fn addr(label: &str) -> String {
        format!(
            "0x{}",
            hex::encode(exact_evm::address(&secret(label)).expect("key"))
        )
    }

    /// A request signed by `signer` authorizing `value` (the server asks `amount`).
    fn request(amount: u64, value: u128, signer: &str, nonce: u8) -> FacilitatorRequest {
        let req = terms().requirements(amount);
        let mut to = [0u8; 20];
        to.copy_from_slice(&hex::decode(req.pay_to.trim_start_matches("0x")).expect("hex"));
        let auth = exact_evm::Authorization {
            from: exact_evm::address(&secret(signer)).expect("key"),
            to,
            value,
            valid_after: 0,
            valid_before: 9_999_999_999,
            nonce: [nonce; 32],
        };
        FacilitatorRequest {
            x402_version: 2,
            payment_payload: PaymentPayload {
                x402_version: 2,
                resource: None,
                accepted: req.clone(),
                payload: exact_evm::sign(&secret(signer), &auth, &req).expect("signs"),
            },
            payment_requirements: req,
        }
    }

    #[tokio::test]
    async fn mock_facilitator_verifies_settles_once_and_refuses_underpayment() {
        let f = MockFacilitator::new([addr("payer a")]);
        let good = request(1000, 1000, "payer a", 1);
        assert!(f.verify(&good).await.expect("v").is_valid);
        assert!(f.settle(&good).await.expect("s").success);
        // Replay of the same authorization: refused by the nonce rule.
        assert!(!f.verify(&good).await.expect("v").is_valid);
        assert!(!f.settle(&good).await.expect("s").success);
        let under = request(1000, 999, "payer a", 2);
        assert_eq!(
            f.verify(&under).await.expect("v").invalid_reason.as_deref(),
            Some("invalid_exact_evm_payload_authorization_value_mismatch")
        );
        let broke = request(1000, 1000, "payer b", 3);
        assert_eq!(
            f.verify(&broke).await.expect("v").invalid_reason.as_deref(),
            Some("insufficient_funds")
        );
    }

    /// Mutation L4: the mock facilitator refuses a wrong x402 version in
    /// either the request or its payload.
    #[tokio::test]
    async fn mock_facilitator_checks_both_versions() {
        let f = MockFacilitator::new([addr("payer a")]);
        let mut outer = request(1000, 1000, "payer a", 7);
        outer.x402_version = 1;
        let mut inner = request(1000, 1000, "payer a", 8);
        inner.payment_payload.x402_version = 1;
        for r in [outer, inner] {
            assert_eq!(
                f.verify(&r).await.expect("v").invalid_reason.as_deref(),
                Some("invalid_x402_version")
            );
        }
    }

    /// Mutation L4: the mock facilitator judges the validity window at the
    /// real time, so an authorization that expired in 1970 is refused.
    #[tokio::test]
    async fn mock_facilitator_uses_the_real_clock() {
        let f = MockFacilitator::new([addr("payer a")]);
        let mut old = request(1000, 1000, "payer a", 9);
        let req = old.payment_requirements.clone();
        let mut to = [0u8; 20];
        to.copy_from_slice(&hex::decode(req.pay_to.trim_start_matches("0x")).expect("hex"));
        let auth = exact_evm::Authorization {
            from: exact_evm::address(&secret("payer a")).expect("key"),
            to,
            value: 1000,
            valid_after: 0,
            valid_before: 2_000,
            nonce: [9; 32],
        };
        old.payment_payload.payload =
            exact_evm::sign(&secret("payer a"), &auth, &req).expect("signs");
        assert_eq!(
            f.verify(&old).await.expect("v").invalid_reason.as_deref(),
            Some("invalid_exact_evm_payload_authorization_valid_before")
        );
    }

    /// Mutation L4: the EIP-712 digest is the token's, not only consistent
    /// with our own signer. Foundry 1.8.5 signs the same typed data (USDC
    /// "2" on Base Sepolia, the test terms, nonce 0x04..04) with the test key
    /// keccak256("payer a"):
    /// `cast wallet sign --data --from-file typed.json --private-key $(cast keccak "payer a")`.
    /// RFC 6979 signatures are deterministic, so ours must be byte-equal.
    #[test]
    fn the_eip712_digest_matches_an_independent_signer() {
        let ok = request(1000, 1000, "payer a", 4);
        assert_eq!(
            addr("payer a"),
            "0xfc5a4ce1518d07ae89a9907eabfe6f4300b3b853"
        );
        assert_eq!(
            ok.payment_payload.payload["signature"],
            "0x881fc2d38ca0493dbabed16316da570ddfe940e23101d7d6773ffe748be404c3\
             38cc41b5063808e8007f9652159f8966310091167b4eb04a543a8a0f63f685451b"
        );
    }

    #[test]
    fn precheck_refuses_forged_expired_and_wrong_domain_payloads() {
        let ok = request(1000, 1000, "payer a", 4);
        let req = &ok.payment_requirements;
        let p = &ok.payment_payload.payload;
        assert!(exact_evm::precheck(p, req, 1_000).is_ok());
        // Claiming to be someone else: the signature recovers the real signer.
        let mut forged = p.clone();
        forged["authorization"]["from"] = json!(addr("payer b"));
        assert_eq!(
            exact_evm::precheck(&forged, req, 1_000),
            Err("invalid_exact_evm_payload_signature")
        );
        // Outside the validity window.
        assert_eq!(
            exact_evm::precheck(p, req, 9_999_999_999),
            Err("invalid_exact_evm_payload_authorization_valid_before")
        );
        // Not a 65-byte EOA signature (e.g. a contract-wallet blob): refused
        // locally, never sent on (M7-L39-F4).
        for len in [0usize, 64, 66, 200] {
            let mut long = p.clone();
            long["signature"] = json!(format!("0x{}", "11".repeat(len)));
            assert_eq!(
                exact_evm::precheck(&long, req, 1_000),
                Err("invalid_exact_evm_payload_signature"),
                "{len}-byte signature"
            );
        }
        // Same signature under another token domain (chain) does not verify.
        let mut other = req.clone();
        other.network = "eip155:8453".into();
        assert_eq!(
            exact_evm::precheck(p, &other, 1_000),
            Err("invalid_exact_evm_payload_signature")
        );
        // A high-s twin of a valid signature is refused (EIP-2).
        let sig = hex::decode(
            p["signature"]
                .as_str()
                .expect("sig")
                .trim_start_matches("0x"),
        )
        .expect("hex");
        let digest = exact_evm::digest(
            &exact_evm::Authorization {
                from: exact_evm::address(&secret("payer a")).expect("k"),
                to: {
                    let mut t = [0u8; 20];
                    t.copy_from_slice(
                        &hex::decode(req.pay_to.trim_start_matches("0x")).expect("h"),
                    );
                    t
                },
                value: 1000,
                valid_after: 0,
                valid_before: 9_999_999_999,
                nonce: [4; 32],
            },
            req,
        )
        .expect("digest");
        assert!(exact_evm::recover(&digest, &sig).is_some());
        // n - s, with v flipped.
        let n = hex::decode("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141")
            .expect("n");
        let mut high = sig.clone();
        let mut borrow = 0i16;
        for i in (0..32).rev() {
            let d = i16::from(n[i]) - i16::from(sig[32 + i]) - borrow;
            borrow = i16::from(d < 0);
            high[32 + i] = u8::try_from((d + 256) % 256).expect("byte");
        }
        high[64] = if sig[64] == 27 { 28 } else { 27 };
        assert_eq!(exact_evm::recover(&digest, &high), None);
    }

    /// Fake transport: records the request and answers a canned status and body.
    struct Fake {
        status: u16,
        answer: Vec<u8>,
        seen: Mutex<Vec<(String, serde_json::Value)>>,
    }
    impl HttpPost for Fake {
        fn post<'a>(
            &'a self,
            url: &'a str,
            body: Vec<u8>,
        ) -> BoxFuture<'a, Result<(u16, Vec<u8>), String>> {
            if let Ok(mut s) = self.seen.lock() {
                s.push((
                    url.into(),
                    serde_json::from_slice(&body).unwrap_or_default(),
                ));
            }
            let (st, a) = (self.status, self.answer.clone());
            Box::pin(async move { Ok((st, a)) })
        }
    }

    #[test]
    fn facilitator_defaults_to_x402_org_on_base_sepolia_only() {
        assert_eq!(
            facilitator_url(BASE_SEPOLIA, None, false).as_deref(),
            Ok("https://x402.org/facilitator")
        );
        assert_eq!(
            facilitator_url(BASE_SEPOLIA, Some(" https://facilitator.x402.rs/ "), false).as_deref(),
            Ok("https://facilitator.x402.rs")
        );
        assert_eq!(
            facilitator_url(BASE_SEPOLIA, Some(""), false).as_deref(),
            Ok(X402_ORG_FACILITATOR)
        );
        // Base mainnet: refused without the switch, and never given a default.
        assert!(facilitator_url("eip155:8453", Some("https://f.example"), false).is_err());
        assert!(facilitator_url("eip155:8453", None, true).is_err());
        assert!(facilitator_url("eip155:8453", Some("https://f.example"), true).is_ok());
    }

    /// M7-L47-F4: the override is read from `ONYX_X402_FACILITATOR`, and a
    /// mainnet network is refused unless `--allow-mainnet` is set.
    #[test]
    fn facilitator_env_override_and_testnet_allowlist() {
        let env = |k: &str| (k == FACILITATOR_URL_ENV).then(|| "https://f.example/".to_string());
        assert_eq!(
            facilitator_url_from_env(BASE_SEPOLIA, false, env).as_deref(),
            Ok("https://f.example")
        );
        assert_eq!(
            facilitator_url_from_env(BASE_SEPOLIA, false, |_| None).as_deref(),
            Ok(X402_ORG_FACILITATOR)
        );
        assert!(facilitator_url_from_env("eip155:8453", false, env).is_err());
        assert!(check_network(BASE_SEPOLIA, false).is_ok());
        assert!(check_network("eip155:1", false).is_err());
        assert!(check_network("eip155:1", true).is_ok());
    }

    /// M7-L48-F4: a facilitator URL must be https; plain http only to this
    /// machine (a dev facilitator). Checked at startup, before any request.
    #[test]
    fn facilitator_urls_must_be_https_except_on_localhost() {
        for ok in [
            "https://f.example",
            "HTTPS://f.example/x",
            "http://localhost:4021",
            "http://127.0.0.1:4021/facilitator",
            "http://[::1]:4021",
            "http://localhost",
            "http://LocalHost:4021/verify",
        ] {
            assert!(check_facilitator_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://f.example",
            "http://localhost.f.example",
            "http://127.0.0.1.f.example",
            "http://localhost@f.example",
            "http://10.0.0.1:4021",
            "ftp://f.example",
            "f.example",
            "https://",
            "",
            // M7-L50-F3: odd hosts the old string checker let through.
            "http://localhost:abc",
            "http://localhost:4021@f.example",
            "http://[::1]x",
            "http://[::1]:4021@f.example/",
            "http://[::2]:4021",
            "http://127.0.0.1\\@f.example",
            "http://localhost./",
            "https://user@f.example",
            "https://:443",
            // M7-L51-F3: `/verify` would be appended inside these.
            "http://localhost#@evil.example",
            "http://localhost?x",
            "https://f.example/?a=b",
        ] {
            assert!(check_facilitator_url(bad).is_err(), "{bad}");
        }
        assert!(facilitator_url(BASE_SEPOLIA, Some("http://f.example"), false).is_err());
        let env = |k: &str| (k == FACILITATOR_URL_ENV).then(|| "http://f.example".to_string());
        assert!(facilitator_url_from_env(BASE_SEPOLIA, false, env).is_err());
        assert_eq!(
            facilitator_url(BASE_SEPOLIA, Some("http://127.0.0.1:4021/"), false).as_deref(),
            Ok("http://127.0.0.1:4021")
        );
    }

    #[tokio::test]
    async fn http_facilitator_speaks_the_spec_bodies() {
        let fake = Fake {
            status: 200,
            answer: br#"{"isValid":false,"invalidReason":"insufficient_funds","payer":"0xaaa"}"#
                .to_vec(),
            seen: Mutex::new(Vec::new()),
        };
        let f = HttpFacilitator::new("https://facilitator.example/", fake);
        let req = request(5, 5, "payer a", 9);
        let v = f.verify(&req).await.expect("answer");
        assert_eq!(v.invalid_reason.as_deref(), Some("insufficient_funds"));
        {
            let seen = f.http.seen.lock().expect("lock");
            assert_eq!(seen[0].0, "https://facilitator.example/verify");
            assert_eq!(seen[0].1["x402Version"], 2);
            assert_eq!(
                seen[0].1["paymentRequirements"]["payTo"],
                req.payment_requirements.pay_to
            );
            assert!(seen[0].1["paymentPayload"]["accepted"].is_object());
        }
        // A body that is not the expected JSON is an outage, not a verdict.
        let bad = HttpFacilitator::new(
            "x",
            Fake {
                status: 200,
                answer: b"<html>".to_vec(),
                seen: Mutex::new(Vec::new()),
            },
        );
        assert!(matches!(
            bad.settle(&req).await,
            Err(FacilitatorError::Unavailable(_))
        ));
    }

    /// M7-L47-F3: a non-2xx answer counts only as a refusal; an approval
    /// under an error status, or a non-JSON error page, is an outage.
    #[tokio::test]
    async fn http_facilitator_rejects_non_2xx_unless_it_is_a_refusal() {
        let req = request(5, 5, "payer a", 9);
        let with = |status: u16, body: &[u8]| {
            HttpFacilitator::new(
                "https://f.example",
                Fake {
                    status,
                    answer: body.to_vec(),
                    seen: Mutex::new(Vec::new()),
                },
            )
        };
        let refusal = with(
            400,
            br#"{"isValid":false,"invalidReason":"invalid_signature"}"#,
        );
        let v = refusal.verify(&req).await.expect("a refusal is a verdict");
        assert!(!v.is_valid);
        let approval = with(500, br#"{"isValid":true}"#);
        assert!(matches!(
            approval.verify(&req).await,
            Err(FacilitatorError::Unavailable(_))
        ));
        let settled = with(
            502,
            br#"{"success":true,"transaction":"0x01","network":"eip155:84532"}"#,
        );
        assert!(matches!(
            settled.settle(&req).await,
            Err(FacilitatorError::Unavailable(_))
        ));
        // A failed settle under an error status is a refusal, not an outage
        // (mutation L4).
        let failed = with(
            400,
            br#"{"success":false,"errorReason":"insufficient_funds","transaction":"","network":"eip155:84532"}"#,
        );
        assert!(!failed.settle(&req).await.expect("a refusal").success);
        let page = with(503, b"<html>busy</html>");
        assert!(matches!(
            page.settle(&req).await,
            Err(FacilitatorError::Unavailable(_))
        ));
        let ok = with(
            200,
            br#"{"success":true,"transaction":"0x01","network":"eip155:84532"}"#,
        );
        assert!(ok.settle(&req).await.expect("settled").success);
    }

    /// Mutation L4: each transport's answer cap and deadline.
    #[cfg(feature = "http-client")]
    #[tokio::test]
    async fn transports_have_their_caps_and_deadlines() {
        assert_eq!(MAX_FACILITATOR_BODY, 65_536);
        assert_eq!(MAX_RPC_BODY, 1_048_576);
        assert_eq!(MAX_LOGS_RPC_BODY, 16_777_216);
        let t = std::time::Duration::from_secs(3);
        assert_eq!(HyperPost::new(t).max_body(), MAX_FACILITATOR_BODY);
        assert_eq!(HyperPost::for_rpc(t).max_body(), MAX_RPC_BODY);
        assert_eq!(HyperPost::for_logs(t).max_body(), MAX_LOGS_RPC_BODY);
        assert_eq!(HyperPost::for_rpc(t).timeout, t);
        assert_eq!(HyperPost::for_logs(t).timeout, t);
        let cfg = crate::ApiConfig::default();
        assert_eq!(
            HyperPost::for_facilitator(&cfg).timeout,
            cfg.facilitator_timeout * crate::SETTLE_PATIENCE
        );
    }

    /// M8-L55-F5: a node answer bigger than the facilitator cap but under
    /// the RPC cap is read by the RPC client and refused by the facilitator
    /// client, with an error that names the peer.
    #[cfg(feature = "http-client")]
    #[tokio::test]
    async fn rpc_transport_has_its_own_body_cap() {
        use std::io::{Read as _, Write as _};
        let body = vec![b'x'; 100 * 1024];
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let served = body.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut c, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 4096];
                let _ = c.read(&mut buf);
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    served.len()
                );
                let _ = c
                    .write_all(head.as_bytes())
                    .and_then(|()| c.write_all(&served));
            }
        });
        let url = format!("http://127.0.0.1:{port}/");
        let t = std::time::Duration::from_secs(10);
        let rpc = HyperPost::for_rpc(t);
        assert_eq!(rpc.max_body(), MAX_RPC_BODY);
        assert_eq!(
            HyperPost::for_logs(std::time::Duration::from_secs(1)).max_body(),
            MAX_LOGS_RPC_BODY
        );
        let (status, got) = rpc.post(&url, b"{}".to_vec()).await.expect("rpc read");
        assert_eq!((status, got.len()), (200, body.len()));
        let fac = HyperPost::new(t);
        assert_eq!(fac.max_body(), MAX_FACILITATOR_BODY);
        let err = fac
            .post(&url, b"{}".to_vec())
            .await
            .expect_err("over the cap");
        assert!(err.starts_with("facilitator answer"), "{err}");
        server.join().expect("server");
    }

    /// M8-L56-F5: a refused URL is named after the peer that would be
    /// called, the node or the facilitator.
    #[cfg(feature = "http-client")]
    #[tokio::test]
    async fn a_refused_url_names_its_peer() {
        let t = std::time::Duration::from_secs(1);
        let url = "http://f.example/";
        let err = HyperPost::for_rpc(t)
            .post(url, b"{}".to_vec())
            .await
            .expect_err("refused");
        assert!(err.starts_with("node URL"), "{err}");
        let err = HyperPost::new(t)
            .post(url, b"{}".to_vec())
            .await
            .expect_err("refused");
        assert!(err.starts_with("facilitator URL"), "{err}");
    }
}
