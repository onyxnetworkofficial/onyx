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
    /// `ONYX_BULK_DIR` (CL5, ADR-0059): publishers' large datasets (manifests,
    /// tiers file, hot shards) and the `/v1/bulk` routes. Needs the
    /// manifest's `signedBook` (manifests are anchored there).
    pub bulk: Option<BulkConfig>,
    /// `ONYX_STORAGE_*` (ADR-0060): storage-node mode. Needs `ONYX_BULK_DIR`
    /// (the shards are held and served by the bulk store).
    pub storage: Option<StorageConfig>,
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
    /// `ONYX_RELAY_KEY_FILE` (CL6, ADR-0056): the relayer's own key (0600 file of 64 hex
    /// digits). With it, `POST /v1/relay` sends self-signed actions paid from credits. Needs
    /// `ONYX_PREPAID` naming an `OnyxPrepaidV2` whose `relayer()` is this key's address; the key
    /// must not be the operator key (checked again against the chain's roles at start).
    pub relay_key_file: Option<PathBuf>,
    /// `ONYX_RELAY_FEES` (CL6): the fixed fee of each relayed action, `name=fee,...` in the
    /// asset's smallest unit (default [`DEFAULT_RELAY_FEES`]).
    pub relay_fees: String,
    /// `ONYX_CREDITS_FROM_BLOCK` (CL6): the block `OnyxPrepaidV2` was deployed at; the credits
    /// routes index its `Credited` events from there (default: the manifest's deploy block).
    pub credits_from_block: Option<u64>,
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
    /// `ONYX_WRITE_STALL_SECS` (default 30, 1..=600): a connection whose
    /// answer cannot move a byte for this long (its reader stopped reading)
    /// is closed (THREATS SERVE-WRITE-1).
    pub write_stall_timeout: Duration,
    /// `ONYX_MAX_CONNECTIONS` (default 1024, 1..=65535): open connections;
    /// when all are taken, a new one evicts the oldest connection that has
    /// not sent a request, else it is closed (AUD-1-F5, AUD-1R-F1).
    pub max_connections: usize,
    /// `ONYX_MAX_CONNS_PER_IP` (default 32, 1..=65535): open connections per
    /// client (an IPv4 address or an IPv6 /64); trusted proxies are exempt
    /// (AUD-1R-F1).
    pub max_per_client: usize,
    /// `ONYX_READS_PER_MINUTE` (default
    /// [`onyx_api::DEFAULT_READS_PER_MINUTE`], 0..=1000000; 0 is off):
    /// requests per client per minute on every API route (THREATS NL20-1);
    /// 429 `rate_limited` beyond.
    pub reads_per_minute: u32,
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
        let relay_key_file = get("ONYX_RELAY_KEY_FILE")
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        if relay_key_file.is_some() && prepaid.is_none() {
            return Err(
                "ONYX_RELAY_KEY_FILE needs ONYX_PREPAID: relays are paid from prepaid credits"
                    .into(),
            );
        }
        if relay_key_file.is_some() && relay_key_file == operator_key_file {
            return Err("ONYX_RELAY_KEY_FILE must be the relayer's own key, not the operator key (ADR-0056 item 4)".into());
        }
        let relay_fees = get("ONYX_RELAY_FEES")
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_RELAY_FEES.into());
        onyx_api::credits::FeeSchedule::parse(&relay_fees, MAX_RELAY_FEE)
            .map_err(|e| format!("ONYX_RELAY_FEES: {e}"))?;
        let credits_from_block = match get("ONYX_CREDITS_FROM_BLOCK").as_deref().map(str::trim) {
            None | Some("") => None,
            Some(v) => Some(v.parse::<u64>().map_err(|_| {
                format!("ONYX_CREDITS_FROM_BLOCK must be a block number, not {v:?}")
            })?),
        };
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
        let write_stall_timeout = match get("ONYX_WRITE_STALL_SECS") {
            None => Duration::from_secs(30),
            Some(s) => Duration::from_secs(
                s.trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (1..=600).contains(n))
                    .ok_or("ONYX_WRITE_STALL_SECS must be 1..=600")?,
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
        let reads_per_minute = match get("ONYX_READS_PER_MINUTE") {
            None => onyx_api::DEFAULT_READS_PER_MINUTE,
            Some(s) => s
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|n| *n <= 1_000_000)
                .ok_or("ONYX_READS_PER_MINUTE must be 0..=1000000 (0 is off)")?,
        };
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
            bulk: BulkConfig::from_env(&get)?,
            storage: StorageConfig::from_env(&get)?,
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
            relay_key_file,
            relay_fees,
            credits_from_block,
            gate_every,
            token_domain: (
                get("ONYX_USDC_EIP712_NAME").unwrap_or_else(|| "USDC".into()),
                get("ONYX_USDC_EIP712_VERSION").unwrap_or_else(|| "2".into()),
            ),
            header_read_timeout,
            write_stall_timeout,
            max_connections,
            max_per_client,
            reads_per_minute,
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

/// Open files the node needs beyond two a connection (a socket, and a shard
/// file while one is streamed): stores, journals, RPC sockets.
pub const FD_RESERVE: u64 = 256;

/// The start-up warning when the open-file limit `soft` (`None`: unknown) is
/// below two a connection plus [`FD_RESERVE`]: past it every new open fails
/// (sockets, shard files, stores) while the cap still admits connections
/// (review SERVE-WRITE-R2 finding 1). The shipped units set
/// `LimitNOFILE=65536`.
pub fn fd_limit_warning(soft: Option<u64>, max_connections: usize) -> Option<String> {
    let need = (max_connections as u64)
        .saturating_mul(2)
        .saturating_add(FD_RESERVE);
    let soft = soft?;
    (soft < need).then(|| {
        format!(
            "onyx-node: warning: the open-file limit ({soft}) is below 2 x ONYX_MAX_CONNECTIONS + {FD_RESERVE} = {need}: each streamed shard holds a socket and a file; raise LimitNOFILE (the shipped units set 65536) or lower ONYX_MAX_CONNECTIONS"
        )
    })
}

/// The process's soft open-file limit, if it can be read.
pub fn fd_soft_limit() -> Option<u64> {
    rustix::process::getrlimit(rustix::process::Resource::Nofile).current
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

/// The relay fees until they are set from measured gas on Base Sepolia (ADR-0056 open question
/// 1), in the asset's smallest unit (for a 6-decimal asset: 0.001, 0.001, 0.003 and 0.002).
pub const DEFAULT_RELAY_FEES: &str =
    "post_commit=1000,post_receipt=1000,post_sealed_reveal=3000,deposit=2000";
/// The highest relay fee a configuration may set (one whole unit of a 6-decimal asset).
pub const MAX_RELAY_FEE: u128 = 1_000_000;

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

/// `ONYX_STORAGE_*` (ADR-0060, docs/ops/config.md "Storage network").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageConfig {
    /// `ONYX_STORAGE_CONTRACT`: the `OnyxStorage` address (turns the mode on).
    pub contract: [u8; 20],
    /// `ONYX_STORAGE_NODE_ID`: this node's id (from `onyx-storage register`).
    pub node: [u8; 32],
    /// `ONYX_STORAGE_KEY_FILE`: the node operator's key (0600 file of 64 hex
    /// digits): answers, seeds and settlements are sent from it.
    pub key_file: PathBuf,
    /// `ONYX_STORAGE_FROM_BLOCK` (default 0): the contract's deploy block,
    /// where log scans start.
    pub from_block: u64,
    /// `ONYX_STORAGE_EVERY_SECS` (default 15): seconds between passes, give
    /// or take a random quarter (`onyx_api::storage::run`).
    pub every_secs: u64,
    /// `ONYX_STORAGE_FETCH` (`on` default, `off`): fetch placed shards.
    pub fetch: bool,
    /// `ONYX_STORAGE_KEEPER` (`off` default, `on`): settle other nodes'
    /// missed answers.
    pub keeper: bool,
    /// `ONYX_STORAGE_FETCHES_PER_TICK` (default 8): repair pace.
    pub fetches_per_tick: usize,
    /// `ONYX_STORAGE_REMOVALS` (`on` default, `log`, `off`): learn of the
    /// publishers' removals (ADR-0059 amendment 4 item 5a slice 2a); `log`
    /// keeps records and logs only, `off` learns nothing.
    pub removals: onyx_api::bulk_serve::RemovalsMode,
    /// `ONYX_STORAGE_CATALOG_NODES`: nodes holding the publishers' catalogs,
    /// asked first (comma separated; https, or http to loopback).
    pub catalog_nodes: Vec<String>,
    /// `ONYX_STORAGE_REMOVALS_EVERY_SECS` (default 3600, 300..=86400):
    /// seconds between learn passes.
    pub removals_every_secs: u64,
}

/// Most `ONYX_STORAGE_CATALOG_NODES` (each is asked about every dataset).
pub const MAX_CATALOG_NODES: usize = 8;

impl StorageConfig {
    fn from_env(get: &impl Fn(&str) -> Option<String>) -> Result<Option<StorageConfig>, String> {
        let keys = [
            "ONYX_STORAGE_NODE_ID",
            "ONYX_STORAGE_KEY_FILE",
            "ONYX_STORAGE_FROM_BLOCK",
            "ONYX_STORAGE_EVERY_SECS",
            "ONYX_STORAGE_FETCH",
            "ONYX_STORAGE_KEEPER",
            "ONYX_STORAGE_FETCHES_PER_TICK",
            "ONYX_STORAGE_REMOVALS",
            "ONYX_STORAGE_CATALOG_NODES",
            "ONYX_STORAGE_REMOVALS_EVERY_SECS",
        ];
        let Some(contract) = get("ONYX_STORAGE_CONTRACT")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        else {
            if let Some(k) = keys.iter().find(|k| get(k).is_some()) {
                return Err(format!("{k} is set but ONYX_STORAGE_CONTRACT is not"));
            }
            return Ok(None);
        };
        if get("ONYX_BULK_DIR").is_none_or(|v| v.trim().is_empty()) {
            return Err("ONYX_STORAGE_CONTRACT needs ONYX_BULK_DIR: a storage node holds and serves its shards in the bulk store".into());
        }
        let hexn = |k: &str, v: &str, n: usize| -> Result<Vec<u8>, String> {
            v.trim()
                .strip_prefix("0x")
                .and_then(|h| hex::decode(h).ok())
                .filter(|b| b.len() == n)
                .ok_or_else(|| format!("{k} must be 0x and {} hex digits", 2 * n))
        };
        let contract: [u8; 20] = hexn("ONYX_STORAGE_CONTRACT", &contract, 20)?
            .try_into()
            .map_err(|_| "ONYX_STORAGE_CONTRACT")?;
        let node: [u8; 32] = hexn(
            "ONYX_STORAGE_NODE_ID",
            &get("ONYX_STORAGE_NODE_ID")
                .ok_or("ONYX_STORAGE_CONTRACT needs ONYX_STORAGE_NODE_ID")?,
            32,
        )?
        .try_into()
        .map_err(|_| "ONYX_STORAGE_NODE_ID")?;
        let key_file = get("ONYX_STORAGE_KEY_FILE")
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .ok_or("ONYX_STORAGE_CONTRACT needs ONYX_STORAGE_KEY_FILE (the node operator's key)")?;
        let num = |k: &str, default: u64, min: u64, max: u64| -> Result<u64, String> {
            match get(k) {
                None => Ok(default),
                Some(v) => v
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (min..=max).contains(n))
                    .ok_or_else(|| format!("{k} must be {min}..={max}")),
            }
        };
        let switch = |k: &str, default: bool| -> Result<bool, String> {
            match get(k).as_deref().map(str::trim) {
                None => Ok(default),
                Some("on") => Ok(true),
                Some("off") => Ok(false),
                Some(v) => Err(format!("{k} must be on or off, not {v:?}")),
            }
        };
        use onyx_api::bulk_serve::RemovalsMode;
        let removals = match get("ONYX_STORAGE_REMOVALS").as_deref().map(str::trim) {
            None | Some("on") => RemovalsMode::On,
            Some("log") => RemovalsMode::Log,
            Some("off") => RemovalsMode::Off,
            Some(v) => {
                return Err(format!(
                    "ONYX_STORAGE_REMOVALS must be on, log or off, not {v:?}"
                ))
            }
        };
        let mut catalog_nodes: Vec<String> = Vec::new();
        for u in get("ONYX_STORAGE_CATALOG_NODES")
            .unwrap_or_default()
            .split(',')
        {
            let u = u.trim().trim_end_matches('/');
            if u.is_empty() {
                continue;
            }
            onyx_api::x402::check_facilitator_url(u).map_err(|_| {
                format!("ONYX_STORAGE_CATALOG_NODES: {u:?} must be https:// (http:// only to localhost, 127.0.0.1 or [::1])")
            })?;
            if !catalog_nodes.iter().any(|x| x == u) {
                catalog_nodes.push(u.to_string());
            }
        }
        if catalog_nodes.len() > MAX_CATALOG_NODES {
            return Err(format!(
                "ONYX_STORAGE_CATALOG_NODES: at most {MAX_CATALOG_NODES} nodes"
            ));
        }
        Ok(Some(StorageConfig {
            contract,
            node,
            key_file,
            from_block: num("ONYX_STORAGE_FROM_BLOCK", 0, 0, u64::MAX >> 1)?,
            every_secs: num("ONYX_STORAGE_EVERY_SECS", 15, 1, 3_600)?,
            fetch: switch("ONYX_STORAGE_FETCH", true)?,
            keeper: switch("ONYX_STORAGE_KEEPER", false)?,
            fetches_per_tick: num("ONYX_STORAGE_FETCHES_PER_TICK", 8, 1, 1_000)? as usize,
            removals,
            catalog_nodes,
            removals_every_secs: num("ONYX_STORAGE_REMOVALS_EVERY_SECS", 3_600, 300, 86_400)?,
        }))
    }
}

/// `ONYX_BULK_*` (CL5, ADR-0059).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BulkConfig {
    /// `ONYX_BULK_DIR`.
    pub dir: PathBuf,
    /// `ONYX_BULK_WARM_DIR` (default `<ONYX_BULK_DIR>/warm`): warm shards.
    pub warm: PathBuf,
    /// `ONYX_BULK_QUOTA_GIB` (default 16), `ONYX_BULK_MAX_DATASETS` (64),
    /// `ONYX_BULK_MAX_GIB` (256), `ONYX_BULK_WARM_AFTER_DAYS` (7),
    /// `ONYX_BULK_FORMAT2` (`off` default, `on`: take format-2 manifests,
    /// ADR-0059 amendment 5 item 8).
    pub limits: onyx_api::bulk_serve::BulkLimits,
    /// `ONYX_BULK_FREE_PER_MINUTE` (default 60): free bulk answers per
    /// client per minute (reads, and storage-network peer fetches, ADR-0060).
    pub free_per_minute: u32,
    /// `ONYX_BULK_ERASE` (`off`, `dry-run` default, `on`),
    /// `ONYX_BULK_ERASE_GRACE_DAYS` (30, at least 30), `ONYX_BULK_ERASE_HOLD`
    /// (publishers or `publisher/dataset`, comma separated) and
    /// `ONYX_BULK_ERASE_MAX_SHARDS` (10,000): erasing removed datasets
    /// (ADR-0059 amendment 4 item 5a). `on` is refused in storage mode.
    pub erase: onyx_api::bulk_serve::EraseConfig,
}

impl BulkConfig {
    fn from_env(get: &impl Fn(&str) -> Option<String>) -> Result<Option<BulkConfig>, String> {
        let keys = [
            "ONYX_BULK_WARM_DIR",
            "ONYX_BULK_QUOTA_GIB",
            "ONYX_BULK_MAX_DATASETS",
            "ONYX_BULK_MAX_GIB",
            "ONYX_BULK_WARM_AFTER_DAYS",
            "ONYX_BULK_FREE_PER_MINUTE",
            "ONYX_BULK_ERASE",
            "ONYX_BULK_ERASE_GRACE_DAYS",
            "ONYX_BULK_ERASE_HOLD",
            "ONYX_BULK_ERASE_MAX_SHARDS",
            "ONYX_BULK_FORMAT2",
        ];
        let Some(dir) = get("ONYX_BULK_DIR")
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
        else {
            if let Some(k) = keys.iter().find(|k| get(k).is_some()) {
                return Err(format!("{k} is set but ONYX_BULK_DIR is not"));
            }
            return Ok(None);
        };
        let num = |k: &str, default: u64, max: u64| -> Result<u64, String> {
            match get(k) {
                None => Ok(default),
                Some(v) => v
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (1..=max).contains(n))
                    .ok_or_else(|| format!("{k} must be 1..={max}")),
            }
        };
        let d = onyx_api::bulk_serve::BulkLimits::default();
        let limits = onyx_api::bulk_serve::BulkLimits {
            quota_bytes: num("ONYX_BULK_QUOTA_GIB", d.quota_bytes >> 30, 1 << 20)? << 30,
            max_datasets: num(
                "ONYX_BULK_MAX_DATASETS",
                d.max_datasets as u64,
                onyx_bulk::catalog::MAX_CATALOG_LEAVES as u64,
            )? as usize,
            max_total_bytes: num("ONYX_BULK_MAX_GIB", d.max_total_bytes >> 30, 1 << 20)? << 30,
            warm_after_secs: num(
                "ONYX_BULK_WARM_AFTER_DAYS",
                d.warm_after_secs / 86_400,
                3_650,
            )? * 86_400,
            catalog_checkpoint: d.catalog_checkpoint,
            // Off until a release that reads format 2 is the rollback
            // target: a node holding a format-2 manifest cannot start an
            // older binary (review BULK-WIDEN-R1 finding 2).
            format2: match get("ONYX_BULK_FORMAT2").as_deref().map(str::trim) {
                None | Some("off") => false,
                Some("on") => true,
                Some(_) => return Err("ONYX_BULK_FORMAT2 must be on or off".into()),
            },
        };
        let warm = get("ONYX_BULK_WARM_DIR")
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .map_or_else(|| dir.join("warm"), PathBuf::from);
        let free_per_minute = num(
            "ONYX_BULK_FREE_PER_MINUTE",
            u64::from(onyx_api::bulk_serve::FREE_PER_MINUTE),
            1_000_000,
        )? as u32;
        let erase = Self::erase_from_env(get, &num)?;
        Ok(Some(BulkConfig {
            dir,
            warm,
            limits,
            free_per_minute,
            erase,
        }))
    }

    fn erase_from_env(
        get: &impl Fn(&str) -> Option<String>,
        num: &impl Fn(&str, u64, u64) -> Result<u64, String>,
    ) -> Result<onyx_api::bulk_serve::EraseConfig, String> {
        use onyx_api::bulk_serve::{EraseConfig, EraseMode, MAX_SHARDS_PER_PASS, MIN_GRACE_DAYS};
        let mode = match get("ONYX_BULK_ERASE").as_deref().map(str::trim) {
            None | Some("dry-run") => EraseMode::DryRun,
            Some("off") => EraseMode::Off,
            Some("on") => EraseMode::On,
            Some(_) => return Err("ONYX_BULK_ERASE must be off, dry-run or on".into()),
        };
        let storage = get("ONYX_STORAGE_CONTRACT").is_some_and(|v| !v.trim().is_empty());
        if mode == EraseMode::On && storage {
            return Err("ONYX_BULK_ERASE=on is refused in storage mode (ONYX_STORAGE_CONTRACT): a storage node's deals are references only slice 2 counts (ADR-0059 amendment 4 item 5a)".into());
        }
        let days = num("ONYX_BULK_ERASE_GRACE_DAYS", MIN_GRACE_DAYS, 3_650)?;
        if days < MIN_GRACE_DAYS {
            return Err(format!(
                "ONYX_BULK_ERASE_GRACE_DAYS must be {MIN_GRACE_DAYS}..=3650"
            ));
        }
        let mut hold = std::collections::BTreeSet::new();
        for h in get("ONYX_BULK_ERASE_HOLD").unwrap_or_default().split(',') {
            let h = h.trim();
            if h.is_empty() {
                continue;
            }
            let ok = match h.split_once('/') {
                None => onyx_core::ids::is_dotted_id(h),
                Some((p, d)) => onyx_core::ids::is_dotted_id(p) && onyx_core::ids::is_dotted_id(d),
            };
            if !ok {
                return Err(format!(
                    "ONYX_BULK_ERASE_HOLD: {h:?} is not a publisher or publisher/dataset id"
                ));
            }
            hold.insert(h.to_string());
        }
        let max_shards = num(
            "ONYX_BULK_ERASE_MAX_SHARDS",
            MAX_SHARDS_PER_PASS as u64,
            1_000_000,
        )? as usize;
        Ok(EraseConfig {
            mode,
            grace_secs: days * 86_400,
            hold,
            max_shards,
        })
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
