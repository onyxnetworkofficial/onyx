//! `onyx-node`: the onyx-api service (docs/ops/deployment.md). The binary
//! is [`main_with`] without serving checks; a sector build passes its
//! gates (`onyx-node-btc` adds `btc.entity-reserve`'s, ADR-0034 Amendment 1).
//!
//! ```text
//! onyx-node                 # serve, configured from ONYX_* variables
//! onyx-node --check-config  # load and check config, manifest, catalog; no network
//! onyx-node healthcheck     # probe GET /v1/health on ONYX_LISTEN (container health check)
//! ```
//!
//! Testnet only: the manifest's chain must be Base Sepolia (or anvil in dev)
//! and the payment network Base Sepolia, unless `--allow-mainnet` is given,
//! which this project never does. The operator key is read from the file in
//! `ONYX_OPERATOR_KEY_FILE` (mode 0600) and never printed.

use crate::catalog_file::CatalogFile;
use crate::config::{read_key_file, url_host, NodeConfig, PaymentMode};
use crate::doctor;
use crate::healthcheck;
use crate::manifest::Deployment;
use crate::shutdown;
use onyx_api::catalog::Catalog;
use onyx_api::client::PayToPin;
use onyx_api::evm_tx::Sender;
use onyx_api::gate::ServingGate;
use onyx_api::indexer::{Holdings, Indexer, IndexerConfig, IndexerHealth};
use onyx_api::intents::IntentResolver;
use onyx_api::journal::Binding;
use onyx_api::prepaid_chain::{ChainAccounts, PrepaidReporter};
use onyx_api::prepaid_gate::PrepaidState;
use onyx_api::reporter::{Reporter, ReporterHealth};
use onyx_api::roots::{ChainRoots, HttpJsonRpc, JsonRpc};
use onyx_api::self_settle::SelfSettle;
use onyx_api::settle_check::ChainSettleCheck;
use onyx_api::x402::{Facilitator, HttpFacilitator, HyperPost, PaymentTerms};
use onyx_api::{router_with, ApiConfig, ApiState, Payments};
use onyx_coordinator::cas::Kind;
use onyx_coordinator::cas::{Cas, FsCas};
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Node calls give up after this.
const RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Tries of each node read the start makes (the settlement's code, the
/// registry floors, level-2 pins, the deploy block, OnyxRevenue's code),
/// SOAK-2: one node error used to stop the start (exit 1), so behind a node
/// failing a quarter of its calls (the L6 soak's fault proxy) 59 starts in a
/// row failed. A failure that stays (a wrong code hash, the wrong chain)
/// still stops the start, after these tries.
pub const STARTUP_TRIES: u32 = 8;
/// The wait after the first failed try; it doubles up to [`STARTUP_MAX_WAIT`]
/// (0.5 + 1 + 2 + 4 + 8 + 8 + 8 = 31.5 s over the 8 tries).
pub const STARTUP_FIRST_WAIT: Duration = Duration::from_millis(500);
/// The longest wait between two tries.
pub const STARTUP_MAX_WAIT: Duration = Duration::from_secs(8);

/// Run one start-up step, trying again after a failure (SOAK-2): at most
/// `tries` calls, waiting `first_wait`, then twice as long each time (at most
/// [`STARTUP_MAX_WAIT`]). Returns the first success or the last error.
pub async fn startup_step<T, F, Fut>(
    what: &str,
    tries: u32,
    first_wait: Duration,
    mut step: F,
) -> Res<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Res<T>>,
{
    let mut wait = first_wait;
    let mut attempt = 1;
    loop {
        match step().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt < tries.max(1) => {
                eprintln!(
                    "onyx-node: start: {what}: {e} (try {attempt} of {tries}; again in {} ms)",
                    wait.as_millis()
                );
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(STARTUP_MAX_WAIT);
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

type Res<T> = Result<T, String>;
/// The chain node client.
type Rpc = HttpJsonRpc<HyperPost>;

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn rpc(c: &NodeConfig) -> Rpc {
    HttpJsonRpc::new(c.rpc_url.clone(), HyperPost::for_rpc(RPC_TIMEOUT))
}

/// The chain indexer's node client: `eth_getLogs` answers up to 16 MiB
/// (IDX-R1-2).
fn rpc_logs(c: &NodeConfig) -> Rpc {
    HttpJsonRpc::new(c.rpc_url.clone(), HyperPost::for_logs(RPC_TIMEOUT))
}

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// `OnyxRevenue.reporter()` at the latest block (L15 runbook "rotate the
/// reporter"): the address the contract accepts `settle`, `credit` and
/// `registerObservation` from.
pub async fn read_reporter<T: JsonRpc + ?Sized>(rpc: &T, revenue: [u8; 20]) -> Res<[u8; 20]> {
    read_address(rpc, revenue, "reporter()").await
}

/// `OnyxSettlement.coordinator()` at `latest` (ROADMAP C10: the signed
/// book's reviewer is never this key, THREATS C10-1).
pub async fn read_coordinator<T: JsonRpc + ?Sized>(rpc: &T, settlement: [u8; 20]) -> Res<[u8; 20]> {
    read_address(rpc, settlement, "coordinator()").await
}

/// What a node checks of its `OnyxPrepaid` before it takes vouchers (ROADMAP S7): the reporter,
/// token and revenue it names, and its exit delay (seconds).
pub async fn read_prepaid<T: JsonRpc + ?Sized>(
    rpc: &T,
    prepaid: [u8; 20],
) -> Res<([u8; 20], [u8; 20], [u8; 20], u64)> {
    let reporter = read_address(rpc, prepaid, "reporter()").await?;
    let token = read_address(rpc, prepaid, "token()").await?;
    let revenue = read_address(rpc, prepaid, "revenue()").await?;
    let delay = read_word(rpc, prepaid, "exitDelay()").await?;
    if delay[..24].iter().any(|b| *b != 0) {
        return Err("exitDelay(): not a uint64".into());
    }
    let delay = u64::from_be_bytes(delay[24..].try_into().map_err(|_| "exitDelay()")?);
    Ok((reporter, token, revenue, delay))
}

/// Refuses an `OnyxPrepaid` that is not this deployment's: another token, another
/// `OnyxRevenue`, or another reporter than the operator key (its redemptions would revert).
pub fn check_prepaid(
    key_address: [u8; 20],
    usdc: Option<[u8; 20]>,
    revenue: Option<[u8; 20]>,
    onchain: ([u8; 20], [u8; 20], [u8; 20], u64),
) -> Res<()> {
    let (reporter, token, rev, _) = onchain;
    if reporter != key_address {
        return Err(format!(
            "ONYX_OPERATOR_KEY_FILE holds the key of {}, but OnyxPrepaid.reporter() is {}",
            hx(&key_address),
            hx(&reporter)
        ));
    }
    if Some(token) != usdc || Some(rev) != revenue {
        return Err(format!(
            "OnyxPrepaid names token {} and revenue {}, not the deployment's",
            hx(&token),
            hx(&rev)
        ));
    }
    Ok(())
}

async fn read_word<T: JsonRpc + ?Sized>(rpc: &T, to: [u8; 20], sig: &str) -> Res<Vec<u8>> {
    let selector = &onyx_core::keccak256(sig.as_bytes()).0[..4];
    let ans = rpc
        .call(
            "eth_call",
            serde_json::json!([{"to": hx(&to), "data": hx(selector)}, "latest"]),
        )
        .await?;
    ans.as_str()
        .and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())
        .filter(|w| w.len() == 32)
        .ok_or_else(|| format!("{sig}: not one word: {ans}"))
}

async fn read_address<T: JsonRpc + ?Sized>(rpc: &T, to: [u8; 20], sig: &str) -> Res<[u8; 20]> {
    let selector = &onyx_core::keccak256(sig.as_bytes()).0[..4];
    let ans = rpc
        .call(
            "eth_call",
            serde_json::json!([{"to": hx(&to), "data": hx(selector)}, "latest"]),
        )
        .await?;
    let word = ans
        .as_str()
        .and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())
        .filter(|w| w.len() == 32 && w[..12].iter().all(|b| *b == 0))
        .ok_or_else(|| format!("{sig}: not an address word: {ans}"))?;
    let mut a = [0u8; 20];
    a.copy_from_slice(&word[12..]);
    Ok(a)
}

/// Refuses a start whose operator key is not OnyxRevenue's reporter: every
/// settle and credit it sent would revert `NotReporter`, so paid requests
/// would fail and receipts pile up (L15 rehearsal, docs/audit/2026-10-16-runbook-rehearsal.md).
/// Addresses only; the key is never printed.
pub fn check_reporter(key_address: [u8; 20], reporter: [u8; 20]) -> Res<()> {
    if key_address == reporter {
        return Ok(());
    }
    Err(format!(
        "ONYX_OPERATOR_KEY_FILE holds the key of {}, but OnyxRevenue.reporter() is {}: install the reporter's key, or have the owner call setReporter (docs/ops/runbooks/rotate-reporter.md)",
        hx(&key_address),
        hx(&reporter)
    ))
}

/// Everything read from disk, checked, before any network call.
struct Loaded {
    config: NodeConfig,
    deployment: Deployment,
    catalog: Catalog,
    shards: usize,
    /// Shards held for the chain indexer (`ONYX_INDEXER=on`, ADR-0033).
    held: Option<Holdings>,
    /// Every spec's params, for the serving checks (stored in the row store).
    params: Vec<Vec<u8>>,
    /// Serving checks the catalog's shards name.
    serving_checks: Vec<String>,
    /// Each catalog entry's shard id (`--check-config` prints them).
    entry_ids: Vec<String>,
}

fn load(allow_mainnet: bool) -> Res<Loaded> {
    load_with(|k| std::env::var(k).ok(), allow_mainnet)
}

/// [`load`] with the settings read through `get` (tests pass a map).
fn load_with(get: impl Fn(&str) -> Option<String>, allow_mainnet: bool) -> Res<Loaded> {
    let config = NodeConfig::from_env(get, allow_mainnet)?;
    let text = std::fs::read_to_string(&config.deployment)
        .map_err(|e| format!("ONYX_DEPLOYMENT {}: {e}", config.deployment.display()))?;
    let raw: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("ONYX_DEPLOYMENT: {e}"))?;
    let deployment = Deployment::from_json(&raw, config.env, config.allow_mainnet)?;
    if config.signed_reports.is_some() && deployment.signed_book.is_none() {
        return Err(
            "ONYX_SIGNED_REPORTS needs the manifest's signedBook, signedBookCodeHash and signedBookReviewer (scripts/deploy-signed-book.sh --manifest)"
                .into(),
        );
    }
    if config.payments != PaymentMode::Off && deployment.revenue.is_none() {
        return Err(
            "payments are on but the manifest has no OnyxRevenue (revenue, revenueCodeHash, usdc)"
                .into(),
        );
    }
    let file = CatalogFile::from_json(
        &std::fs::read_to_string(&config.catalog)
            .map_err(|e| format!("ONYX_CATALOG {}: {e}", config.catalog.display()))?,
    )?;
    let mut catalog = file.to_catalog()?;
    // ADR-0033: with the indexer, the chain says what is served; the file
    // only says what the operator holds.
    let held = match &config.indexer {
        Some(_) => {
            if !file.shards.is_empty() {
                return Err("ONYX_INDEXER=on: the catalog file must list no served shards (move them to `held`; the indexer serves what the chain finalized)".into());
            }
            if deployment.deploy_block.is_none() {
                return Err(
                    "ONYX_INDEXER=on needs the settlement contract's deployBlock in the manifest"
                        .into(),
                );
            }
            Some(file.holdings_at(now_unix())?)
        }
        None => {
            if !file.held.is_empty() {
                return Err(
                    "the catalog file holds shards for the chain indexer: set ONYX_INDEXER=on"
                        .into(),
                );
            }
            // R-LBL-FIX-1: listed shards get the indexer's label-set check
            // (pinned set held, evidence resolves) before anything is served.
            file.check_listed_label_sets(&onyx_coordinator::cas::FsCas::new(
                config.cas_dir.clone(),
            ))
            .map_err(|e| format!("ONYX_CATALOG: {e}"))?;
            None
        }
    };
    // Level-2 metrics (ADR-0027 amendment 4): every round is checked against
    // its store's template at load; a store that does not verify stops the start.
    for p in &config.observations {
        let text = std::fs::read_to_string(p)
            .map_err(|e| format!("ONYX_OBSERVATIONS {}: {e}", p.display()))?;
        // The store's template must be the pinned registration (L2API-R4-F1).
        let feed = onyx_api::observations::ObservationStore::from_json(
            &text,
            now_unix(),
            &config.observation_templates,
        )
        .map_err(|e| format!("ONYX_OBSERVATIONS {}: {e}", p.display()))?;
        // A store may name only the deployment's registry (AUD-2-F8); the
        // registry's code hash is checked against the chain at serve time.
        crate::observations::check_store_registry(&feed, &deployment)
            .map_err(|e| format!("ONYX_OBSERVATIONS {}: {e}", p.display()))?;
        catalog
            .add_observations(feed)
            .map_err(|e| format!("ONYX_OBSERVATIONS {}: {e}", p.display()))?;
    }
    // Every pinned template must have a store (AUD-2-F8).
    crate::observations::check_pins_have_stores(&config.observation_templates, &catalog)?;
    if let Some(p) = &config.operator_key_file {
        // Checked now so a bad key file stops the start, not the first payment.
        read_key_file(p)?;
    }
    let mut serving_checks: Vec<String> = file
        .shards
        .iter()
        .filter_map(|e| e.serving_check.clone())
        .chain(file.held.iter().filter_map(|h| h.serving_check.clone()))
        .collect();
    serving_checks.sort();
    serving_checks.dedup();
    Ok(Loaded {
        entry_ids: file.entry_ids()?,
        params: file.params_blobs()?,
        serving_checks,
        shards: file.shards.len(),
        config,
        deployment,
        catalog,
        held,
    })
}

/// Opens the revenue book of a level-2 fact whose receipts were skipped as
/// unregistered (why 4): `OnyxRevenue.registerObservation(fact)` (R-REV-12).
/// Shard books are registered by anyone once final; this node does not build
/// those (it does not keep the commitment fields).
fn observation_registrar() -> onyx_api::reporter::Registrar {
    Arc::new(|round, shard| {
        (round == onyx_api::observations::OBSERVATION_ROUND)
            .then(|| onyx_api::observations::register_observation_calldata(&shard))
    })
}

async fn shutdown_signal() {
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = term => {},
        _ = tokio::signal::ctrl_c() => {},
    }
}

async fn serve(l: Loaded, gates: Vec<Arc<dyn ServingGate>>) -> Res<()> {
    let Loaded {
        config: c,
        deployment: d,
        mut catalog,
        shards,
        held,
        params,
        serving_checks,
        entry_ids: _,
    } = l;
    // ADR-0034 Amendment 1: a shard naming a serving check this build does
    // not run is refused on every request (fail closed); say so at start.
    for g in &serving_checks {
        if !gates.iter().any(|x| x.name() == g) {
            eprintln!("onyx-node: the catalog names the serving check {g}, which this build does not run: those shards are refused (run a build with that gate, e.g. onyx-node-btc)");
        }
    }
    // Review RESAPI-R1 F1: a shard whose binding a known gate covers needs
    // it whatever its catalog entry says.
    let mut claimed: Vec<&str> = catalog
        .shards()
        .filter_map(onyx_api::gate::known_claim)
        .filter(|g| !gates.iter().any(|x| x.name() == *g))
        .collect();
    claimed.sort_unstable();
    claimed.dedup();
    for g in claimed {
        eprintln!("onyx-node: catalog shards have a binding the serving check {g} covers, which this build does not run: those shards are refused (run a build with that gate, e.g. onyx-node-btc)");
    }
    // ROADMAP L14: say at start what fallback mode withholds.
    if c.rights == onyx_api::catalog::LabelRights::Fallback {
        let withheld = catalog.shards().filter(|m| m.label_derived()).count();
        eprintln!("onyx-node: ONYX_RIGHTS=fallback: label-derived metrics (entity flows, balances, reserves) are refused with 403 label_rights_withheld; {withheld} catalog shard(s) withheld now, indexed ones per request");
    }
    std::fs::create_dir_all(&c.state_dir)
        .map_err(|e| format!("ONYX_STATE_DIR {}: {e}", c.state_dir.display()))?;
    // Roots: the settlement contract's runtime code must be the deployed build.
    let roots = Arc::new(
        startup_step("settlement", STARTUP_TRIES, STARTUP_FIRST_WAIT, || async {
            ChainRoots::connect(
                d.settlement,
                d.chain_id,
                Some(d.settlement_code_hash),
                rpc(&c),
            )
            .await
            .map_err(|e| format!("settlement {}: {e}", hx(&d.settlement)))
        })
        .await?,
    );
    // AUD-2R-F3: with a pinned registry (its code and its ledger, which must
    // be the manifest's settlement, L16), every shard is floored at the level
    // the chain attested for it (which the registry refused below the label
    // set's and the metric's registered minimum, e.g. 4 for pool shards). A
    // catalog file's level never lifts a shard above the chain's.
    if let Some((registry, code_hash)) = d.registry.zip(d.registry_code_hash) {
        let base = &catalog;
        let (unattested, floored) =
            startup_step("registry", STARTUP_TRIES, STARTUP_FIRST_WAIT, || async {
                let mut cat = base.clone();
                onyx_api::levels::floor_to_registry(
                    &rpc(&c),
                    registry,
                    code_hash,
                    d.settlement,
                    &mut cat,
                )
                .await
                .map(|n| (n, cat))
                .map_err(|e| format!("registry {}: {e}", hx(&registry)))
            })
            .await?;
        catalog = floored;
        // L16 (THREATS L2R2-F2): say the pin is on, so an operator (and
        // scripts/docker-smoke.sh) can tell a pinned start from one whose
        // manifest names no registry (catalog levels, not the chain's).
        eprintln!(
            "onyx-node: registry {} pinned (code hash and ledger checked): shards are served at most at their attested level",
            hx(&registry)
        );
        if unattested > 0 {
            eprintln!(
                "onyx-node: {unattested} shard(s) have no final attested level on the registry; served at level 5 (no market settles on them)"
            );
        }
    }
    // ROADMAP C10: the level-3 book. Its code must be the pinned build, its
    // reviewer the manifest's, and that reviewer never the settlement's
    // coordinator (THREATS C10-1).
    let signed_book = match d.signed_book {
        Some((book, code_hash, reviewer)) => {
            let pin = onyx_api::signed_serve::BookPin {
                chain_id: d.chain_id,
                book,
                code_hash,
                reviewer,
            };
            let pin_ref = &pin;
            startup_step("signed book", STARTUP_TRIES, STARTUP_FIRST_WAIT, || async {
                onyx_api::signed_serve::check_book(&rpc(&c), pin_ref)
                    .await
                    .and(Ok(()))?;
                let coordinator = read_coordinator(&rpc(&c), d.settlement).await?;
                if coordinator == reviewer {
                    return Err(
                        "the signed book's reviewer is the settlement's coordinator (THREATS C10-1)"
                            .to_string(),
                    );
                }
                Ok(())
            })
            .await
            .map_err(|e| format!("signed book {}: {e}", hx(&book)))?;
            eprintln!(
                "onyx-node: signed book {} pinned (code hash and reviewer checked): level 3 is served only against settleable at a finalized block",
                hx(&book)
            );
            Some(pin)
        }
        None => None,
    };
    // Level-2 stores that name a registry: the chain must hold exactly the
    // pinned template for the metric (L2API-R4-F1).
    for metric in c.observation_templates.keys() {
        if let Some(feed) = catalog.observations(metric) {
            let registry = d.registry.zip(d.registry_code_hash);
            startup_step("level-2 pin", STARTUP_TRIES, STARTUP_FIRST_WAIT, || async {
                onyx_api::observations::verify_registered(&rpc(&c), d.chain_id, registry, feed)
                    .await
                    .map_err(|e| format!("ONYX_OBSERVATIONS: {e}"))
            })
            .await?;
        }
    }
    let cas: Arc<dyn Cas + Send + Sync> = Arc::new(FsCas::new(c.cas_dir.clone()));
    // Serving checks read a shard's params from the store by its params hash.
    if !gates.is_empty() {
        for p in &params {
            cas.put(Kind::Blob, p)
                .map_err(|e| format!("ONYX_CAS_DIR (spec params): {e}"))?;
        }
    }
    // The chain indexer (ADR-0033): fills the catalog from Finalized events.
    let indexer = match (&c.indexer, held) {
        (Some(s), Some(held)) => {
            let cfg = IndexerConfig {
                span: s.span,
                overlap: s.overlap,
                allow_test_labels: c.allow_test_labels,
                registry: d.registry,
                health_detail: s.health_detail,
                rescan_every: s.rescan_every,
                rescan_budget: s.rescan_budget,
                max_jump: s.max_jump,
                ..IndexerConfig::new(
                    d.chain_id,
                    d.settlement,
                    d.deploy_block.ok_or("manifest deployBlock")?,
                )
            };
            let idx = Indexer::open(
                cfg,
                rpc_logs(&c),
                held,
                cas.clone(),
                Some(s.state_file.clone()),
            )?;
            // IDX-R1-4: the manifest's deployBlock must be where the
            // contract's code appeared; refuse to start otherwise.
            let idx_ref = &idx;
            startup_step(
                "deploy block",
                STARTUP_TRIES,
                STARTUP_FIRST_WAIT,
                || async move {
                    idx_ref
                        .check_deploy_block()
                        .await
                        .map_err(|e| format!("ONYX_INDEXER: {e}"))
                },
            )
            .await?;
            Some((Arc::new(idx), s.every))
        }
        _ => None,
    };
    let network = d.network();
    let terms = |pay_to: [u8; 20], asset: [u8; 20]| PaymentTerms {
        network: network.clone(),
        asset: hx(&asset),
        pay_to: hx(&pay_to),
        max_timeout_seconds: 60,
        extra: Some(serde_json::json!({"name": c.token_domain.0, "version": c.token_domain.1})),
    };
    // Payments on: the revenue contract's code must be the deployed build too
    // (the same pin a client checks before it pays, REV-4).
    if c.payments != PaymentMode::Off {
        let pin = PayToPin::from_manifest(&d.raw).map_err(|e| format!("manifest: {e}"))?;
        let pin = &pin;
        startup_step("OnyxRevenue", STARTUP_TRIES, STARTUP_FIRST_WAIT, || async {
            pin.check_code(&rpc(&c))
                .await
                .map(|_| ())
                .map_err(|e| format!("OnyxRevenue: {e}"))
        })
        .await?;
    }
    let key = c
        .operator_key_file
        .as_deref()
        .map(read_key_file)
        .transpose()?;
    let sender = |k: [u8; 32]| -> Res<Sender<Rpc>> {
        Sender::new(rpc(&c), d.chain_id, k)
            .map(|s| s.with_fee_limits(c.max_fee_per_gas, c.max_gas))
            .ok_or_else(|| "operator key is not a valid secp256k1 key".to_string())
    };
    // L15 (rotate the reporter): a node that sends as the reporter must hold
    // the key OnyxRevenue names, or every settle and credit reverts.
    let sends_as_reporter = c.payments == PaymentMode::SelfSettle
        || (c.payments == PaymentMode::Facilitator && c.reporter);
    if let (true, Some(k), Some(revenue)) = (sends_as_reporter, key, d.revenue) {
        let me = sender(k)?.address();
        let onchain = startup_step(
            "OnyxRevenue reporter",
            STARTUP_TRIES,
            STARTUP_FIRST_WAIT,
            || async {
                read_reporter(&rpc(&c), revenue)
                    .await
                    .map_err(|e| format!("OnyxRevenue: {e}"))
            },
        )
        .await?;
        check_reporter(me, onchain)?;
        eprintln!(
            "onyx-node: operator key {} is OnyxRevenue's reporter",
            hx(&me)
        );
    }
    type Parts = (
        Option<Payments>,
        Option<Arc<SelfSettle<Rpc>>>,
        Option<Arc<Sender<Rpc>>>,
    );
    let (payments, settle, reporter_parts): Parts = match c.payments {
        PaymentMode::Off => (None, None, None),
        PaymentMode::SelfSettle => {
            let (revenue, usdc) = (d.revenue.ok_or("no revenue")?, d.usdc.ok_or("no usdc")?);
            let k = key.ok_or("self-settle needs the operator key")?;
            let f = Arc::new(
                SelfSettle::new(
                    sender(k)?,
                    revenue,
                    usdc,
                    network.clone(),
                    shutdown::SELF_SETTLE_DEADLINE,
                )
                .persist(c.state_dir.join("self-settle"))
                .map_err(|e| format!("state dir (self-settle journals): {e}"))?,
            );
            (
                Some(Payments {
                    terms: terms(revenue, usdc),
                    facilitator: f.clone() as Arc<dyn Facilitator>,
                }),
                Some(f),
                None,
            )
        }
        PaymentMode::Facilitator => {
            let (revenue, usdc) = (d.revenue.ok_or("no revenue")?, d.usdc.ok_or("no usdc")?);
            let url = c.facilitator_url.clone().ok_or("no facilitator URL")?;
            let f = Arc::new(HttpFacilitator::new(
                url,
                // The whole settle patience, not the 4 s verify timeout: a
                // slower settle that landed must still reach its receipt
                // (OPS-L2-F1).
                HyperPost::for_facilitator(&ApiConfig::default()),
            ));
            (
                Some(Payments {
                    terms: terms(revenue, usdc),
                    facilitator: f as Arc<dyn Facilitator>,
                }),
                None,
                // ONYX_REPORTER=off keeps receipts on disk only (an explicit
                // choice, OPS-L1-F5); otherwise the config required the key.
                if c.reporter {
                    key.map(sender).transpose()?.map(Arc::new)
                } else {
                    None
                },
            )
        }
    };
    let mut state = ApiState::new(
        catalog,
        roots,
        cas,
        payments,
        Arc::new(now_unix),
        c.public_url.clone(),
        ApiConfig {
            allow_test_labels: c.allow_test_labels,
            rights: c.rights,
            allow_mainnet: c.allow_mainnet,
            trusted_proxies: c.trusted_proxies.clone(),
            ..ApiConfig::default()
        },
    )
    .map_err(|e| e.0)?;
    // ROADMAP S7: the prepaid pass sends with the reporter's own sender (one nonce sequence).
    let prepaid_sender = reporter_parts.clone();
    // Receipts and the reporter that posts them to OnyxRevenue.credit.
    let reporter = match (&settle, reporter_parts, d.revenue) {
        (Some(f), _, _) => {
            state.receipts = f.receipts();
            Some(Arc::new(
                Reporter::for_self_settle(f.clone())
                    .with_state_file(c.state_dir.join("credits-in-flight.jsonl"))
                    .map_err(|e| format!("state dir (credits in flight): {e}"))?
                    .with_registrar(observation_registrar()),
            ))
        }
        (None, sender, Some(revenue)) if c.payments == PaymentMode::Facilitator => {
            state = state
                .with_receipt_log(
                    c.state_dir.join("receipts.jsonl"),
                    &Binding {
                        chain_id: d.chain_id,
                        contract: revenue,
                    },
                )
                .map_err(|e| format!("state dir (receipts): {e}"))?;
            match sender {
                Some(s) => Some(Arc::new(
                    Reporter::new(s, revenue, state.receipts.clone())
                        .with_state_file(c.state_dir.join("credits-in-flight.jsonl"))
                        .map_err(|e| format!("state dir (credits in flight): {e}"))?
                        .with_registrar(observation_registrar()),
                )),
                None => {
                    eprintln!("onyx-node: ONYX_REPORTER=off: receipts are kept on disk but not posted to OnyxRevenue");
                    None
                }
            }
        }
        _ => None,
    };
    if let Some(r) = &reporter {
        state = state.with_reporter(r.clone() as Arc<dyn ReporterHealth>);
    }
    // ROADMAP S7 (ADR-0046 amendment 3): prepaid vouchers beside x402, and the pass that
    // redeems and allocates them, at least every half exit delay.
    let mut prepaid_books = None;
    let prepaid_pass = match (c.prepaid, prepaid_sender) {
        (None, _) => None,
        (Some(_), None) => return Err("ONYX_PREPAID needs the reporter's key".into()),
        (Some(contract), Some(s)) => {
            let onchain =
                startup_step("OnyxPrepaid", STARTUP_TRIES, STARTUP_FIRST_WAIT, || async {
                    read_prepaid(&rpc(&c), contract)
                        .await
                        .map_err(|e| format!("OnyxPrepaid: {e}"))
                })
                .await?;
            check_prepaid(s.address(), d.usdc, d.revenue, onchain)?;
            let domain = onyx_api::prepaid::domain_separator(d.chain_id, &contract);
            let books = Arc::new(
                PrepaidState::open(
                    c.state_dir.join("prepaid.journal"),
                    domain,
                    Arc::new(ChainAccounts::new(rpc(&c), contract)),
                    60,
                )?
                // ADR-0049 (R-OBS-8): a hold lasts at most half the exit delay.
                .with_hold_limit(onchain.3 / 2),
            );
            state = state.with_prepaid(books.clone());
            prepaid_books = Some(books.clone());
            let every = c
                .reporter_every
                .min(Duration::from_secs((onchain.3 / 2).max(1)));
            eprintln!(
                "onyx-node: prepaid vouchers on: OnyxPrepaid {}, a pass every {} s",
                hx(&contract),
                every.as_secs()
            );
            let pass = PrepaidReporter::new(
                books,
                Arc::new(ChainAccounts::latest(rpc(&c), contract)),
                s,
                contract,
                Duration::from_secs(120),
            );
            Some((pass, every))
        }
    };
    // ADR-0049 (NORTH-STAR NS10): observe on the first paid query.
    let observe_on = match (&c.observe, &prepaid_books) {
        (None, _) => false,
        (Some(_), None) => return Err("ONYX_OBSERVE needs ONYX_PREPAID".into()),
        (Some(path), Some(books)) => {
            let f = crate::observe::load(path, &c.observations)?;
            {
                let cat = state.catalog.read().map_err(|_| "catalog lock")?;
                for m in f.offers.keys() {
                    if !cat
                        .observations(m)
                        .is_some_and(|feed| feed.template.is_dated())
                    {
                        return Err(format!(
                            "ONYX_OBSERVE offers {m}, which no dated ONYX_OBSERVATIONS store serves"
                        ));
                    }
                }
            }
            let runner = Arc::new(crate::observe::CommandRunner::new(
                f.command,
                c.observation_templates.clone(),
                now_unix,
            ));
            let obs = onyx_api::observe::ObserveState::new(f.offers, runner, books)?;
            eprintln!(
                "onyx-node: observe on payment: {} metric(s), {} hold(s) pending from before the restart",
                obs.metrics(),
                books.holds().len()
            );
            state = state.with_observe(Arc::new(obs)).map_err(|e| e.0)?;
            true
        }
    };
    if let Some((i, _)) = &indexer {
        state = state.with_indexer(i.clone() as Arc<dyn IndexerHealth>);
    }
    for g in gates {
        state = state.with_gate(g).map_err(|e| e.0)?;
    }
    // Settle intents (OPS-L2-F1): every paid settle is on disk before it is
    // made; the resolver turns those whose outcome stayed unknown (a slow
    // facilitator, a crash, a kill) into receipts when the token's logs
    // prove the payment reached OnyxRevenue, and drops them once the
    // authorization expired unused. It needs only read access to the node,
    // so it runs with or without a reporter key.
    let resolver_task = match (c.payments, d.revenue, d.usdc) {
        (PaymentMode::Off, _, _) => None,
        (_, Some(revenue), Some(usdc)) => {
            state = state
                .with_intent_log(
                    c.state_dir.join("settle-intents.jsonl"),
                    &Binding {
                        chain_id: d.chain_id,
                        contract: revenue,
                    },
                )
                .map_err(|e| format!("state dir (settle intents): {e}"))?;
            let open = state.intents.open_count();
            if open > 0 {
                eprintln!("onyx-node: {open} settle intent(s) from before the restart: resolving them from the chain");
            }
            // M7-F5 residual: a facilitator settle whose outcome is
            // unknown is checked against the token before it is re-sent or
            // given up (`onyx_api::settle_check`).
            if c.payments == PaymentMode::Facilitator {
                state = state.with_settle_check(Arc::new(ChainSettleCheck::new(
                    rpc(&c),
                    usdc,
                    revenue,
                )));
            }
            let resolver = Arc::new(IntentResolver::new(
                rpc(&c),
                usdc,
                revenue,
                state.intents.clone(),
                state.receipts.clone(),
            ));
            Some(resolver.spawn(c.reporter_every))
        }
        _ => return Err("payments need the deployment's revenue and usdc addresses".into()),
    };
    let reporter_task = reporter.map(|r| r.spawn(c.reporter_every));
    let prepaid_task = prepaid_pass.map(|(pass, every)| {
        tokio::spawn(async move {
            loop {
                match pass.pass().await {
                    Ok(p) if p.redeemed + p.allocated > 0 => eprintln!(
                        "onyx-node: prepaid: {} voucher(s) redeemed, {} book(s) paid {}",
                        p.redeemed, p.allocated, p.allocated_amount
                    ),
                    Ok(_) => {}
                    Err(e) => eprintln!("onyx-node: prepaid pass: {e}"),
                }
                tokio::time::sleep(every).await;
            }
        })
    });
    // ROADMAP C10: `/v1/signed/{fact}` when the manifest pins the book and
    // the operator runs a level-3 coordinator (its post log). Free, never
    // priced, outside `/v1/settlement` (no market settles on it before H6).
    // ROADMAP C62: the same store reports its post log on `/v1/health`.
    let signed_store = match (signed_book, &c.signed_reports) {
        (Some(pin), Some(path)) => {
            let store = Arc::new(onyx_api::signed_serve::SignedStore::new(
                pin,
                Some(path.clone()),
                Arc::new(rpc(&c)),
                Arc::new(now_unix),
            ));
            if let Err(e) = store.log() {
                return Err(format!("ONYX_SIGNED_REPORTS: {e}"));
            }
            state = state.with_level3(store.clone());
            Some(store)
        }
        _ => None,
    };
    // Open before binding the listener; damaged/locked intake must fail startup.
    let intake = crate::submissions::routes(c.submissions.as_deref())
        .map_err(|e| format!("ONYX_SUBMISSIONS: {e}"))?;
    // ROADMAP D30 (PP3): salted label rows; a bad or non-test set fails startup.
    let labels = salted_label_routes(&c.salted_labels)?;
    // North-star loop NL9: the bounty board. Wants are taken only from open prepaid deposits
    // (read on chain, never charged); without ONYX_PREPAID the board is shown and posting is closed.
    let wants_accounts = c.prepaid.map(|contract| {
        Arc::new(ChainAccounts::new(rpc(&c), contract))
            as Arc<dyn onyx_api::prepaid_gate::AccountSource>
    });
    let wants = onyx_api::wants::WantsStore::open(&c.state_dir, wants_accounts, Arc::new(now_unix))
        .map_err(|e| format!("wants: {e}"))?;
    let wants = onyx_api::wants::routes(Arc::new(wants));
    let state = Arc::new(state);
    // Listed shards are judged now; indexed ones after each poll.
    for r in onyx_api::gate::refresh(&state).await {
        eprintln!(
            "onyx-node: serving check {}: {} of {} finalized shard(s) cleared",
            r.gate, r.cleared, r.judged
        );
    }
    // Review RESAPI-R1 F3: judge again on a timer, with or without the
    // indexer, so a listed shard finalizing later is served without a
    // restart and a failed root read is retried.
    let gate_task = (!state.gates.is_empty()).then(|| {
        onyx_api::gate::spawn_refresh(state.clone(), c.gate_every, |r| {
            if let Some(why) = &r.first_refusal {
                eprintln!(
                    "onyx-node: serving check {}: {} of {} cleared ({} root read error(s)); first refusal: {why}",
                    r.gate, r.cleared, r.judged, r.read_errors
                );
            }
        })
    });
    // ADR-0049: the observe worker runs one paid round at a time.
    let observe_task = observe_on.then(|| {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                let s = onyx_api::observe::step(&st).await;
                if !s.released.is_empty() {
                    eprintln!(
                        "onyx-node: observe: {} hold(s) released (round failed, did not check, or missed its deadline)",
                        s.released.len()
                    );
                }
                if s.observed.is_some() {
                    eprintln!("onyx-node: observe: a paid round is served and its hold charged");
                } else if s.released.is_empty() {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        })
    });
    let held_count = indexer.as_ref().map(|(i, _)| i.known().len());
    let indexer_task = indexer.map(|(idx, every)| {
        let st = state.clone();
        tokio::spawn(async move {
            let mut polls: u64 = 0;
            loop {
                polls += 1;
                match idx.poll(&st.catalog).await {
                    Ok(r) => {
                        for (round, shard) in &r.admitted {
                            eprintln!("onyx-node: indexer: round {round} shard {shard} is final on chain and its held rows hash to the root: serving it");
                        }
                        // ADR-0034 Amendment 1: judge the finalized shards
                        // again when the catalog grew or shrank (review
                        // RESAPI-R2-2), and every 20 polls.
                        for g in onyx_api::gate::after_poll(&st, &r, polls).await.unwrap_or_default() {
                            if let Some(why) = g.first_refusal {
                                eprintln!("onyx-node: serving check {}: {} of {} cleared; first refusal: {why}", g.gate, g.cleared, g.judged);
                            }
                        }
                        for (round, shard) in &r.removed {
                            eprintln!("onyx-node: indexer: round {round} shard {shard}: two reads refuted its final view; no longer served");
                        }
                    }
                    Err(e) => eprintln!(
                        "onyx-node: indexer: {e} (next try in {}s)",
                        every.as_secs()
                    ),
                }
                tokio::time::sleep(every).await;
            }
        })
    });
    let (request_timeout, settle_budget) =
        (state.config.request_timeout, state.settle_task_budget());
    let stop_total = shutdown::total(state.stop_budget());
    if stop_total > shutdown::SUPERVISOR_STOP {
        eprintln!(
            "onyx-node: warning: a clean stop can take {}s, more than the shipped {}s stop timeout; raise TimeoutStopSec / --stop-timeout",
            stop_total.as_secs(),
            shutdown::SUPERVISOR_STOP.as_secs()
        );
    }
    let tasks = state.settle_tasks.clone();
    let unknown = state.unknown_settles.clone();
    let listener = tokio::net::TcpListener::bind(c.listen)
        .await
        .map_err(|e| format!("ONYX_LISTEN {}: {e}", c.listen))?;
    let shards = match (&c.indexer, held_count) {
        (Some(s), Some(known)) => format!(
            "indexer on from block {} (span {}, {known} shard(s) known from {})",
            d.deploy_block.unwrap_or(0),
            s.span,
            s.state_file.display()
        ),
        _ => format!("{shards} shard(s)"),
    };
    eprintln!(
        "onyx-node: listening on {} ({}); chain {} via {}; settlement {}; payments {:?}; {}",
        c.listen,
        c.public_url,
        d.chain_id,
        url_host(&c.rpc_url),
        hx(&d.settlement),
        c.payments,
        shards
    );
    if let Some(w) = c.plain_http_public_warning() {
        eprintln!("{w}");
    }
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let limits = crate::serve::ServeLimits {
        header_read_timeout: c.header_read_timeout,
        max_connections: c.max_connections,
        max_per_client: c.max_per_client,
    };
    let trusted = c.trusted_proxies.clone();
    // ROADMAP C6, C20: `/v1/track` answers from the coordinator's publication file
    // (`ONYX_TRACK_PUBLICATIONS`, re-read when it changes) or, unset, from an empty store (every
    // record a 404 `no_track_record`); its served head is kept in ONYX_STATE_DIR. A bad file
    // answers 503 on the track routes only. Paths in, a Router out: no handle to any record here.
    let (rights, publications, track_state) =
        (c.rights, c.track_publications.clone(), c.state_dir.clone());
    let track = onyx_api::track_serve::node_routes(rights, publications, track_state);
    // ROADMAP C10: the level-3 route, from the store built above.
    let extra = match signed_store {
        Some(store) => track.merge(onyx_api::signed_serve::routes(store)),
        None => track,
    };
    let extra = extra.merge(intake).merge(labels).merge(wants);
    let server = tokio::spawn(async move {
        // Header read timeout and a connection cap (AUD-1-F5).
        crate::serve::serve_with(
            listener,
            router_with(state, extra),
            limits,
            &trusted,
            async {
                let _ = stop_rx.await;
            },
        )
        .await
    });
    shutdown_signal().await;
    eprintln!(
        "onyx-node: stopping: no new connections; draining in-flight requests and their settles (up to {}s)",
        stop_total.as_secs()
    );
    let _ = stop_tx.send(());
    // Requests first, then the settle tasks they started: a settle outlives
    // its request, and dropping the runtime would cancel it before its
    // receipt or pending entry is written (OPS-L1-F1).
    let stopped = shutdown::drain(server, &tasks, request_timeout, settle_budget)
        .await
        .with_unknown(unknown.load(std::sync::atomic::Ordering::Relaxed));
    // A reporter post cut short commits nothing and is repeated after the
    // restart (its credits in flight are on disk); journals sync every write.
    for t in [
        reporter_task,
        resolver_task,
        indexer_task,
        gate_task,
        prepaid_task,
        observe_task,
    ]
    .into_iter()
    .flatten()
    {
        t.abort();
        let _ = t.await;
    }
    if stopped.clean() {
        eprintln!(
            "onyx-node: stopped (drained: requests and settles finished, no unknown outcomes)"
        );
    } else {
        eprintln!(
            "onyx-node: stopped (requests drained: {}; settles cut short: {}; settles with an unknown outcome since start: {}); every one is a settle intent on disk, resolved from the chain after the restart (THREATS.md OPS-L2-F1)",
            stopped.requests_drained, stopped.settles_left, stopped.unknown_outcomes
        );
    }
    Ok(())
}

/// The `node` row of docs/ops/hardware.md on the state directory, at every
/// start and `--check-config` (ROADMAP P5): one summary line when all pass,
/// else each line that is not `PASS`. `false`: refuse to start, only when the
/// state directory has less free space than the soak's largest state
/// directory ([`doctor::NODE_REFUSE_FREE_BYTES`]); every other shortfall warns.
fn hardware_preflight(state_dir: &std::path::Path) -> bool {
    let (lines, refuse) = doctor::preflight_node(state_dir, &doctor::Roots::default());
    let open: Vec<_> = lines
        .iter()
        .filter(|l| !matches!(l.status, doctor::Status::Pass | doctor::Status::Skip))
        .collect();
    if open.is_empty() {
        eprintln!("onyx-node: hardware preflight (node): PASS (docs/ops/hardware.md)");
    }
    for l in open {
        eprintln!("onyx-node: hardware preflight (node): {l} (docs/ops/hardware.md)");
    }
    if refuse {
        eprintln!(
            "onyx-node: refusing to start: {} has less than {} bytes free, the state directory the soak measured",
            state_dir.display(),
            doctor::NODE_REFUSE_FREE_BYTES
        );
    }
    !refuse
}

/// Run the node with `gates` as its serving checks (none in `onyx-node`).
pub fn main_with(gates: Vec<Arc<dyn ServingGate>>) -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("doctor") {
        return doctor::main(&args[1..]);
    }
    let mut allow_mainnet = false;
    let mut check_only = false;
    let mut health = false;
    for a in &args {
        match a.as_str() {
            "--allow-mainnet" => allow_mainnet = true,
            "--check-config" => check_only = true,
            "healthcheck" => health = true,
            "-h" | "--help" => {
                println!("usage: onyx-node [--check-config | healthcheck | doctor ...] [--allow-mainnet]\nconfiguration: ONYX_* environment variables (docs/ops/config.md)\nhardware preflight: onyx-node doctor --help (docs/ops/hardware.md)");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("onyx-node: unknown argument {other:?} (see --help)");
                return ExitCode::from(2);
            }
        }
    }
    if health {
        let listen = std::env::var("ONYX_LISTEN")
            .ok()
            .and_then(|s| s.parse::<SocketAddr>().ok())
            .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8080)));
        return match healthcheck::check(healthcheck::probe_addr(listen), Duration::from_secs(3)) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("onyx-node healthcheck: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let loaded = match load(allow_mainnet) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("onyx-node: {e}");
            return ExitCode::from(2);
        }
    };
    if !hardware_preflight(&loaded.config.state_dir) {
        return ExitCode::from(2);
    }
    if check_only {
        if let Some(w) = loaded.config.plain_http_public_warning() {
            eprintln!("{w}");
        }
        let shards = match &loaded.held {
            Some(h) => format!(
                "indexer on from block {}, {} held shard(s)",
                loaded.deployment.deploy_block.unwrap_or(0),
                h.len()
            ),
            None => format!("{} shard(s)", loaded.shards),
        };
        println!(
            "onyx-node: config ok: chain {}, settlement {}, payments {:?}, {}",
            loaded.deployment.chain_id,
            hx(&loaded.deployment.settlement),
            loaded.config.payments,
            shards
        );
        for line in &loaded.entry_ids {
            println!("onyx-node: catalog {line}");
        }
        // The operator key's address (public; the key is never printed), to
        // compare with OnyxRevenue.reporter() (L15 rotate-reporter runbook).
        if let Some(p) = &loaded.config.operator_key_file {
            let addr = read_key_file(p).ok().and_then(|k| {
                Sender::new(rpc(&loaded.config), loaded.deployment.chain_id, k).map(|s| s.address())
            });
            match addr {
                Some(a) => println!("onyx-node: operator key address {}", hx(&a)),
                None => {
                    eprintln!("onyx-node: operator key is not a valid secp256k1 key");
                    return ExitCode::from(2);
                }
            }
        }
        return ExitCode::SUCCESS;
    }
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("onyx-node: runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(serve(loaded, gates)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("onyx-node: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The salted-label route over the configured files, or no route when none is configured.
fn salted_label_routes(files: &[std::path::PathBuf]) -> Result<axum::Router, String> {
    if files.is_empty() {
        return Ok(axum::Router::new());
    }
    let mut parsed = Vec::with_capacity(files.len());
    for f in files {
        let text = std::fs::read_to_string(f)
            .map_err(|e| format!("ONYX_SALTED_LABELS {}: {e}", f.display()))?;
        parsed.push(
            serde_json::from_str(&text)
                .map_err(|e| format!("ONYX_SALTED_LABELS {}: {e}", f.display()))?,
        );
    }
    let store = onyx_api::labels_serve::SaltedLabelStore::from_files(&parsed)
        .map_err(|e| format!("ONYX_SALTED_LABELS: {e}"))?;
    eprintln!(
        "onyx-node: serving {} salted label set(s) (test sets only)",
        store.content_hashes().len()
    );
    Ok(onyx_api::labels_serve::routes(std::sync::Arc::new(store)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A node answering `eth_call` with one fixed word.
    struct Word(String);
    impl JsonRpc for Word {
        fn call<'a>(
            &'a self,
            method: &'a str,
            params: serde_json::Value,
        ) -> onyx_api::x402::BoxFuture<'a, Result<serde_json::Value, String>> {
            Box::pin(async move {
                assert_eq!(method, "eth_call");
                // reporter() = 0x010ec441 (cast sig "reporter()")
                assert_eq!(params[0]["data"], "0x010ec441");
                Ok(serde_json::json!(self.0.clone()))
            })
        }
    }

    /// L15 rotate-reporter runbook: the start reads OnyxRevenue.reporter()
    /// and refuses an operator key that is not it (the old key after a
    /// rotation, or a new key before setReporter), naming both addresses.
    #[tokio::test]
    async fn the_start_refuses_an_operator_key_that_is_not_the_reporter() {
        let reporter = [0xab; 20];
        let word = format!("0x{}{}", "00".repeat(12), "ab".repeat(20));
        let got = read_reporter(&Word(word), [1; 20]).await.expect("reporter");
        assert_eq!(got, reporter);
        assert!(check_reporter(reporter, got).is_ok());
        let err = check_reporter([0xcd; 20], got).expect_err("old key refused");
        assert!(
            err.contains(&"cd".repeat(20)) && err.contains(&"ab".repeat(20)),
            "{err}"
        );
        // Not an address word: dirty high bytes, short, not hex.
        for bad in [
            format!("0x{}{}", "01".repeat(12), "ab".repeat(20)),
            "0x1234".to_string(),
            "zz".to_string(),
        ] {
            assert!(read_reporter(&Word(bad), [1; 20]).await.is_err());
        }
    }
    /// An `OnyxPrepaid` answering four views by selector.
    struct PrepaidViews;
    impl JsonRpc for PrepaidViews {
        fn call<'a>(
            &'a self,
            _: &'a str,
            params: serde_json::Value,
        ) -> onyx_api::x402::BoxFuture<'a, Result<serde_json::Value, String>> {
            Box::pin(async move {
                let data = params[0]["data"].as_str().unwrap_or_default().to_string();
                let sel = |sig: &str| hx(&onyx_core::keccak256(sig.as_bytes()).0[..4]);
                let addr = |b: &str| format!("0x{}{}", "00".repeat(12), b.repeat(20));
                let w = if data == sel("reporter()") {
                    addr("ab")
                } else if data == sel("token()") {
                    addr("0c")
                } else if data == sel("revenue()") {
                    addr("0e")
                } else if data == sel("exitDelay()") {
                    format!("0x{:064x}", 3600)
                } else {
                    return Err(format!("unexpected call {data}"));
                };
                Ok(serde_json::json!(w))
            })
        }
    }

    /// ROADMAP S7 (ADR-0046 amendment 3): the start reads OnyxPrepaid's reporter, token,
    /// revenue and exit delay, and refuses a contract that is not this deployment's.
    #[tokio::test]
    async fn the_start_refuses_a_prepaid_contract_that_is_not_the_deployments() {
        let got = read_prepaid(&PrepaidViews, [1; 20]).await.expect("views");
        assert_eq!(got, ([0xab; 20], [0x0c; 20], [0x0e; 20], 3600));
        let ok = check_prepaid([0xab; 20], Some([0x0c; 20]), Some([0x0e; 20]), got);
        assert!(ok.is_ok());
        let e =
            check_prepaid([0xcd; 20], Some([0x0c; 20]), Some([0x0e; 20]), got).expect_err("key");
        assert!(
            e.contains(&"cd".repeat(20)) && e.contains(&"ab".repeat(20)),
            "{e}"
        );
        assert!(
            check_prepaid([0xab; 20], Some([0x0d; 20]), Some([0x0e; 20]), got).is_err(),
            "token"
        );
        assert!(
            check_prepaid([0xab; 20], Some([0x0c; 20]), None, got).is_err(),
            "revenue"
        );
    }

    use crate::catalog_file::{ShardEntry, CATALOG_VERSION};
    use onyx_coordinator::cas::{CasExt, Kind};
    use onyx_core::{
        keccak256, ChainId, Confidence, DataRights, EntityKind, EvidenceRef, Hash32, LabelSetRef,
        ShardSpec, Snapshot, SourceId, SourceRef,
    };
    use onyx_labels::{Label, LabelSet};
    use std::collections::BTreeMap;

    /// R-LBL-FIX-1 on the real start path (`load`, which `main_with` runs before
    /// any network call): with the indexer off, a catalog that lists a shard
    /// pinning a label set the row store does not hold stops the start, and
    /// the same files start once the set and its evidence are stored.
    #[test]
    fn the_start_refuses_a_listed_shard_whose_label_set_is_not_held() {
        let dir = std::env::temp_dir().join(format!(
            "onyx-node-start-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        // The committed fixture's end block: its header and hash.
        let p = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/btc/blocks-921080-921082.json"
        );
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(p).expect("fixture")).expect("json");
        let b = v["blocks"]
            .as_array()
            .and_then(|a| a.last())
            .expect("end block");
        let header = hex::decode(b["header"].as_str().expect("header")).expect("hex");
        let mut tip = [0u8; 32];
        tip.copy_from_slice(&hex::decode(b["hash"].as_str().expect("hash")).expect("hash hex"));
        let cas = FsCas::new(dir.join("cas"));
        let doc = cas
            .put(Kind::Blob, b"published reserve list")
            .expect("blob");
        let set = LabelSet {
            name: "onyx.entities.example".into(),
            version: 1,
            labels: vec![Label {
                address: "t:a".into(),
                entity: "entity:example".into(),
                kind: EntityKind::Exchange,
                confidence: Confidence(900_000),
                evidence: vec![EvidenceRef {
                    kind: "por-list".into(),
                    content_hash: doc,
                }],
            }],
        };
        let rights = DataRights::public_chain(SourceId("chain.test".into()));
        let spec = ShardSpec {
            recipe: None,
            runtime: None,
            label_set: Some(LabelSetRef {
                name: set.name.clone(),
                version: set.version,
                content_hash: set.content_hash(),
            }),
            source: SourceRef {
                source: SourceId("chain.test".into()),
                snapshot: Snapshot::Block {
                    chain: ChainId("btc.mainnet".into()),
                    height: 921_082,
                    block_hash: Hash32(tip),
                },
            },
            schema: keccak256(b"schema"),
            rights_hash: rights.hash(),
            params: vec![],
            registry_root: keccak256(b"r"),
        };
        let entry =
            ShardEntry::new(1, "test.flow", &spec, &rights, Some(1), Some(&header)).expect("entry");
        let catalog = CatalogFile {
            version: CATALOG_VERSION,
            shards: vec![entry],
            held: vec![],
            markets: vec![],
            default_pricing: None,
            pricing: Default::default(),
            default_dataset_pricing: None,
            dataset_pricing: Default::default(),
        };
        std::fs::write(
            dir.join("catalog.json"),
            serde_json::to_string(&catalog).expect("catalog json"),
        )
        .expect("catalog");
        std::fs::write(
            dir.join("deployment.json"),
            serde_json::json!({
                "chainId": 84_532,
                "settlement": format!("0x{}", "11".repeat(20)),
                "settlementCodeHash": format!("0x{}", "22".repeat(32)),
            })
            .to_string(),
        )
        .expect("deployment");
        let path = |f: &str| dir.join(f).display().to_string();
        let env = BTreeMap::from([
            ("ONYX_ENV", "testnet".to_string()),
            ("ONYX_RIGHTS", "cleared".to_string()),
            ("ONYX_PUBLIC_URL", "https://api.example.test/".to_string()),
            ("ONYX_RPC_URL", "https://sepolia.base.org".to_string()),
            ("ONYX_DEPLOYMENT", path("deployment.json")),
            ("ONYX_CATALOG", path("catalog.json")),
            ("ONYX_CAS_DIR", path("cas")),
            ("ONYX_STATE_DIR", path("state")),
        ]);
        let get = |k: &str| env.get(k).cloned();
        let err = match load_with(get, false) {
            Ok(_) => panic!("a listed shard with an unheld label set started"),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains("ONYX_CATALOG") && err.contains("label_set_missing"),
            "{err}"
        );
        // Held, with its evidence in the store: the same files start.
        cas.put_label_set(&set).expect("set");
        assert!(load_with(get, false).is_ok());
        // ROADMAP C10: a level-3 report log needs the manifest's book pin.
        let mut signed = env.clone();
        signed.insert("ONYX_SIGNED_REPORTS", path("posts.json"));
        let err = match load_with(|k: &str| signed.get(k).cloned(), false) {
            Ok(_) => panic!("ONYX_SIGNED_REPORTS started without a signed book in the manifest"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("signedBook"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
