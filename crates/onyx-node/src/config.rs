//! Service configuration from environment variables (docs/ops/config.md).
//!
//! Read once at start, validated, and never logged with secrets: the
//! operator key is read from a file whose path is configured, never from an
//! environment variable, and the node URL is shown by host only.

use onyx_api::Cidr;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Where the service runs. There is no mainnet environment (docs/ops/upgrades.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Env {
    /// A laptop or the dev box against a local chain (anvil, chain 31337).
    Dev,
    /// Base Sepolia (chain 84532).
    Testnet,
}

/// How paid requests are settled. There is deliberately no mock mode: the
/// demo drivers' mock facilitator cannot be selected here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaymentMode {
    /// Everything is free (no x402 terms are offered).
    Off,
    /// The server submits each payment's EIP-3009 authorization itself into
    /// `OnyxRevenue.settle` with the operator key (ADR-0026).
    SelfSettle,
    /// An HTTPS x402 facilitator verifies and settles; receipts are posted to
    /// `OnyxRevenue.credit` by the reporter when an operator key is set.
    Facilitator,
}

impl PaymentMode {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "off" => Ok(PaymentMode::Off),
            "self-settle" => Ok(PaymentMode::SelfSettle),
            "facilitator" => Ok(PaymentMode::Facilitator),
            other => Err(format!(
                "ONYX_PAYMENTS must be off, self-settle or facilitator, not {other:?}"
            )),
        }
    }
}

/// Everything the service needs, validated.
#[derive(Clone, Debug)]
pub struct NodeConfig {
    /// `ONYX_ENV`.
    pub env: Env,
    /// `ONYX_LISTEN` (default `127.0.0.1:8080`). The server speaks plain
    /// HTTP: an address that is not loopback is refused unless
    /// `ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1` (M7-7; put a TLS proxy in front,
    /// docs/ops/deployment.md "TLS in front").
    pub listen: SocketAddr,
    /// `ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1`: the operator accepts a listen
    /// address that is not loopback (a container published on 127.0.0.1
    /// only, a testnet lab). Default 0.
    pub allow_plain_http_public: bool,
    /// `ONYX_PUBLIC_URL`: the base URL clients use (x402 `resource.url`).
    pub public_url: String,
    /// The chain node (HTTPS, or http to loopback): `ONYX_RPC_URL`, or the
    /// content of the file at `ONYX_RPC_URL_FILE` (a credential: a provider
    /// URL often carries an API key, OPS-L1-F4).
    pub rpc_url: String,
    /// `ONYX_DEPLOYMENT`: the deployment manifest.
    pub deployment: PathBuf,
    /// `ONYX_CATALOG`: the catalog file.
    pub catalog: PathBuf,
    /// `ONYX_OBSERVATIONS`: level-2 results store files (comma-separated,
    /// optional), each a metric's template and its settled rounds, written
    /// by the level-2 round runner and checked at load (ADR-0027 amendment 4).
    pub observations: Vec<PathBuf>,
    /// `ONYX_OBSERVATION_TEMPLATES`: `metric=0x<template id>` pairs
    /// (comma-separated), the reviewed registration of every level-2 metric a
    /// store may serve (L2API-R4-F1). Required when `ONYX_OBSERVATIONS` is
    /// set: a store whose template is not pinned here is refused at load.
    pub observation_templates: onyx_api::observations::TemplatePins,
    /// `ONYX_SIGNED_REPORTS`: the level-3 coordinator's post log (ROADMAP
    /// C10, the coordinator's `PostLog`), re-read when it
    /// changes. Needs the manifest's `signedBook`; `/v1/signed/{fact}` serves
    /// a fact only against `settleable` at a finalized block.
    pub signed_reports: Option<PathBuf>,
    /// `ONYX_TRACK_PUBLICATIONS`: the track-record coordinator's publication
    /// file (ROADMAP C20, a Borsh `Publication`), re-read when it changes; `/v1/track` serves it. Unset: nothing
    /// published (every record a 404 `no_track_record`).
    pub track_publications: Option<PathBuf>,
    /// Opt-in durable intake journal (ADR-0039); unset means no submission routes.
    pub submissions: Option<PathBuf>,
    /// `ONYX_SALTED_LABELS`: comma-separated salted label set files served by
    /// `/v1/labels/{set}/{address}` (ROADMAP D30); only `test.*` sets load. Unset: no route.
    pub salted_labels: Vec<PathBuf>,
    /// `ONYX_CAS_DIR`: row batches (`<dir>/rows/<root>`).
    pub cas_dir: PathBuf,
    /// `ONYX_STATE_DIR`: receipt journals and credits in flight.
    pub state_dir: PathBuf,
    /// `ONYX_PAYMENTS`.
    pub payments: PaymentMode,
    /// `ONYX_OPERATOR_KEY_FILE`: the reporter key (0600 file of 64 hex digits).
    pub operator_key_file: Option<PathBuf>,
    /// Whether the reporter posts receipts to `OnyxRevenue.credit`
    /// (`ONYX_REPORTER=on|off`, default on). Facilitator mode with the
    /// reporter on needs the operator key; `off` keeps receipts on disk
    /// only, which must be chosen explicitly (OPS-L1-F5).
    pub reporter: bool,
    /// The facilitator URL (`ONYX_X402_FACILITATOR`, else the public x402.org
    /// facilitator for Base Sepolia); only in facilitator mode.
    pub facilitator_url: Option<String>,
    /// `ONYX_TRUSTED_PROXIES`: comma-separated CIDRs.
    pub trusted_proxies: Vec<Cidr>,
    /// `ONYX_ALLOW_TEST_LABELS=1`: serve shards pinned to a `test.*` label set.
    pub allow_test_labels: bool,
    /// `ONYX_RIGHTS=cleared|fallback` (default `cleared`; ROADMAP L14,
    /// THREATS L8-5): `fallback` serves no label-derived shard (entity flows,
    /// balances, reserves; 403 `label_rights_withheld`).
    pub rights: onyx_api::catalog::LabelRights,
    /// `--allow-mainnet` (command line only; this project never gives it).
    pub allow_mainnet: bool,
    /// `ONYX_REPORTER_EVERY_SECS` (default 30).
    pub reporter_every: Duration,
    /// `ONYX_PREPAID`: an `OnyxPrepaid` address (ROADMAP S7, ADR-0046 amendment 3): paid routes
    /// also take prepaid vouchers, the usage journal is `<state dir>/prepaid.journal`, and the
    /// reporter redeems and allocates every `ONYX_REPORTER_EVERY_SECS`. Facilitator mode with
    /// the reporter on only (the reporter's sender is shared, one nonce sequence per key).
    pub prepaid: Option<[u8; 20]>,
    /// `ONYX_OBSERVE`: the observe-on-payment file (ADR-0049, NORTH-STAR NS10): level-2 subjects
    /// no round settled yet are offered for a prepaid hold, and the operator's round command
    /// runs one round per paid fact. Needs `ONYX_PREPAID` (holds are prepaid vouchers).
    pub observe: Option<PathBuf>,
    /// `ONYX_GATE_REFRESH_SECS` (default 60, 1..=86400): time between runs
    /// of the serving checks (ADR-0034 Amendment 1, review RESAPI-R1 F3), so
    /// a listed shard that finalizes after the start is judged without a
    /// restart. Used only by a build with gates (`onyx-node-btc`).
    pub gate_every: Duration,
    /// EIP-712 domain of the payment token (`ONYX_USDC_EIP712_NAME`,
    /// `ONYX_USDC_EIP712_VERSION`; default `USDC`, `2` as on Base Sepolia).
    pub token_domain: (String, String),
    /// `ONYX_HEADER_READ_TIMEOUT_SECS` (default 10, 1..=120): a connection
    /// that has not sent its request headers by then is closed (AUD-1-F5).
    pub header_read_timeout: Duration,
    /// `ONYX_MAX_CONNECTIONS` (default 1024, 1..=65535): open connections;
    /// when all are taken, a new one evicts the oldest connection that has
    /// not sent a request, else it is closed (AUD-1-F5, AUD-1R-F1).
    pub max_connections: usize,
    /// `ONYX_MAX_CONNS_PER_IP` (default 32, 1..=65535): open connections per
    /// client (an IPv4 address or an IPv6 /64); trusted proxies are exempt
    /// (AUD-1R-F1).
    pub max_per_client: usize,
    /// `ONYX_MAX_FEE_GWEI` (default 10, 1..=10000): the fee cap of every
    /// transaction the node signs, sends and replacements alike (AUD-1R-F3).
    pub max_fee_per_gas: u128,
    /// `ONYX_MAX_GAS` (default 15000000, 21000..=30000000): a send whose gas
    /// estimate is above it is refused (AUD-1R-F3).
    pub max_gas: u64,
    /// The chain indexer (ADR-0033), when `ONYX_INDEXER=on`: the catalog's
    /// shards are discovered from `OnyxSettlement`'s `Finalized` events.
    pub indexer: Option<IndexerSettings>,
}

/// Chain indexer settings (`ONYX_INDEXER=on`, ADR-0033). The contract and
/// its deploy block come from the manifest, the node from `ONYX_RPC_URL`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexerSettings {
    /// `ONYX_INDEX_SPAN` (default 200, 1..=10000): blocks per `eth_getLogs`.
    pub span: u64,
    /// `ONYX_INDEX_OVERLAP` (default 100, 0..=10000): blocks read again each poll.
    pub overlap: u64,
    /// `ONYX_INDEX_EVERY_SECS` (default 15, 1..=3600): time between polls.
    pub every: Duration,
    /// `ONYX_INDEX_STATE` (default `$ONYX_STATE_DIR/indexer.json`): the cursor.
    pub state_file: PathBuf,
    /// `ONYX_INDEX_HEALTH` (`full` by default, or `summary`): `summary`
    /// shows counts only on `/v1/health` (no shard ids, no error text), for
    /// a node anyone can reach (IDX-R1-5).
    pub health_detail: bool,
    /// `ONYX_INDEX_RESCAN_POLLS` (default 240, 0..=1000000): polls between
    /// held-shard rescans from the deploy block, counted from the end of the
    /// previous one; 0 rescans after a start only (ADR-0033 Amendment 4).
    /// Or `ONYX_INDEX_RESCAN_SECS` (0..=31536000): the same as a time,
    /// rounded up to whole polls (IDX-R5-4). Not both.
    pub rescan_every: u64,
    /// `ONYX_INDEX_RESCAN_CALLS` (default 100, 1..=2000): `eth_getLogs`
    /// calls per poll for the held-shard rescan (IDX-R5-4).
    pub rescan_budget: u64,
    /// `ONYX_INDEX_MAX_JUMP` (default 100, 1..=1000000): blocks the scan may
    /// read past the `finalized` head two polls agree on (IDX-R5-5).
    pub max_jump: u64,
}

/// `metric=0x<id>` pairs, comma-separated; a metric named twice is refused.
fn template_pins(text: &str) -> Result<onyx_api::observations::TemplatePins, String> {
    let mut pins = onyx_api::observations::TemplatePins::new();
    for pair in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (metric, id) = pair
            .split_once('=')
            .ok_or("ONYX_OBSERVATION_TEMPLATES entries are metric=0x<template id>")?;
        let metric = metric.trim();
        if metric.is_empty() {
            return Err("ONYX_OBSERVATION_TEMPLATES: empty metric".into());
        }
        let id = crate::catalog_file::hash32(id.trim(), "ONYX_OBSERVATION_TEMPLATES template id")?;
        if pins.insert(metric.to_owned(), id).is_some() {
            return Err(format!("ONYX_OBSERVATION_TEMPLATES names {metric} twice"));
        }
    }
    Ok(pins)
}

fn required(get: &impl Fn(&str) -> Option<String>, k: &str) -> Result<String, String> {
    get(k)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("{k} is required"))
}

/// The host of a URL, for logs: never the path or query, which may carry a
/// node provider's API key. A value with no `scheme://` is not a URL at all
/// (a bare API key pasted into the file, OPS-L2-F4): it is never echoed.
pub fn url_host(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return "<not a URL; value withheld>".into();
    };
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("")
        .to_string()
}

impl NodeConfig {
    /// Read and validate the configuration from `get` (the environment in the
    /// binary, a map in tests). `allow_mainnet` comes from the command line.
    pub fn from_env(
        get: impl Fn(&str) -> Option<String>,
        allow_mainnet: bool,
    ) -> Result<Self, String> {
        let env = match required(&get, "ONYX_ENV")?.as_str() {
            "dev" => Env::Dev,
            "testnet" => Env::Testnet,
            other => {
                return Err(format!(
                "ONYX_ENV must be dev or testnet, not {other:?} (there is no mainnet environment)"
            ))
            }
        };
        let listen: SocketAddr = get("ONYX_LISTEN")
            .unwrap_or_else(|| "127.0.0.1:8080".into())
            .parse()
            .map_err(|_| "ONYX_LISTEN must be ip:port".to_string())?;
        let allow_plain_http_public = match get("ONYX_ALLOW_PLAIN_HTTP_PUBLIC").as_deref() {
            None | Some("") | Some("0") => false,
            Some("1") => true,
            Some(other) => {
                return Err(format!(
                    "ONYX_ALLOW_PLAIN_HTTP_PUBLIC must be 0 or 1, not {other:?}"
                ))
            }
        };
        // M7-7: this server has no TLS. A non-loopback address puts plain
        // HTTP (payment headers, answers) on a network unless something
        // else keeps it off one; that must be said explicitly.
        if !is_loopback(listen) && !allow_plain_http_public {
            return Err(format!(
                "ONYX_LISTEN {listen} is not a loopback address: this server speaks plain HTTP, and a public deployment must never be plain HTTP (THREATS.md M7-7). Bind 127.0.0.1 and put the TLS proxy in front (deploy/caddy/Caddyfile, docs/ops/deployment.md \"TLS in front\"); only where the port cannot be reached from outside (a container published on 127.0.0.1, a testnet lab) set ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1"
            ));
        }
        let public_url = required(&get, "ONYX_PUBLIC_URL")?
            .trim_end_matches('/')
            .to_string();
        // The public URL is part of every payment's `resource.url`; plain
        // http only for a loopback dev server.
        onyx_api::x402::check_facilitator_url(&public_url).map_err(|_| {
            format!(
                "ONYX_PUBLIC_URL must be https:// (http:// only to localhost), not {public_url:?}"
            )
        })?;
        let (rpc_url, rpc_source) = match (
            get("ONYX_RPC_URL").filter(|v| !v.trim().is_empty()),
            get("ONYX_RPC_URL_FILE").filter(|v| !v.trim().is_empty()),
        ) {
            (Some(u), None) => (u.trim().to_string(), "ONYX_RPC_URL"),
            (None, Some(f)) => (
                read_secret_file(std::path::Path::new(f.trim()), "ONYX_RPC_URL_FILE")?,
                "ONYX_RPC_URL_FILE",
            ),
            (Some(_), Some(_)) => {
                return Err("set ONYX_RPC_URL or ONYX_RPC_URL_FILE, not both".into())
            }
            (None, None) => return Err("ONYX_RPC_URL (or ONYX_RPC_URL_FILE) is required".into()),
        };
        // The error names where the value came from and only its host; a
        // value with no scheme is never echoed (OPS-L2-F4).
        onyx_api::x402::check_facilitator_url(&rpc_url).map_err(|_| {
            format!(
                "{rpc_source} must hold an https:// URL (http:// only to localhost); host: {}",
                url_host(&rpc_url)
            )
        })?;
        let payments = PaymentMode::parse(&get("ONYX_PAYMENTS").unwrap_or_else(|| "off".into()))?;
        let operator_key_file = get("ONYX_OPERATOR_KEY_FILE")
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from);
        if payments == PaymentMode::SelfSettle && operator_key_file.is_none() {
            return Err("ONYX_PAYMENTS=self-settle needs ONYX_OPERATOR_KEY_FILE".into());
        }
        let reporter = match get("ONYX_REPORTER").as_deref().map(str::trim) {
            None | Some("") | Some("on") => true,
            Some("off") => false,
            Some(other) => return Err(format!("ONYX_REPORTER must be on or off, not {other:?}")),
        };
        // Facilitator mode records receipts; without a reporter nobody posts
        // them, so contributors are never paid. That must be a choice, not a
        // missing setting (OPS-L1-F5).
        if payments == PaymentMode::Facilitator && reporter && operator_key_file.is_none() {
            return Err("ONYX_PAYMENTS=facilitator posts receipts with ONYX_OPERATOR_KEY_FILE; set it, or set ONYX_REPORTER=off to keep receipts on disk only".into());
        }
        let prepaid = match get("ONYX_PREPAID").as_deref().map(str::trim) {
            None | Some("") => None,
            Some(v) => {
                let a: [u8; 20] = v
                    .strip_prefix("0x")
                    .and_then(|h| hex::decode(h).ok())
                    .and_then(|b| b.try_into().ok())
                    .ok_or_else(|| format!("ONYX_PREPAID must be a 0x address, not {v:?}"))?;
                if payments != PaymentMode::Facilitator || !reporter || operator_key_file.is_none()
                {
                    return Err("ONYX_PREPAID needs ONYX_PAYMENTS=facilitator with the reporter on (ONYX_OPERATOR_KEY_FILE): its redemptions are sent by the reporter".into());
                }
                Some(a)
            }
        };
        let observe = get("ONYX_OBSERVE")
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        if observe.is_some() && prepaid.is_none() {
            return Err(
                "ONYX_OBSERVE needs ONYX_PREPAID: a round is paid by a prepaid hold".into(),
            );
        }
        let facilitator_url = match payments {
            PaymentMode::Facilitator => Some(onyx_api::x402::facilitator_url_from_env(
                onyx_api::x402::BASE_SEPOLIA,
                allow_mainnet,
                |k| get(k),
            )?),
            _ => None,
        };
        // A plain-http facilitator is a process on this machine that decides
        // what is paid: only on a dev box, never on testnet (OPS-L1-F3).
        if env != Env::Dev
            && facilitator_url
                .as_deref()
                .is_some_and(|u| !u.to_ascii_lowercase().starts_with("https://"))
        {
            return Err(
                "ONYX_X402_FACILITATOR must be https:// outside ONYX_ENV=dev (a loopback http facilitator is dev only)"
                    .into(),
            );
        }
        let trusted_proxies = get("ONYX_TRUSTED_PROXIES")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<Cidr>().map_err(|e| e.0))
            .collect::<Result<Vec<_>, _>>()?;
        let allow_test_labels = match get("ONYX_ALLOW_TEST_LABELS").as_deref() {
            None | Some("") | Some("0") => false,
            Some("1") => true,
            Some(other) => {
                return Err(format!(
                    "ONYX_ALLOW_TEST_LABELS must be 0 or 1, not {other:?}"
                ))
            }
        };
        // ROADMAP L14: exact `cleared` or `fallback`, anything else is
        // refused (L9A-6). Unset or empty is `cleared` only for `dev` (a
        // local chain); a `testnet` node is the one that can be public, and
        // the label-set rights are not cleared (LC-1), so there it must be
        // named: an unset value fails closed (review L14-R1, TRACKER).
        let rights = match (get("ONYX_RIGHTS").as_deref(), env) {
            (None | Some(""), Env::Dev) => onyx_api::catalog::LabelRights::Cleared,
            (None | Some(""), Env::Testnet) => {
                return Err("ONYX_RIGHTS is required with ONYX_ENV=testnet: set fallback (serve no metric derived from the entity label sets; launch-checklist LC-1 path B) or cleared (only once those rights are cleared in writing, LC-1 path A)".into())
            }
            (Some(v), _) => onyx_api::catalog::LabelRights::parse(v)?,
        };
        let reporter_every = match get("ONYX_REPORTER_EVERY_SECS") {
            None => Duration::from_secs(30),
            Some(s) => Duration::from_secs(
                s.parse::<u64>()
                    .ok()
                    .filter(|n| (1..=3_600).contains(n))
                    .ok_or("ONYX_REPORTER_EVERY_SECS must be 1..=3600")?,
            ),
        };
        let observations: Vec<PathBuf> = get("ONYX_OBSERVATIONS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .collect();
        let observation_templates =
            template_pins(&get("ONYX_OBSERVATION_TEMPLATES").unwrap_or_default())?;
        if !observations.is_empty() && observation_templates.is_empty() {
            return Err(
                "ONYX_OBSERVATIONS needs ONYX_OBSERVATION_TEMPLATES (metric=0x<template id>, the reviewed registration)"
                    .into(),
            );
        }
        let header_read_timeout = match get("ONYX_HEADER_READ_TIMEOUT_SECS") {
            None => Duration::from_secs(10),
            Some(s) => Duration::from_secs(
                s.trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (1..=120).contains(n))
                    .ok_or("ONYX_HEADER_READ_TIMEOUT_SECS must be 1..=120")?,
            ),
        };
        let max_connections = match get("ONYX_MAX_CONNECTIONS") {
            None => 1_024,
            Some(s) => s
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=65_535).contains(n))
                .ok_or("ONYX_MAX_CONNECTIONS must be 1..=65535")?,
        };
        let max_per_client = match get("ONYX_MAX_CONNS_PER_IP") {
            None => crate::serve::DEFAULT_MAX_PER_CLIENT,
            Some(s) => s
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=65_535).contains(n))
                .ok_or("ONYX_MAX_CONNS_PER_IP must be 1..=65535")?,
        };
        if max_per_client > max_connections {
            return Err(format!(
                "ONYX_MAX_CONNS_PER_IP ({max_per_client}) cannot exceed ONYX_MAX_CONNECTIONS ({max_connections})"
            ));
        }
        let max_fee_per_gas = match get("ONYX_MAX_FEE_GWEI") {
            None => onyx_api::evm_tx::DEFAULT_MAX_FEE_PER_GAS,
            Some(s) => {
                s.trim()
                    .parse::<u128>()
                    .ok()
                    .filter(|n| (1..=10_000).contains(n))
                    .ok_or("ONYX_MAX_FEE_GWEI must be 1..=10000")?
                    * 1_000_000_000
            }
        };
        let max_gas = match get("ONYX_MAX_GAS") {
            None => onyx_api::evm_tx::DEFAULT_MAX_GAS,
            Some(s) => s
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|n| (21_000..=30_000_000).contains(n))
                .ok_or("ONYX_MAX_GAS must be 21000..=30000000")?,
        };
        let state_dir: PathBuf = required(&get, "ONYX_STATE_DIR")?.into();
        let bounded = |k: &str, default: u64, lo: u64, hi: u64| -> Result<u64, String> {
            match get(k) {
                None => Ok(default),
                Some(s) => s
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (lo..=hi).contains(n))
                    .ok_or_else(|| format!("{k} must be {lo}..={hi}")),
            }
        };
        let gate_every = Duration::from_secs(bounded("ONYX_GATE_REFRESH_SECS", 60, 1, 86_400)?);
        let indexer = match get("ONYX_INDEXER").as_deref().map(str::trim) {
            None | Some("") | Some("off") => None,
            Some("on") => Some(IndexerSettings {
                span: bounded(
                    "ONYX_INDEX_SPAN",
                    onyx_api::indexer::DEFAULT_SPAN,
                    1,
                    onyx_api::indexer::MAX_SPAN,
                )?,
                overlap: bounded(
                    "ONYX_INDEX_OVERLAP",
                    onyx_api::indexer::DEFAULT_OVERLAP,
                    0,
                    onyx_api::indexer::MAX_SPAN,
                )?,
                every: Duration::from_secs(bounded("ONYX_INDEX_EVERY_SECS", 15, 1, 3_600)?),
                rescan_every: match (
                    get("ONYX_INDEX_RESCAN_POLLS"),
                    get("ONYX_INDEX_RESCAN_SECS"),
                ) {
                    (Some(_), Some(_)) => {
                        return Err(
                            "set ONYX_INDEX_RESCAN_POLLS or ONYX_INDEX_RESCAN_SECS, not both"
                                .into(),
                        )
                    }
                    (None, Some(_)) => {
                        let secs = bounded("ONYX_INDEX_RESCAN_SECS", 0, 0, 31_536_000)?;
                        let every = bounded("ONYX_INDEX_EVERY_SECS", 15, 1, 3_600)?;
                        secs.div_ceil(every)
                    }
                    _ => bounded(
                        "ONYX_INDEX_RESCAN_POLLS",
                        onyx_api::indexer::DEFAULT_RESCAN_EVERY,
                        0,
                        onyx_api::indexer::MAX_RESCAN_EVERY,
                    )?,
                },
                rescan_budget: bounded(
                    "ONYX_INDEX_RESCAN_CALLS",
                    onyx_api::indexer::DEFAULT_RESCAN_BUDGET,
                    1,
                    onyx_api::indexer::MAX_RANGES_PER_POLL,
                )?,
                max_jump: bounded(
                    "ONYX_INDEX_MAX_JUMP",
                    onyx_api::indexer::DEFAULT_MAX_JUMP,
                    1,
                    onyx_api::indexer::MAX_MAX_JUMP,
                )?,
                state_file: get("ONYX_INDEX_STATE")
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
                    .map_or_else(|| state_dir.join("indexer.json"), PathBuf::from),
                health_detail: match get("ONYX_INDEX_HEALTH").as_deref().map(str::trim) {
                    None | Some("") | Some("full") => true,
                    Some("summary") => false,
                    Some(other) => {
                        return Err(format!(
                            "ONYX_INDEX_HEALTH must be full or summary, not {other:?}"
                        ))
                    }
                },
            }),
            Some(other) => return Err(format!("ONYX_INDEXER must be on or off, not {other:?}")),
        };
        // IDX-R6-3: the overlap must read again every block one lying
        // `finalized` head let the scan pass (IDX-R5-5).
        if let Some(s) = &indexer {
            if s.max_jump > s.overlap {
                return Err(format!(
                    "ONYX_INDEX_MAX_JUMP ({}) must be at most ONYX_INDEX_OVERLAP ({}): the overlap reads again the blocks one lying head let the scan pass (IDX-R5-5)",
                    s.max_jump, s.overlap
                ));
            }
        }
        if indexer.is_none() {
            for k in [
                "ONYX_INDEX_SPAN",
                "ONYX_INDEX_OVERLAP",
                "ONYX_INDEX_EVERY_SECS",
                "ONYX_INDEX_RESCAN_POLLS",
                "ONYX_INDEX_RESCAN_SECS",
                "ONYX_INDEX_RESCAN_CALLS",
                "ONYX_INDEX_MAX_JUMP",
                "ONYX_INDEX_STATE",
                "ONYX_INDEX_HEALTH",
            ] {
                if get(k).is_some() {
                    return Err(format!("{k} is set but ONYX_INDEXER is not on"));
                }
            }
        }
        Ok(NodeConfig {
            env,
            listen,
            allow_plain_http_public,
            public_url,
            rpc_url,
            deployment: required(&get, "ONYX_DEPLOYMENT")?.into(),
            catalog: required(&get, "ONYX_CATALOG")?.into(),
            observations,
            observation_templates,
            signed_reports: get("ONYX_SIGNED_REPORTS")
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
            track_publications: get("ONYX_TRACK_PUBLICATIONS")
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
            submissions: get("ONYX_SUBMISSIONS")
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
            salted_labels: get("ONYX_SALTED_LABELS")
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default(),
            cas_dir: required(&get, "ONYX_CAS_DIR")?.into(),
            state_dir,
            payments,
            operator_key_file,
            reporter,
            facilitator_url,
            trusted_proxies,
            allow_test_labels,
            rights,
            allow_mainnet,
            reporter_every,
            prepaid,
            observe,
            gate_every,
            token_domain: (
                get("ONYX_USDC_EIP712_NAME").unwrap_or_else(|| "USDC".into()),
                get("ONYX_USDC_EIP712_VERSION").unwrap_or_else(|| "2".into()),
            ),
            header_read_timeout,
            max_connections,
            max_per_client,
            max_fee_per_gas,
            max_gas,
            indexer,
        })
    }
}

/// Whether `a` can be reached from this machine only: 127.0.0.0/8, `::1`,
/// or an IPv4-mapped loopback address.
fn is_loopback(a: SocketAddr) -> bool {
    a.ip().to_canonical().is_loopback()
}

impl NodeConfig {
    /// The start-up warning for a plain-HTTP listen address that is not
    /// loopback (allowed only with `ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1`, M7-7);
    /// `None` on loopback.
    pub fn plain_http_public_warning(&self) -> Option<String> {
        (!is_loopback(self.listen)).then(|| {
            format!(
                "onyx-node: warning: ONYX_LISTEN {} is not loopback and serves plain HTTP (ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1); keep it unreachable from outside (a container published on 127.0.0.1) and put TLS in front (THREATS.md M7-7)",
                self.listen
            )
        })
    }
}

/// The directory systemd hands this service's credentials in
/// (`$CREDENTIALS_DIRECTORY`, set for units with `LoadCredential=`).
fn credentials_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("CREDENTIALS_DIRECTORY")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
}

/// Owner, group and mode of a file or directory, as `check_private` reads them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Perms {
    mode: u32,
    uid: u32,
    gid: u32,
}

/// The pure rule for a systemd credential (L7-SD-1): what systemd writes for
/// `LoadCredential=` and nothing looser. The file is owned by root:root and
/// may be read by its owner and group only (0440 or tighter, never written by
/// group); the directory is owned by root:root and closed to others. A group
/// other than root (e.g. `/etc/onyx` as root:onyx 0750 holding a root:onyx
/// 0440 key) is refused, so pointing `CREDENTIALS_DIRECTORY` at a directory
/// a group shares (by hand or from an `EnvironmentFile=`) gains nothing.
fn credential_allowed(file: Perms, dir: Perms) -> bool {
    file.uid == 0
        && file.gid == 0
        && file.mode & 0o037 == 0
        && dir.uid == 0
        && dir.gid == 0
        && dir.mode & 0o007 == 0
}

/// The file's and its directory's `Perms` when `path` is a regular file (not
/// a symlink, which could point anywhere) whose own parent is `cred_dir`.
#[cfg(unix)]
fn credential_perms(path: &std::path::Path, cred_dir: &std::path::Path) -> Option<(Perms, Perms)> {
    use std::os::unix::fs::MetadataExt;
    let perms = |m: &std::fs::Metadata| Perms {
        mode: m.mode() & 0o7777,
        uid: m.uid(),
        gid: m.gid(),
    };
    let file = std::fs::symlink_metadata(path).ok()?;
    if !file.file_type().is_file() {
        return None;
    }
    let dir = cred_dir.canonicalize().ok()?;
    let parent = path.parent()?.canonicalize().ok()?;
    if parent != dir {
        return None;
    }
    let dir_meta = std::fs::metadata(&dir).ok()?;
    if !dir_meta.is_dir() {
        return None;
    }
    Some((perms(&file), perms(&dir_meta)))
}

/// Refuse a secret file that group or others can read (Unix): `chmod 600`.
/// One exception (L7-SD-1): systemd writes each `LoadCredential=` file as
/// root:root mode 0440 (with an ACL for the service's user) in a per-service
/// root:root directory others cannot enter; the 0600 rule refused every
/// credential, so the shipped unit could not start with its operator key or
/// `rpc-url`. A regular file directly in `cred_dir` that matches exactly
/// that (`credential_allowed`: file and directory root:root, file group-read
/// at most, directory closed to others) is accepted; a symlink, a non-root
/// owner or group, or an open directory falls back to the 0600 rule.
fn check_private(
    path: &std::path::Path,
    what: &str,
    cred_dir: Option<&std::path::Path>,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map_err(|e| format!("{what} {}: {e}", path.display()))?
            .permissions()
            .mode();
        let systemd_credential = cred_dir
            .and_then(|d| credential_perms(path, d))
            .is_some_and(|(file, dir)| credential_allowed(file, dir));
        if !systemd_credential && mode & 0o077 != 0 {
            return Err(format!(
                "{what} {} must not be readable by group or others (mode {:o}); chmod 600",
                path.display(),
                mode & 0o777
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = (path, what, cred_dir);
    Ok(())
}

/// Read a one-line secret (a node URL with an API key) from `path`. Refuses
/// a file group or others can read (Unix). Errors never contain the
/// file's content.
pub fn read_secret_file(path: &std::path::Path, what: &str) -> Result<String, String> {
    check_private(path, what, credentials_dir().as_deref())?;
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("{what} {}: {e}", path.display()))?;
    let v = text.trim();
    if v.is_empty() || v.contains(['\n', '\r']) {
        return Err(format!("{what} {} must hold one line", path.display()));
    }
    Ok(v.to_string())
}

/// Read the operator key from `path`: 64 hex digits (an optional `0x` and
/// surrounding whitespace allowed). Refuses a file other users can read or
/// write (Unix), so a key is never left world-readable. Errors never contain
/// the file's content.
pub fn read_key_file(path: &std::path::Path) -> Result<[u8; 32], String> {
    check_private(path, "operator key file", credentials_dir().as_deref())?;
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("operator key file {}: {e}", path.display()))?;
    let t = text.trim();
    let t = t.strip_prefix("0x").unwrap_or(t);
    let bytes = hex::decode(t)
        .ok()
        .filter(|b| b.len() == 32)
        .ok_or_else(|| {
            format!(
                "operator key file {} must hold 64 hex digits",
                path.display()
            )
        })?;
    let mut k = [0u8; 32];
    k.copy_from_slice(&bytes);
    if k == [0u8; 32] {
        return Err(format!(
            "operator key file {} holds a zero key",
            path.display()
        ));
    }
    Ok(k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn base() -> BTreeMap<&'static str, &'static str> {
        BTreeMap::from([
            ("ONYX_ENV", "testnet"),
            ("ONYX_PUBLIC_URL", "https://api.example.test/"),
            ("ONYX_RPC_URL", "https://sepolia.base.org"),
            ("ONYX_DEPLOYMENT", "/etc/onyx/deployment.json"),
            ("ONYX_CATALOG", "/etc/onyx/catalog.json"),
            ("ONYX_CAS_DIR", "/var/lib/onyx/cas"),
            ("ONYX_STATE_DIR", "/var/lib/onyx/state"),
            ("ONYX_RIGHTS", "cleared"),
        ])
    }

    fn load(m: &BTreeMap<&str, &str>) -> Result<NodeConfig, String> {
        NodeConfig::from_env(|k| m.get(k).map(|v| v.to_string()), false)
    }

    #[test]
    fn a_minimal_testnet_config_is_free_and_strict() {
        let c = load(&base()).expect("config");
        assert_eq!(c.env, Env::Testnet);
        assert_eq!(c.payments, PaymentMode::Off);
        assert_eq!(c.public_url, "https://api.example.test");
        assert!(!c.allow_test_labels && !c.allow_mainnet);
        assert_eq!(c.listen, "127.0.0.1:8080".parse().expect("addr"));
        assert!(c.facilitator_url.is_none());
    }

    /// IDX-R6-3: `ONYX_INDEX_MAX_JUMP` past `ONYX_INDEX_OVERLAP` is refused,
    /// so the overlap always reads again what one lying head let the scan
    /// pass (IDX-R5-5); equal is fine.
    #[test]
    fn idx_r6_3_the_jump_is_at_most_the_overlap() {
        let mut m = base();
        m.insert("ONYX_INDEXER", "on");
        m.insert("ONYX_INDEX_OVERLAP", "50");
        let e = load(&m).expect_err("default jump 100 > overlap 50");
        assert!(
            e.contains("ONYX_INDEX_MAX_JUMP (100) must be at most ONYX_INDEX_OVERLAP (50)"),
            "{e}"
        );
        m.insert("ONYX_INDEX_MAX_JUMP", "50");
        let c = load(&m).expect("equal").indexer.expect("on");
        assert_eq!((c.max_jump, c.overlap), (50, 50));
        m.insert("ONYX_INDEX_MAX_JUMP", "51");
        assert!(load(&m).is_err());
    }

    /// ADR-0033: the indexer is off unless asked for; its settings are
    /// bounded and refused when it is off.
    #[test]
    fn the_indexer_is_opt_in_and_bounded() {
        assert!(load(&base()).expect("config").indexer.is_none());
        let mut m = base();
        m.insert("ONYX_INDEX_SPAN", "100");
        assert!(load(&m).expect_err("off").contains("ONYX_INDEXER"));
        m.insert("ONYX_INDEXER", "on");
        let c = load(&m).expect("config").indexer.expect("on");
        assert_eq!(c.span, 100);
        assert_eq!(c.overlap, onyx_api::indexer::DEFAULT_OVERLAP);
        assert_eq!(c.every, Duration::from_secs(15));
        assert!(c.health_detail, "full health detail by default");
        assert_eq!(c.rescan_every, onyx_api::indexer::DEFAULT_RESCAN_EVERY);
        assert_eq!(c.rescan_budget, onyx_api::indexer::DEFAULT_RESCAN_BUDGET);
        assert_eq!(c.max_jump, onyx_api::indexer::DEFAULT_MAX_JUMP);
        // IDX-R5-4: the interval as a time, in whole polls.
        let mut daily = m.clone();
        daily.insert("ONYX_INDEX_RESCAN_SECS", "86400");
        daily.insert("ONYX_INDEX_EVERY_SECS", "7");
        assert_eq!(
            load(&daily)
                .expect("secs")
                .indexer
                .expect("on")
                .rescan_every,
            12_343
        );
        daily.insert("ONYX_INDEX_RESCAN_POLLS", "10");
        assert!(load(&daily).expect_err("both").contains("not both"));
        let mut tuned = m.clone();
        tuned.insert("ONYX_INDEX_RESCAN_CALLS", "20");
        tuned.insert("ONYX_INDEX_MAX_JUMP", "50");
        let t = load(&tuned).expect("tuned").indexer.expect("on");
        assert_eq!((t.rescan_budget, t.max_jump), (20, 50));
        let mut rescan = m.clone();
        rescan.insert("ONYX_INDEX_RESCAN_POLLS", "0");
        assert_eq!(
            load(&rescan)
                .expect("rescan")
                .indexer
                .expect("on")
                .rescan_every,
            0
        );
        let mut summary = m.clone();
        summary.insert("ONYX_INDEX_HEALTH", "summary");
        assert!(
            !load(&summary)
                .expect("summary")
                .indexer
                .expect("on")
                .health_detail
        );
        assert_eq!(
            c.state_file,
            PathBuf::from("/var/lib/onyx/state/indexer.json")
        );
        for (k, v) in [
            ("ONYX_INDEX_SPAN", "0"),
            ("ONYX_INDEX_SPAN", "10001"),
            ("ONYX_INDEX_OVERLAP", "-1"),
            ("ONYX_INDEX_EVERY_SECS", "0"),
            ("ONYX_INDEX_RESCAN_POLLS", "1000001"),
            ("ONYX_INDEX_RESCAN_SECS", "31536001"),
            ("ONYX_INDEX_RESCAN_CALLS", "0"),
            ("ONYX_INDEX_RESCAN_CALLS", "2001"),
            ("ONYX_INDEX_MAX_JUMP", "0"),
            // IDX-R6-3: a jump past the overlap (the default jump is 100).
            ("ONYX_INDEX_MAX_JUMP", "101"),
            ("ONYX_INDEX_OVERLAP", "99"),
            ("ONYX_INDEX_OVERLAP", "0"),
            ("ONYX_INDEXER", "yes"),
            ("ONYX_INDEX_HEALTH", "none"),
        ] {
            let mut bad = m.clone();
            bad.insert(k, v);
            assert!(load(&bad).is_err(), "{k}={v}");
        }
        m.insert("ONYX_INDEX_STATE", "/srv/idx.json");
        assert_eq!(
            load(&m).expect("config").indexer.expect("on").state_file,
            PathBuf::from("/srv/idx.json")
        );
    }

    #[test]
    fn level_two_stores_are_optional_and_comma_separated() {
        assert!(load(&base()).expect("config").observations.is_empty());
        let mut m = base();
        m.insert(
            "ONYX_OBSERVATIONS",
            " /etc/onyx/debt.json, ,/etc/onyx/other.json ",
        );
        // L2API-R4-F1: stores need pinned templates.
        assert!(load(&m)
            .expect_err("no pins")
            .contains("ONYX_OBSERVATION_TEMPLATES"));
        let id = format!("0x{}", "ab".repeat(32));
        m.insert(
            "ONYX_OBSERVATION_TEMPLATES",
            Box::leak(format!("a.b={id},a.b={id}").into_boxed_str()),
        );
        assert!(load(&m).expect_err("twice").contains("twice"));
        m.insert("ONYX_OBSERVATION_TEMPLATES", "a.b=0x12");
        assert!(load(&m).is_err(), "short id");
        m.insert(
            "ONYX_OBSERVATION_TEMPLATES",
            Box::leak(format!(" us.debt = {id} ").into_boxed_str()),
        );
        let c = load(&m).expect("config");
        assert_eq!(
            c.observation_templates.get("us.debt").map(|h| h.0),
            Some([0xab; 32])
        );
        assert_eq!(
            c.observations,
            vec![
                PathBuf::from("/etc/onyx/debt.json"),
                PathBuf::from("/etc/onyx/other.json")
            ]
        );
    }

    #[test]
    fn fee_gas_and_per_client_limits_are_settable_and_bounded() {
        // AUD-1R-F1/F3: defaults, overrides, and out-of-range values refused.
        let c = load(&base()).expect("config");
        assert_eq!(c.max_per_client, crate::serve::DEFAULT_MAX_PER_CLIENT);
        assert_eq!(c.max_fee_per_gas, onyx_api::evm_tx::DEFAULT_MAX_FEE_PER_GAS);
        assert_eq!(c.max_gas, onyx_api::evm_tx::DEFAULT_MAX_GAS);
        let mut m = base();
        m.insert("ONYX_MAX_CONNS_PER_IP", "8");
        m.insert("ONYX_MAX_FEE_GWEI", "50");
        m.insert("ONYX_MAX_GAS", "2000000");
        let c = load(&m).expect("config");
        assert_eq!(c.max_per_client, 8);
        assert_eq!(c.max_fee_per_gas, 50_000_000_000);
        assert_eq!(c.max_gas, 2_000_000);
        for (k, v) in [
            ("ONYX_MAX_CONNS_PER_IP", "0"),
            ("ONYX_MAX_FEE_GWEI", "0"),
            ("ONYX_MAX_FEE_GWEI", "10001"),
            ("ONYX_MAX_FEE_GWEI", "1.5"),
            ("ONYX_MAX_GAS", "20999"),
            ("ONYX_MAX_GAS", "30000001"),
        ] {
            let mut m = base();
            m.insert(k, v);
            assert!(load(&m).expect_err(v).contains(k), "{k}={v}");
        }
    }

    /// M7-7 (ROADMAP L11): the server speaks plain HTTP, so a listen address
    /// other machines can reach is refused unless the operator says, with
    /// `ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1`, that something else keeps it off
    /// the open network (a container published on 127.0.0.1, a testnet lab).
    /// Loopback (127.0.0.0/8, ::1, and ::ffff:127.x) needs nothing.
    #[test]
    fn m7_7_a_public_listen_address_needs_an_explicit_opt_in() {
        for addr in [
            "0.0.0.0:8080",
            "[::]:8080",
            "192.0.2.10:8080",
            "[2001:db8::1]:8080",
        ] {
            let mut m = base();
            m.insert("ONYX_LISTEN", addr);
            let e = load(&m).expect_err(addr);
            assert!(
                e.contains("ONYX_LISTEN")
                    && e.contains("plain HTTP")
                    && e.contains("TLS")
                    && e.contains("ONYX_ALLOW_PLAIN_HTTP_PUBLIC=1"),
                "{addr}: {e}"
            );
            for off in ["0", ""] {
                m.insert("ONYX_ALLOW_PLAIN_HTTP_PUBLIC", off);
                assert!(load(&m).is_err(), "{addr} with {off:?}");
            }
            m.insert("ONYX_ALLOW_PLAIN_HTTP_PUBLIC", "1");
            let c = load(&m).expect(addr);
            let w = c.plain_http_public_warning().expect("a warning at start");
            assert!(w.contains(addr) && w.contains("TLS"), "{w}");
        }
        for addr in [
            "127.0.0.1:8080",
            "127.0.0.2:9000",
            "[::1]:8080",
            "[::ffff:127.0.0.1]:8080",
        ] {
            let mut m = base();
            m.insert("ONYX_LISTEN", addr);
            let c = load(&m).expect(addr);
            assert!(c.plain_http_public_warning().is_none(), "{addr}");
            // The opt-in changes nothing on loopback.
            m.insert("ONYX_ALLOW_PLAIN_HTTP_PUBLIC", "1");
            assert!(load(&m).expect(addr).plain_http_public_warning().is_none());
        }
        for bad in ["yes", "true", "2", " 1"] {
            let mut m = base();
            m.insert("ONYX_ALLOW_PLAIN_HTTP_PUBLIC", bad);
            assert!(
                load(&m)
                    .expect_err(bad)
                    .contains("ONYX_ALLOW_PLAIN_HTTP_PUBLIC must be 0 or 1"),
                "{bad:?}"
            );
        }
    }

    /// AUD-2-F8: a per-client cap above the server's cap, and a /0 trusted
    /// proxy range (which would trust every peer), are refused.
    #[test]
    fn per_client_cap_and_trusted_proxy_ranges_are_checked() {
        let mut m = base();
        m.insert("ONYX_MAX_CONNECTIONS", "64");
        m.insert("ONYX_MAX_CONNS_PER_IP", "65");
        assert!(load(&m)
            .expect_err("per ip > total")
            .contains("ONYX_MAX_CONNS_PER_IP"));
        m.insert("ONYX_MAX_CONNS_PER_IP", "64");
        assert!(load(&m).is_ok());
        for r in ["0.0.0.0/0", "::/0", "10.0.0.0/8,::/0"] {
            let mut m = base();
            m.insert("ONYX_TRUSTED_PROXIES", r);
            assert!(load(&m).expect_err(r).contains("/0"), "{r}");
        }
        let mut m = base();
        m.insert("ONYX_TRUSTED_PROXIES", "10.0.0.0/8, 2001:db8::/32");
        assert_eq!(load(&m).expect("ranges").trusted_proxies.len(), 2);
    }

    #[test]
    fn no_mock_or_dev_facilitator_can_be_selected() {
        for mode in ["mock", "dev", "local", "Mock", ""] {
            let mut m = base();
            m.insert("ONYX_PAYMENTS", mode);
            let e = load(&m).expect_err(mode);
            assert!(e.contains("ONYX_PAYMENTS"), "{mode}: {e}");
        }
        // Facilitator mode: an https facilitator only (or loopback http).
        let mut m = base();
        m.insert("ONYX_PAYMENTS", "facilitator");
        m.insert("ONYX_REPORTER", "off");
        let c = load(&m).expect("default facilitator");
        assert!(c
            .facilitator_url
            .as_deref()
            .is_some_and(|u| u.starts_with("https://")));
        m.insert("ONYX_X402_FACILITATOR", "http://f.example");
        assert!(load(&m).is_err(), "plain http to a remote facilitator");
    }

    /// OPS-L1-F3: a loopback http facilitator decides what is paid; it is a
    /// dev-box tool, refused on testnet.
    #[test]
    fn a_loopback_http_facilitator_is_dev_only() {
        let mut m = base();
        m.insert("ONYX_PAYMENTS", "facilitator");
        m.insert("ONYX_REPORTER", "off");
        m.insert("ONYX_X402_FACILITATOR", "http://127.0.0.1:9");
        let e = load(&m).expect_err("testnet");
        assert!(e.contains("dev only"), "{e}");
        m.insert("ONYX_ENV", "dev");
        assert!(load(&m).is_ok(), "dev may use a local facilitator");
    }

    /// OPS-L1-F5: facilitator mode without a key would record receipts that
    /// ROADMAP S7: ONYX_PREPAID is one address, and only where the reporter sends.
    #[test]
    fn prepaid_needs_an_address_and_the_facilitator_reporter() {
        let mut m = base();
        m.insert("ONYX_PREPAID", "0x1111111111111111111111111111111111111111");
        assert!(load(&m)
            .expect_err("payments off")
            .contains("ONYX_PREPAID needs"));
        m.insert("ONYX_PAYMENTS", "facilitator");
        m.insert("ONYX_REPORTER", "off");
        assert!(load(&m)
            .expect_err("reporter off")
            .contains("ONYX_PREPAID needs"));
        m.insert("ONYX_REPORTER", "on");
        m.insert(
            "ONYX_OPERATOR_KEY_FILE",
            "/run/credentials/onyx/operator-key",
        );
        assert_eq!(load(&m).expect("ok").prepaid, Some([0x11; 20]));
        for bad in [
            "1111111111111111111111111111111111111111",
            "0x11",
            "0xzz11111111111111111111111111111111111111",
        ] {
            m.insert("ONYX_PREPAID", bad);
            assert!(load(&m).expect_err(bad).contains("0x address"), "{bad}");
        }
        m.insert("ONYX_PREPAID", "");
        assert_eq!(load(&m).expect("unset").prepaid, None);
    }

    /// ADR-0049: ONYX_OBSERVE only with prepaid holds.
    #[test]
    fn observe_needs_prepaid() {
        let mut m = base();
        m.insert("ONYX_OBSERVE", "/etc/onyx/observe.json");
        assert!(load(&m)
            .expect_err("no prepaid")
            .contains("ONYX_OBSERVE needs ONYX_PREPAID"));
        m.insert("ONYX_PAYMENTS", "facilitator");
        m.insert(
            "ONYX_OPERATOR_KEY_FILE",
            "/run/credentials/onyx/operator-key",
        );
        m.insert("ONYX_PREPAID", "0x1111111111111111111111111111111111111111");
        assert_eq!(
            load(&m).expect("ok").observe,
            Some(PathBuf::from("/etc/onyx/observe.json"))
        );
    }

    /// nobody posts; that needs an explicit ONYX_REPORTER=off.
    #[test]
    fn facilitator_mode_needs_a_key_or_an_explicit_reporter_off() {
        let mut m = base();
        m.insert("ONYX_PAYMENTS", "facilitator");
        let e = load(&m).expect_err("no key, reporter on");
        assert!(e.contains("ONYX_REPORTER=off"), "{e}");
        m.insert("ONYX_REPORTER", "off");
        assert!(!load(&m).expect("explicit off").reporter);
        m.insert("ONYX_REPORTER", "maybe");
        assert!(load(&m).is_err());
        m.insert("ONYX_REPORTER", "on");
        m.insert(
            "ONYX_OPERATOR_KEY_FILE",
            "/run/credentials/onyx/operator-key",
        );
        assert!(load(&m).expect("key").reporter);
    }

    #[test]
    fn mainnet_and_unknown_environments_are_refused() {
        for env in ["mainnet", "prod", "production", ""] {
            let mut m = base();
            m.insert("ONYX_ENV", env);
            assert!(load(&m).is_err(), "{env}");
        }
    }

    #[test]
    fn urls_are_https_or_loopback() {
        let mut m = base();
        m.insert("ONYX_RPC_URL", "http://node.example:8545");
        assert!(load(&m).is_err());
        m.insert("ONYX_RPC_URL", "http://127.0.0.1:8545");
        assert!(load(&m).is_ok());
        m.insert("ONYX_PUBLIC_URL", "http://api.example.test");
        assert!(load(&m).is_err());
    }

    #[test]
    fn self_settle_needs_a_key_file_and_test_labels_need_an_explicit_switch() {
        let mut m = base();
        m.insert("ONYX_PAYMENTS", "self-settle");
        assert!(load(&m).is_err());
        m.insert(
            "ONYX_OPERATOR_KEY_FILE",
            "/run/credentials/onyx/operator-key",
        );
        assert!(load(&m).is_ok());
        m.insert("ONYX_ALLOW_TEST_LABELS", "yes");
        assert!(load(&m).is_err());
        m.insert("ONYX_ALLOW_TEST_LABELS", "1");
        assert!(load(&m).expect("config").allow_test_labels);
    }

    #[test]
    fn rights_take_only_cleared_or_fallback_and_a_testnet_node_must_name_one() {
        use onyx_api::catalog::LabelRights;
        let mut m = base();
        // Review L14-R1: a testnet node (the one that can be public) with the
        // switch unset or empty fails closed; it never defaults to cleared.
        for unset in [None, Some("")] {
            match unset {
                None => m.remove("ONYX_RIGHTS"),
                Some(v) => m.insert("ONYX_RIGHTS", v),
            };
            let e = load(&m).expect_err("testnet without ONYX_RIGHTS");
            assert!(
                e.contains("ONYX_RIGHTS is required with ONYX_ENV=testnet"),
                "{e}"
            );
        }
        // A dev node (local chain) keeps today's default.
        m.insert("ONYX_ENV", "dev");
        m.insert("ONYX_RPC_URL", "http://127.0.0.1:8545");
        m.remove("ONYX_RIGHTS");
        assert_eq!(load(&m).expect("dev config").rights, LabelRights::Cleared);
        m.insert("ONYX_RIGHTS", "");
        assert_eq!(load(&m).expect("dev config").rights, LabelRights::Cleared);
        let mut m = base();
        m.insert("ONYX_RIGHTS", "cleared");
        assert_eq!(load(&m).expect("config").rights, LabelRights::Cleared);
        m.insert("ONYX_RIGHTS", "fallback");
        assert_eq!(load(&m).expect("config").rights, LabelRights::Fallback);
        for bad in ["Fallback", "FALLBACK", "true", "1", "off", "fallback "] {
            m.insert("ONYX_RIGHTS", bad);
            let e = load(&m).expect_err(bad);
            assert!(e.contains("cleared or fallback"), "{bad:?}: {e}");
        }
    }

    #[test]
    fn url_host_drops_paths_queries_and_credentials() {
        assert_eq!(
            url_host("https://base-sepolia.g.alchemy.com/v2/SECRETKEY"),
            "base-sepolia.g.alchemy.com"
        );
        assert_eq!(
            url_host("https://user:pw@node.example:8545/?key=x"),
            "node.example:8545"
        );
        // OPS-L2-F4: not a URL at all (a bare key): never echoed.
        assert!(!url_host("SECRETAPIKEY123").contains("SECRET"));
        assert!(!url_host("n.example/v2/SECRETPATH").contains("SECRET"));
    }

    /// OPS-L1-F4: the node URL can come from a private file (a credential),
    /// since a provider URL often carries an API key.
    #[cfg(unix)]
    #[test]
    fn the_rpc_url_can_come_from_a_private_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("onyx-node-rpc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let p = dir.join("rpc");
        std::fs::write(&p, "https://node.example/v2/SECRETKEY\n").expect("write");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let path = p.display().to_string();
        let mut m: BTreeMap<&str, String> = base()
            .into_iter()
            .map(|(k, v)| (k, v.to_string()))
            .collect();
        m.remove("ONYX_RPC_URL");
        m.insert("ONYX_RPC_URL_FILE", path.clone());
        let get = |m: &BTreeMap<&str, String>| {
            let m = m.clone();
            NodeConfig::from_env(move |k| m.get(k).cloned(), false)
        };
        let e = get(&m).expect_err("world readable");
        assert!(e.contains("chmod 600") && !e.contains("SECRETKEY"), "{e}");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        assert_eq!(
            get(&m).expect("config").rpc_url,
            "https://node.example/v2/SECRETKEY"
        );
        m.insert("ONYX_RPC_URL", "https://sepolia.base.org".into());
        assert!(get(&m).expect_err("both").contains("not both"));
        m.remove("ONYX_RPC_URL");
        std::fs::write(&p, "http://node.example/v2/SECRETKEY").expect("write");
        let e = get(&m).expect_err("plain http remote");
        assert!(!e.contains("SECRETKEY"), "errors never echo the URL: {e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn key_files_must_be_private_and_well_formed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("onyx-node-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let p = dir.join("k");
        std::fs::write(&p, format!("0x{}\n", "11".repeat(32))).expect("write");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(read_key_file(&p)
            .expect_err("world readable")
            .contains("chmod 600"));
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        assert_eq!(read_key_file(&p).expect("key"), [0x11; 32]);
        std::fs::write(&p, "not hex").expect("write");
        let e = read_key_file(&p).expect_err("bad key");
        assert!(!e.contains("not hex"), "errors never echo the file: {e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L7-SD-1: the pure rule accepts exactly what systemd writes for
    /// `LoadCredential=` (file root:root 0440 or tighter, directory root:root
    /// closed to others) and refuses every looser owner, group or mode.
    #[test]
    fn credential_rule_accepts_only_root_owned_files_in_a_root_owned_closed_directory() {
        let p = |mode, uid, gid| Perms { mode, uid, gid };
        let systemd_dir = p(0o550, 0, 0);
        assert!(credential_allowed(p(0o440, 0, 0), systemd_dir));
        assert!(credential_allowed(p(0o400, 0, 0), p(0o500, 0, 0)));
        assert!(credential_allowed(p(0o600, 0, 0), p(0o700, 0, 0)));
        // The review's hole: /etc/onyx root:onyx 0750 with a root:onyx 0440 key.
        assert!(!credential_allowed(p(0o440, 0, 998), p(0o750, 0, 998)));
        // A 0750 directory with a non-root group, even with a root:root file.
        assert!(!credential_allowed(p(0o440, 0, 0), p(0o750, 0, 998)));
        // A file with a non-root owner, or a non-root group.
        assert!(!credential_allowed(p(0o440, 1000, 0), systemd_dir));
        assert!(!credential_allowed(p(0o440, 0, 1000), systemd_dir));
        // A directory not owned by root.
        assert!(!credential_allowed(p(0o440, 0, 0), p(0o550, 1000, 0)));
        // Others may never read, group may never write, the directory never open.
        assert!(!credential_allowed(p(0o444, 0, 0), systemd_dir));
        assert!(!credential_allowed(p(0o460, 0, 0), systemd_dir));
        assert!(!credential_allowed(p(0o440, 0, 0), p(0o551, 0, 0)));
        assert!(!credential_allowed(p(0o440, 0, 0), p(0o1777, 0, 0)));
    }

    /// L7-SD-1 on the file system (tests do not run as root, so here every
    /// group-readable file is refused; the root case is the pure rule above):
    /// only a regular file directly in the directory is a candidate, never a
    /// symlink in it that points elsewhere, and 0600 passes anywhere.
    #[cfg(unix)]
    #[test]
    fn credential_candidates_are_regular_files_directly_in_the_directory() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path, m: u32| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).expect("chmod")
        };
        let root = std::env::temp_dir().join(format!("onyx-node-cred-{}", std::process::id()));
        let creds = root.join("credentials");
        let other = root.join("other");
        std::fs::create_dir_all(&creds).expect("dir");
        std::fs::create_dir_all(&other).expect("dir");
        let key = creds.join("operator-key");
        std::fs::write(&key, "11".repeat(32)).expect("write");
        mode(&key, 0o440);
        let elsewhere = other.join("operator-key");
        std::fs::write(&elsewhere, "11".repeat(32)).expect("write");
        mode(&elsewhere, 0o440);
        let link = creds.join("linked-key");
        std::os::unix::fs::symlink(&elsewhere, &link).expect("symlink");
        mode(&creds, 0o550);
        // A regular file in the directory is a candidate (its real owner is
        // read, not assumed); a symlink in it pointing outside is not, nor is
        // a file elsewhere.
        let (f, d) = credential_perms(&key, &creds).expect("candidate");
        assert_eq!(f.mode & 0o777, 0o440);
        assert_eq!(d.mode & 0o777, 0o550);
        assert!(credential_perms(&link, &creds).is_none());
        assert!(credential_perms(&elsewhere, &creds).is_none());
        // Not root-owned here, so the 0440 file, the symlink and the outside
        // file are all refused, with or without the directory.
        let c = Some(creds.as_path());
        if running_as_root() {
            eprintln!("running as root: the not-root refusals below are skipped");
        } else {
            assert!(check_private(&key, "k", c).is_err());
            assert!(check_private(&link, "k", c).is_err());
        }
        assert!(check_private(&key, "k", None).is_err());
        assert!(check_private(&elsewhere, "k", c).is_err());
        // 0600 passes anywhere, as before.
        mode(&elsewhere, 0o600);
        check_private(&elsewhere, "k", None).expect("0600");
        mode(&creds, 0o700);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Whether the test runs as root (then files it writes are root-owned).
    #[cfg(unix)]
    fn running_as_root() -> bool {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata("/proc/self")
            .map(|m| m.uid() == 0)
            .unwrap_or(false)
    }
}
