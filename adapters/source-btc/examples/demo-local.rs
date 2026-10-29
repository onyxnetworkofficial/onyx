//! M8 rehearsal on a local chain (docs/ops/demo-local.md).
//!
//! Drives one real round of the BTC entity-flow shard through the deployed
//! settlement contracts on a local anvil node (chain id 31337 only), then
//! serves the finalized shard with onyx-api reading its root from the chain,
//! and checks one row end to end as a client would:
//!
//! 1. setup: test tokens, coordinator bond, reward pool, 16 staked validators;
//! 2. the coordinator opens round 1 with the shard (the contract snapshots the
//!    validator set and computes the round tag; Rust computes the same tag);
//! 3. two miners run the recipe, commit (bonded, EIP-191 signed) and reveal;
//! 4. the seed is drawn from the beacon block (Rust computes the same seed);
//!    each validator reruns the recipe for its assigned reveals, commits and
//!    reveals a verdict; validators with no task stand by for a dispute panel;
//! 5. the shard is finalized, the challenge window passes with no dispute, the
//!    miners' bonds and reward are released and everyone withdraws;
//! 6. onyx-api serves the shard (`ChainRoots`, `eth_call` at the `finalized`
//!    block); the client pays with x402 (local mock facilitator), checks the
//!    answer with `client::ClientNode::verify` against its own `getFinalizedRoot`
//!    call, and the contract's `verifyRow` accepts the same row and proof;
//! 7. with `--dispute`, a second round on the 3-block fixture where miner B
//!    reveals a made-up root: the shard goes Pending, a rerun case draws a
//!    panel of standby validators, each reruns and votes, the true root is
//!    finalized and B loses its bond;
//! 8. local mode, with the reserve tail files (`--reserve-dir`): a
//!    `btc-entity-reserve` shard is settled, found by the chain indexer,
//!    cleared by its serving check and served as `btc.entity-reserve` (402,
//!    then a paid row the client checks on chain at level 4); Binance's row
//!    is refused, and a forged-base spec is refused by the serving check
//!    (ADR-0034 Amendment 1).
//!
//! Modes (`--mode`, docs/ops/demo-testnet.md):
//! - `local` (default): anvil, chain id 31337, `cast send --unlocked` from
//!   anvil's dev accounts (public test keys read from anvil's `--config-out`
//!   file; they hold nothing outside this node), a local mock facilitator.
//!   Run it through `scripts/demo-local.sh`.
//! - `fork`: anvil forking Base Sepolia (loopback RPC, chain id 84532, anvil
//!   only): the real USDC bytecode, per-actor keys signed in this process,
//!   the deployer funded with exactly the planned budget, and self-settled
//!   x402 payments to `OnyxRevenue` (ADR-0026). The rehearsal of `testnet`.
//! - `testnet`: Base Sepolia over HTTPS (chain id 84532, never an anvil):
//!   the same as `fork`, waiting for real blocks, with the deployer key from
//!   `.env` and actor keys in a gitignored directory. Run both through
//!   `scripts/demo-testnet.sh`; `--plan` prints the budget and sends nothing.
//!
//! Any other chain is refused. Keys are read from files or the environment,
//! held in memory, and never printed or passed on a command line.

// Timings are reported in seconds; not protocol code.
#![allow(clippy::float_arithmetic)]
// The unit tests below cover option parsing only; under `cargo test` the
// driver itself is not called.
#![cfg_attr(test, allow(dead_code, unused_imports))]

use onyx_api::catalog::{Catalog, Market, PricingPolicy, ShardMeta};
use onyx_api::client::{
    draw_budget_from_return, observation_level_from_return, transcript_complete_from_return,
    verify_observation, verify_observation_onchain, ClientNode, Expect, MetricDef,
    ObservationExpect, PayToPin, Window,
};
use onyx_api::evm_tx::{Sender, Tx1559};
use onyx_api::indexer::{HeldShard, Holdings, Indexer, IndexerConfig, IndexerHealth, ShardStatus};
use onyx_api::journal::Binding;
use onyx_api::observation_book::{open_observation_books, Opened};
use onyx_api::observations::{ObservationFeed, ObservationStore, OBSERVATION_ROUND};
use onyx_api::receipts::ReceiptLog;
use onyx_api::reporter::Reporter;
use onyx_api::roots::{
    decode_finalized_root, get_finalized_root_calldata, ChainRoots, HttpJsonRpc, JsonRpc,
};
use onyx_api::self_settle::{transfer_with_authorization_calldata, SelfSettle};
use onyx_api::x402::{
    decode_header, encode_header, exact_evm, BoxFuture, Facilitator, FacilitatorError,
    FacilitatorRequest, HttpPost, HyperPost, MockFacilitator, PaymentPayload, PaymentRequired,
    PaymentTerms, SettleResponse, VerifyResponse, BASE_SEPOLIA, PAYMENT_REQUIRED, PAYMENT_RESPONSE,
    PAYMENT_SIGNATURE,
};
use onyx_api::{router, ApiConfig, ApiState, Payments};
use onyx_btc_schema::{BalanceParams, EntityBase, EntityFlowRow};
use onyx_coordinator::cas::{Cas, CasExt, Kind, MemCas};
use onyx_coordinator::dispute::DisputeConfig;
use onyx_coordinator::keyfile::ReceiptKey;
use onyx_coordinator::observe::{
    transcript_log_key, ChainView, DrawChain, DrawRecord, LogBlock, OnchainDraw, OnchainFact,
    Posted, PostedCommit, RecordedDraw, TranscriptLogQuery,
};
use onyx_coordinator::revenue::{split, Recipients, RevenueSplit};
use onyx_coordinator::round::{round_tag, RoundConfig};
use onyx_coordinator::validate::{assigned, canary_commit, sample_seed, ValidationConfig, Verdict};
use onyx_core::onchain::{metric_key, runtime_bytes, MetricKind, ShardPreimage};
use onyx_core::{
    evm_ledger_id, keccak256, Hash32, LabelSetRef, MetricId, ProofLevel, Registry, RuntimeProfile,
    ShardSpec, SourceAdapter, SourceId, SourceRef,
};
use onyx_miner::{Miner, Work};
use onyx_recipe_host::{Host, HOST_ID};
use onyx_recipe_sdk::OutputRow;
use onyx_source_btc::node::{load_node_range, NodePin, MAINNET_MIN_BITS};
use onyx_source_btc::{
    fixture_registry, label_set_proof_level, load_label_set, BtcFixtureAdapter, CHAIN_ID,
    METRIC_MIN_LEVELS, METRIC_POOL_BLOCKS, SOURCE_ID,
};
use onyx_validator::Validator;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Step 8: the reserve metric behind its serving check (ADR-0034 Amendment 1).
#[path = "demo-local-reserve/reserve.rs"]
mod reserve;

/// `--soak-rounds`: more rounds on the same deployment (scripts/soak.sh).
#[path = "demo-local-soak/soak.rs"]
mod soak;

/// Local mode's chain (anvil).
const ANVIL: u64 = 31_337;
/// Fork and testnet modes' chain (Base Sepolia).
const BASE_SEPOLIA_ID: u64 = 84_532;
/// Circle's USDC on Base Sepolia (developers.circle.com, "USDC contract
/// addresses"); EIP-712 domain name "USDC", version "2" (read from the token).
const USDC_BASE_SEPOLIA: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";
/// OP Stack `GasPriceOracle` predeploy: `getL1Fee(bytes)` prices a raw
/// transaction's L1 data fee, which an anvil fork does not charge.
const GAS_PRICE_ORACLE: &str = "0x420000000000000000000000000000000000000F";
const ROUND: u64 = 1;
/// Mock USDC has 6 decimals, like USDC.
const USDC: u128 = 1_000_000;
const STAKE: u128 = 100 * USDC;
const MINERS: usize = 2;
const VALIDATORS: usize = 16;
/// Each validator checks a reveal with this probability. With 16 validators
/// and 2 reveals, enough of them are left without a task to draw a dispute
/// panel (L29-F4), except with probability about 0.3%.
const RATE_PPM: u64 = 250_000;
const BOND: u128 = 4 * USDC;
const PRICE_MICRO_USDC: u64 = 2_500;
/// Seed of the demo payer's test key (public on purpose: it holds nothing).
const DEMO_PAYER_SEED: &str = "demo payer (test key, holds nothing)";
const DEMO_PAY_TO: &str = "0x209693Bc6afc0C5328bA36FaF03C514EF312287C";
/// The verified root of the 24 live blocks 970,543-970,566 under the v1 test
/// label set this driver uses (the website's v1 export; the site moved to
/// label set v2 later, whose root differs), checked when the node files are
/// present (M8-L55-F3).
const LIVE_ROOT: &str = "0x99789e60491178040f5353fb96909f76cba6b0d4eb627fadcc91c9ba86d0727e";
/// OKX over the same blocks by the independent Python recount
/// (scripts/recount-entity-flow.py): received, sent, net sats.
const LIVE_OKX: (u128, u128, i128) = (23_427_283_885, 34_226_666_074, -10_799_382_189);
/// Largest HTTP answer the loopback client reads, and its overall deadline
/// (M8-L55-F6).
const MAX_HTTP_BODY: usize = 8 << 20;
const HTTP_DEADLINE: Duration = Duration::from_secs(60);
/// How long the API's self-settle waits for its transaction to mine.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);
const METRIC: &str = "btc.entity-flow";
const SUBJECT: &str = "entity:okx";

// ------------------------------------------------------------------ options

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Local,
    Fork,
    Testnet,
}

struct Opts {
    mode: Mode,
    /// Actor keys (fork, testnet): created if missing, 0600, never printed.
    keys: Option<PathBuf>,
    /// forge's broadcast record of `DeployRevenue` (fork, testnet).
    revenue_broadcast: Option<PathBuf>,
    /// forge's broadcast record of `DeployRegistry` (ADR-0030): the driver
    /// registers the metric's recipe, attests each shard's preflight before
    /// opening it and checks the level a consumer verifies on chain.
    registry_broadcast: Option<PathBuf>,
    /// Print the funding plan and budget, send nothing.
    plan: bool,
    /// After the run, return what the actor keys hold to the deployer
    /// (fork, testnet; M8-L58-F8).
    sweep: bool,
    /// Append every transaction (label, hash) here (gitignored run log).
    log: Option<PathBuf>,
    rpc: String,
    /// anvil's `--config-out` file (local, fork); its directory is the work dir.
    accounts: Option<PathBuf>,
    /// The run's temp dir when there is no anvil (testnet).
    work: Option<PathBuf>,
    broadcast: PathBuf,
    live_dir: Option<PathBuf>,
    live_from: u64,
    live_to: u64,
    port: u16,
    serve: bool,
    dispute: bool,
    /// `--level2` (fork only; local mode always runs it): run the recorded
    /// Treasury level-2 round on the fork too, open its fact's revenue book
    /// from the reporter key before serving it (THREATS L2API-4, ROADMAP B1)
    /// and sell it with a self-settled payment. Testnet refuses it until
    /// level 2 is enabled on Base Sepolia (ROADMAP B2-B6).
    level2: bool,
    /// Write the run's report (steps, payouts, outcome) here as JSON; the
    /// website shows it (apps/web/static/data/live-run.json).
    json: Option<PathBuf>,
    /// The API's state on disk (REV-11): receipt logs, pending settles and
    /// the deployment manifest the client pins `payTo` from. Default: an
    /// `api-state` directory in the run's work dir.
    state_dir: Option<PathBuf>,
    /// Write what `onyx-node` needs to serve this run (deployment manifest,
    /// catalog, row store, a `node.env`) into this directory
    /// (docs/ops/deployment.md).
    export_node: Option<PathBuf>,
    /// Block files of the reserve tail (`fixtures-live/tail`): every mode
    /// settles one `btc-entity-reserve` shard on them (step 5b) and sells it
    /// through its serving check (step 8, ADR-0034 Amendment 1). Local mode
    /// skips it without the files; fork and testnet refuse to start.
    reserve_dir: Option<PathBuf>,
    /// With `--serve` and `--export-node`: settle this many more rounds on
    /// the deployment, one every `soak_every` seconds (scripts/soak.sh).
    soak_rounds: u64,
    soak_every: u64,
}

impl Opts {
    /// The API state directory (see [`Opts::state_dir`]).
    fn state_dir(&self) -> PathBuf {
        if let Some(d) = &self.state_dir {
            return d.clone();
        }
        let work = self
            .accounts
            .as_ref()
            .and_then(|a| a.parent().map(Path::to_path_buf))
            .or_else(|| self.work.clone())
            .unwrap_or_else(std::env::temp_dir);
        work.join("api-state")
    }

    /// The API state of one deployment (REV-L3-F4): a directory per
    /// `OnyxRevenue` (the local mock's address in local mode), so a new
    /// deployment never opens a journal of an old one (which its binding
    /// would refuse anyway).
    fn deploy_dir(&self, revenue: &[u8; 20]) -> PathBuf {
        self.state_dir()
            .join(format!("onyxrevenue-{}", hex::encode(revenue)))
    }
}

fn opts() -> Res<Opts> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    parse_opts(&args)
}

/// Flags that take a value, and flags that stand alone. Anything else is
/// refused, so a typo or an unknown flag (`--dry-run`, `--plna`) can never
/// fall through to a real run (M8-L58-F1).
const VALUE_FLAGS: [&str; 19] = [
    "--mode",
    "--keys",
    "--revenue-broadcast",
    "--registry-broadcast",
    "--log",
    "--rpc",
    "--accounts",
    "--work",
    "--broadcast",
    "--live-dir",
    "--live-from",
    "--live-to",
    "--port",
    "--json",
    "--state-dir",
    "--export-node",
    "--reserve-dir",
    "--soak-rounds",
    "--soak-every",
];
const BARE_FLAGS: [&str; 6] = [
    "--plan",
    "--serve",
    "--dispute",
    "--yes",
    "--sweep",
    "--level2",
];

fn parse_opts(args: &[String]) -> Res<Opts> {
    let mut vals: BTreeMap<&str, String> = BTreeMap::new();
    let mut bare: BTreeSet<&str> = BTreeSet::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(f) = VALUE_FLAGS.iter().find(|f| **f == a.as_str()) {
            let v = it.next().ok_or_else(|| format!("{f} needs a value"))?;
            if vals.insert(f, v.clone()).is_some() {
                return Err(format!("{f} given twice").into());
            }
        } else if let Some(f) = BARE_FLAGS.iter().find(|f| **f == a.as_str()) {
            bare.insert(f);
        } else {
            return Err(format!(
                "unknown argument {a:?}; flags: {} {}",
                VALUE_FLAGS.join(" "),
                BARE_FLAGS.join(" ")
            )
            .into());
        }
    }
    let val = |f: &str| vals.get(f).cloned();
    let num =
        |f: &str, d: u64| -> Res<u64> { Ok(val(f).map(|v| v.parse()).transpose()?.unwrap_or(d)) };
    let mode = match val("--mode").as_deref() {
        None | Some("local") => Mode::Local,
        Some("fork") => Mode::Fork,
        Some("testnet") => Mode::Testnet,
        Some(m) => return Err(format!("--mode {m}: want local, fork or testnet").into()),
    };
    let plan = bare.contains("--plan");
    // A real Base Sepolia run spends the deployer's test funds: it needs an
    // explicit `--yes` after the plan was printed (M8-L58-F1).
    if mode == Mode::Testnet && !plan && !bare.contains("--yes") {
        return Err(
            "testnet mode sends real (testnet) transactions: run with --plan first, then add --yes"
                .into(),
        );
    }
    // ADR-0027 amendment 8 item 8: level 2 stays off Base Sepolia until its
    // remaining items (ROADMAP B2-B5) are done; the fork rehearses it.
    if bare.contains("--level2") && mode == Mode::Testnet {
        return Err("--level2 is for fork mode: level 2 on Base Sepolia waits for ROADMAP B2-B6 (ADR-0027 amendment 8 item 8); rehearse it with `fork --level2`".into());
    }
    if bare.contains("--sweep") && mode == Mode::Local {
        return Err("--sweep is for fork and testnet modes".into());
    }
    let soak_rounds = num("--soak-rounds", 0)?;
    if soak_rounds > 0
        && (mode == Mode::Testnet || !bare.contains("--serve") || val("--export-node").is_none())
    {
        return Err(
            "--soak-rounds is for local and fork modes, with --serve and --export-node".into(),
        );
    }
    Ok(Opts {
        mode,
        keys: val("--keys").map(PathBuf::from),
        revenue_broadcast: val("--revenue-broadcast").map(PathBuf::from),
        registry_broadcast: val("--registry-broadcast").map(PathBuf::from),
        plan,
        sweep: bare.contains("--sweep"),
        log: val("--log").map(PathBuf::from),
        // The script passes a testnet node URL in ONYX_DEMO_RPC, never on the
        // command line (it can hold an API key; M8-L58-F7).
        rpc: val("--rpc")
            .or_else(|| std::env::var("ONYX_DEMO_RPC").ok())
            .unwrap_or_else(|| "http://127.0.0.1:8545".into()),
        accounts: val("--accounts").map(PathBuf::from),
        work: val("--work").map(PathBuf::from),
        broadcast: val("--broadcast")
            .ok_or("--broadcast <forge run-latest.json> is required")?
            .into(),
        live_dir: val("--live-dir").map(PathBuf::from),
        live_from: num("--live-from", 970_543)?,
        live_to: num("--live-to", 970_566)?,
        port: u16::try_from(num("--port", 8403)?)?,
        serve: bare.contains("--serve"),
        dispute: bare.contains("--dispute"),
        level2: bare.contains("--level2"),
        json: val("--json").map(PathBuf::from),
        state_dir: val("--state-dir").map(PathBuf::from),
        export_node: val("--export-node").map(PathBuf::from),
        reserve_dir: val("--reserve-dir").map(PathBuf::from),
        soak_rounds,
        soak_every: num("--soak-every", 600)?,
    })
}

// ------------------------------------------------------------------ loopback HTTP

/// Split `http://host:port/path` and refuse anything but this machine: this
/// transport is plain HTTP and exists only for the local node and server.
fn loopback(url: &str) -> Result<(String, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("{url}: only http:// to this machine"))?;
    let (authority, path) = rest
        .split_once('/')
        .map_or((rest, "/".to_string()), |(a, p)| (a, format!("/{p}")));
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
    if !matches!(host, "127.0.0.1" | "localhost" | "[::1]") || authority.contains('@') {
        return Err(format!("{url}: only http:// to this machine"));
    }
    Ok((authority.to_string(), path))
}

/// Status, headers (lower-case names) and body of an HTTP answer.
type Reply = (u16, Vec<(String, String)>, Vec<u8>);

/// One HTTP/1.1 exchange with `Connection: close`. Returns status, headers
/// (lower-case names) and body.
fn http(method: &str, url: &str, headers: &[(&str, String)], body: &[u8]) -> Result<Reply, String> {
    let (authority, path) = loopback(url)?;
    let addr = authority.replace("localhost", "127.0.0.1");
    let mut s = TcpStream::connect(&addr).map_err(|e| format!("{url}: {e}"))?;
    let deadline = Instant::now() + HTTP_DEADLINE;
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !body.is_empty() {
        req.push_str("Content-Type: application/json\r\n");
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes())
        .and_then(|()| s.write_all(body))
        .map_err(|e| e.to_string())?;
    // One deadline for the whole answer and a size cap, so a slow or endless
    // reply cannot hang the driver (M8-L55-F6).
    let mut raw = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| format!("{url}: no full answer within {HTTP_DEADLINE:?}"))?;
        s.set_read_timeout(Some(left)).map_err(|e| e.to_string())?;
        let n = s.read(&mut buf).map_err(|e| format!("{url}: {e}"))?;
        if n == 0 {
            break;
        }
        if raw.len() + n > MAX_HTTP_BODY {
            return Err(format!("{url}: answer over {MAX_HTTP_BODY} bytes"));
        }
        raw.extend_from_slice(&buf[..n]);
    }
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("no header end")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut payload = raw[split + 4..].to_vec();
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or("bad status line")?;
    let hs: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    if hs
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.eq_ignore_ascii_case("chunked"))
    {
        payload = dechunk(&payload)?;
    }
    Ok((status, hs, payload))
}

fn dechunk(mut b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let end = b.windows(2).position(|w| w == b"\r\n").ok_or("bad chunk")?;
        let size_text = String::from_utf8_lossy(&b[..end]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|e| e.to_string())?;
        b = &b[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if b.len() < size + 2 {
            return Err("short chunk".into());
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

/// The node transport: loopback HTTP (anvil) or HTTPS (Base Sepolia).
enum Post {
    Loop(LoopbackPost),
    Tls(HyperPost),
}

impl HttpPost for Post {
    fn post<'a>(
        &'a self,
        url: &'a str,
        body: Vec<u8>,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), String>> {
        match self {
            Post::Loop(p) => p.post(url, body),
            Post::Tls(p) => p.post(url, body),
        }
    }
}

fn node_post(mode: Mode) -> Post {
    match mode {
        Mode::Testnet => Post::Tls(HyperPost::for_rpc(Duration::from_secs(30))),
        Mode::Local | Mode::Fork => Post::Loop(LoopbackPost),
    }
}

/// The chain indexer's transport: `eth_getLogs` answers up to 16 MiB on a
/// public node (IDX-R1-2).
fn logs_post(mode: Mode) -> Post {
    match mode {
        Mode::Testnet => Post::Tls(HyperPost::for_logs(Duration::from_secs(30))),
        Mode::Local | Mode::Fork => Post::Loop(LoopbackPost),
    }
}

/// [`HttpPost`] over loopback HTTP, for `HttpJsonRpc` against the local node.
struct LoopbackPost;

impl HttpPost for LoopbackPost {
    fn post<'a>(
        &'a self,
        url: &'a str,
        body: Vec<u8>,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), String>> {
        let url = url.to_string();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || http("POST", &url, &[], &body))
                .await
                .map_err(|e| e.to_string())?
                .map(|(status, _, body)| (status, body))
        })
    }
}

// ------------------------------------------------------------------ chain via cast

/// The node, driven with `cast`. Local mode sends from anvil's unlocked dev
/// accounts; fork and testnet modes sign in this process with `keys` and
/// publish raw transactions (the keys never reach a command line).
struct Chain {
    cast: PathBuf,
    rpc: String,
    /// cast runs here, a temp dir outside the repo: Foundry loads a `.env`
    /// from its working directory and project root (M8-L55-F4).
    work: PathBuf,
    txs: usize,
    chain_id: u64,
    /// The node is an anvil (blocks can be mined on demand).
    anvil: bool,
    /// Signing keys by address (fork, testnet).
    keys: BTreeMap<[u8; 20], [u8; 32]>,
    /// Per sender: transactions, L2 gas used, wei paid on L2, L1 data fee.
    spent: BTreeMap<[u8; 20], Spend>,
    /// Per sender: the nonce after its last mined transaction. A load-balanced
    /// public node (sepolia.base.org) can answer the pending count from a node
    /// a block behind, which would reuse a nonce ("replacement transaction
    /// underpriced"), so the count only ever moves forward from here.
    next_nonce: BTreeMap<[u8; 20], u64>,
    /// The block of this run's last mined transaction: reads ask for at least
    /// this block, so a lagging node cannot answer from before our own write.
    seen_block: u64,
    /// (what, hash) of every transaction, for the run log.
    log: Vec<(String, String)>,
    /// Tests only: answers `cast` in place of a node, and waits are cut to
    /// a millisecond. `None` in every run.
    fake: Option<FakeNode>,
}

/// A stand-in for `cast` (tests of the send loop).
type FakeNode = Box<dyn Fn(&[String]) -> Res<String>>;

/// A node error that is the transport's, not the node's answer: asked
/// again. HTTP statuses are matched by their wording ("status 429",
/// "HTTP error 503", "Too Many Requests"), never as bare digits, which also
/// occur in revert data and hashes (review LAUNCH-R1-B2).
fn is_transport(e: &str) -> bool {
    let l = e.to_lowercase();
    const WORDS: [&str; 8] = [
        "error sending request",
        "timed out",
        "connection reset",
        "connection closed",
        "too many requests",
        "bad gateway",
        "service unavailable",
        "gateway timeout",
    ];
    WORDS.iter().any(|m| l.contains(m))
        || ["429", "502", "503", "504"].iter().any(|code| {
            [
                format!("status {code}"),
                format!("status: {code}"),
                format!("status code {code}"),
                format!("http {code}"),
                format!("http error {code}"),
                format!("http/1.1 {code}"),
                format!("http/2 {code}"),
            ]
            .iter()
            .any(|m| l.contains(m.as_str()))
        })
}

#[derive(Clone, Copy, Debug, Default)]
struct Spend {
    txs: u64,
    gas: u128,
    l2_wei: u128,
    l1_wei: u128,
}

fn hx(h: &Hash32) -> String {
    format!("0x{}", h.to_hex())
}

fn ax(a: &[u8; 20]) -> String {
    format!("0x{}", hex::encode(a))
}

fn parse_h32(s: &str) -> Res<Hash32> {
    let mut a = [0u8; 32];
    hex::decode_to_slice(s.trim().trim_matches('"').trim_start_matches("0x"), &mut a)?;
    Ok(Hash32(a))
}

fn parse_addr(s: &str) -> Res<[u8; 20]> {
    let mut a = [0u8; 20];
    hex::decode_to_slice(s.trim().trim_start_matches("0x"), &mut a)?;
    Ok(a)
}

/// First number of `cast` output such as `1000000 [1e6]`.
fn first_u128(s: &str) -> Res<u128> {
    Ok(s.split_whitespace().next().ok_or("empty")?.parse()?)
}

/// A JSON-RPC quantity (`"0x1a"`).
fn qty(v: &Value) -> Res<u128> {
    let t = v.as_str().ok_or_else(|| format!("not a quantity: {v}"))?;
    Ok(u128::from_str_radix(t.trim_start_matches("0x"), 16)?)
}

impl Chain {
    fn run(&self, args: &[String]) -> Res<String> {
        // A public node drops connections and rate-limits now and then; a
        // transport failure is asked again with the same arguments (a resent
        // raw transaction is the same bytes: "already known", or "nonce too
        // low" once mined, which send_signed resolves by its receipt).
        let mut tries = 0;
        loop {
            tries += 1;
            match self.run_once(args) {
                Err(e) if !self.anvil && tries < 6 && is_transport(&e.to_string()) => {
                    eprintln!("  node unreachable ({tries} of 5); retrying");
                    self.pause(Duration::from_secs(5 * tries));
                }
                r => return r,
            }
        }
    }

    /// Wait `d` (a millisecond under a [`FakeNode`]).
    fn pause(&self, d: Duration) {
        std::thread::sleep(if self.fake.is_some() {
            Duration::from_millis(1)
        } else {
            d
        });
    }

    fn run_once(&self, args: &[String]) -> Res<String> {
        if let Some(fake) = &self.fake {
            return fake(args);
        }
        // The node URL goes in the environment, not on the command line
        // (it can hold an API key); no key variable reaches cast (M8-L58-F7).
        let out = Command::new(&self.cast)
            .current_dir(&self.work)
            .args(args)
            .env("ETH_RPC_URL", &self.rpc)
            .env_remove("ONYX_DEPLOYER_KEY")
            .env_remove("BASE_SEPOLIA_PRIVATE_KEY")
            .output()?;
        if !out.status.success() {
            // Both streams: a revert's reason can be on either (M8-L55-F1).
            let text = [&out.stderr, &out.stdout]
                .iter()
                .map(|b| String::from_utf8_lossy(b).trim().to_string())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" | ")
                .replace(&self.rpc, &redact_url(&self.rpc));
            return Err(format!(
                "cast {}: {}",
                args.first().map_or("", String::as_str),
                if text.is_empty() {
                    "no output".into()
                } else {
                    text
                }
            )
            .into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// `cast` without a node (e.g. `calldata`).
    fn offline(&self, args: &[String]) -> Res<String> {
        let out = Command::new(&self.cast)
            .current_dir(&self.work)
            .args(args)
            .env_remove("ONYX_DEPLOYER_KEY")
            .env_remove("BASE_SEPOLIA_PRIVATE_KEY")
            .env_remove("ETH_RPC_URL")
            .output()?;
        if !out.status.success() {
            return Err(format!(
                "cast {}: {}",
                args.first().map_or("", String::as_str),
                String::from_utf8_lossy(&out.stderr).trim()
            )
            .into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// One JSON-RPC request through `cast rpc --raw`.
    fn rpc(&self, method: &str, params: Value) -> Res<Value> {
        let out = self.run(&[
            "rpc".into(),
            "--raw".into(),
            method.into(),
            params.to_string(),
        ])?;
        Ok(serde_json::from_str(&out)?)
    }

    /// Send a transaction; returns its hash. Signed here when `from` has a
    /// key, else from an unlocked (or impersonated) anvil account.
    fn send(&mut self, from: &[u8; 20], to: &[u8; 20], sig: &str, args: &[String]) -> Res<String> {
        if self.keys.contains_key(from) {
            let mut a = vec!["calldata".to_string(), sig.into()];
            a.extend(args.iter().cloned());
            let data = hex::decode(self.offline(&a)?.trim_start_matches("0x"))?;
            return self.send_signed(from, to, data, 0, sig);
        }
        self.send_unlocked(from, to, sig, args)
    }

    /// Send `wei` from a keyed account (funding).
    fn send_value(&mut self, from: &[u8; 20], to: &[u8; 20], wei: u128) -> Res<String> {
        self.send_signed(from, to, Vec::new(), wei, "transfer ETH")
    }

    /// Estimate, sign with `from`'s key, publish and wait for the receipt.
    fn send_signed(
        &mut self,
        from: &[u8; 20],
        to: &[u8; 20],
        data: Vec<u8>,
        value: u128,
        what: &str,
    ) -> Res<String> {
        let key = *self.keys.get(from).ok_or("no key for sender")?;
        let call = json!({"from": ax(from), "to": ax(to), "data": format!("0x{}", hex::encode(&data)),
                          "value": format!("0x{value:x}")});
        // A node behind the last mined transaction estimates against old
        // state (an approve it has not seen reverts the pull), so on a live
        // network a revert is asked again before it counts.
        // Like reads, the estimate names a block at or after this run's last
        // mined transaction: a node from before it prices a different path
        // (a withdraw of 0 after an unseen claim) and the send runs out of gas.
        let mut tries = 0;
        let gas = loop {
            tries += 1;
            let params = if self.anvil || self.seen_block == 0 {
                json!([call])
            } else {
                json!([call, format!("0x{:x}", self.block()?)])
            };
            match self.rpc("eth_estimateGas", params) {
                Ok(v) => break qty(&v)?,
                Err(e)
                    if !self.anvil && tries < 10 && {
                        let t = e.to_string().to_lowercase();
                        t.contains("revert")
                            || t.contains("not found")
                            || t.contains("unknown block")
                    } =>
                {
                    eprintln!("  {what}: estimate refused ({tries} of 9); asking again");
                    self.pause(Duration::from_secs(3));
                }
                Err(e) => return Err(format!("{what}: {e}").into()),
            }
        };
        // A stale node can hand back a used nonce; such a send is refused
        // before it reaches the pool, so wait for the nodes and try again.
        // Review LAUNCH-R1-B1: every version signed in this send is kept,
        // and before a new one is signed each is looked up: "nonce too low"
        // or "underpriced" can mean an earlier version landed (a timed-out
        // send that reached the pool), and a new nonce would send it twice.
        let mut signed: Vec<(String, u128, u64)> = Vec::new();
        let mut attempt = 0;
        let (hash, l1, nonce, receipt) = loop {
            attempt += 1;
            if let Some((h, r)) = self.receipt_of(&signed)? {
                let (_, l1, nonce) = signed.iter().find(|s| s.0 == h).cloned().ok_or("lost")?;
                eprintln!("  {what}: an earlier version {h} was mined; not sent again");
                break (h, l1, nonce, Some(r));
            }
            let pending: u64 =
                qty(&self.rpc("eth_getTransactionCount", json!([ax(from), "pending"]))?)?
                    .try_into()?;
            let nonce = pending.max(self.next_nonce.get(from).copied().unwrap_or(0));
            let base =
                qty(&self.rpc("eth_getBlockByNumber", json!(["latest", false]))?["baseFeePerGas"])?;
            let tip = self
                .rpc("eth_maxPriorityFeePerGas", json!([]))
                .ok()
                .and_then(|v| qty(&v).ok())
                .unwrap_or(1_000_000);
            let tx = Tx1559 {
                chain_id: self.chain_id,
                nonce,
                max_priority_fee_per_gas: tip,
                max_fee_per_gas: base * 2 + tip,
                gas: (gas * 5 / 4).try_into()?,
                to: *to,
                value,
                data: data.clone(),
            };
            let (raw, hash) = tx.sign(&key).ok_or("signing failed")?;
            let raw_hex = format!("0x{}", hex::encode(&raw));
            let hash = format!("0x{}", hex::encode(hash));
            let l1 = self.l1_fee(&raw_hex);
            if !signed.iter().any(|s| s.0 == hash) {
                signed.push((hash.clone(), l1, nonce));
            }
            match self.rpc("eth_sendRawTransaction", json!([raw_hex])) {
                Ok(_) => break (hash, l1, nonce, None),
                // The same signed bytes are already in the pool: wait for them.
                Err(e) if e.to_string().contains("already known") => break (hash, l1, nonce, None),
                Err(e)
                    if !self.anvil
                        && attempt < 6
                        && ["underpriced", "nonce too low", "nonce too high"]
                            .iter()
                            .any(|m| e.to_string().contains(m)) =>
                {
                    eprintln!("  {what}: {e}; retrying ({attempt} of 5)");
                    // Up to 8 s for a version already sent to show its
                    // receipt (a lagging node) before a new one is signed.
                    for _ in 0..8 {
                        if self.receipt_of(&signed)?.is_some() {
                            break;
                        }
                        self.pause(Duration::from_secs(1));
                    }
                }
                Err(e) if self.receipt_of(&signed)?.is_some() => {
                    eprintln!("  {what}: {e}, but an earlier version was mined");
                }
                Err(e) => return Err(format!("{what}: {e}").into()),
            }
        };
        // Any version signed in this send may be the one mined.
        let (hash, l1, nonce, receipt) = match receipt {
            Some(r) => (hash, l1, nonce, r),
            None => loop {
                if let Some((h, r)) = self.receipt_of(&signed)? {
                    let (_, l1, nonce) = signed.iter().find(|s| s.0 == h).cloned().ok_or("lost")?;
                    break (h, l1, nonce, r);
                }
                self.pause(Duration::from_millis(if self.anvil { 50 } else { 500 }));
            },
        };
        self.next_nonce.insert(*from, nonce + 1);
        if let Ok(b) = qty(&receipt["blockNumber"]) {
            self.seen_block = self.seen_block.max(b.try_into().unwrap_or(0));
        }
        if receipt["status"] != "0x1" {
            return Err(format!("{what}: transaction {hash} reverted").into());
        }
        let used = qty(&receipt["gasUsed"])?;
        let price = qty(&receipt["effectiveGasPrice"])?;
        // A real OP Stack receipt carries its L1 fee; an anvil fork charges
        // none, so the oracle's price for the same bytes stands in.
        let l1 = receipt.get("l1Fee").and_then(|v| qty(v).ok()).unwrap_or(l1);
        let sp = self.spent.entry(*from).or_default();
        sp.txs += 1;
        sp.gas += used;
        sp.l2_wei += used * price;
        sp.l1_wei += l1;
        self.txs += 1;
        self.log.push((what.to_string(), hash.clone()));
        Ok(hash)
    }

    /// The first of `signed` (hash, L1 fee, nonce) with a receipt.
    fn receipt_of(&self, signed: &[(String, u128, u64)]) -> Res<Option<(String, Value)>> {
        for (hash, _, _) in signed {
            let r = self.rpc("eth_getTransactionReceipt", json!([hash]))?;
            if !r.is_null() {
                return Ok(Some((hash.clone(), r)));
            }
        }
        Ok(None)
    }

    /// L1 data fee of `raw` by the OP Stack oracle (0 where there is none).
    fn l1_fee(&self, raw_hex: &str) -> u128 {
        self.run(&[
            "call".into(),
            GAS_PRICE_ORACLE.into(),
            "getL1Fee(bytes)(uint256)".into(),
            raw_hex.into(),
        ])
        .ok()
        .and_then(|o| first_u128(&o).ok())
        .unwrap_or(0)
    }

    /// Send from an unlocked anvil account; returns its hash.
    fn send_unlocked(
        &mut self,
        from: &[u8; 20],
        to: &[u8; 20],
        sig: &str,
        args: &[String],
    ) -> Res<String> {
        let mut a = vec![
            "send".to_string(),
            "--unlocked".into(),
            "--from".into(),
            ax(from),
            "--json".into(),
            ax(to),
            sig.into(),
        ];
        a.extend(args.iter().cloned());
        let out = self.run(&a).map_err(|e| format!("{sig}: {e}"))?;
        let receipt: Value = serde_json::from_str(&out)?;
        if receipt["status"] != "0x1" {
            return Err(format!("{sig}: transaction failed").into());
        }
        self.txs += 1;
        let hash = receipt["transactionHash"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        self.log.push((sig.to_string(), hash.clone()));
        Ok(hash)
    }

    fn call(&self, to: &[u8; 20], sig: &str, args: &[String]) -> Res<String> {
        let mut a = vec!["call".to_string(), ax(to), sig.into()];
        a.extend(args.iter().cloned());
        self.read(a)
    }

    /// A `cast call` that never reads state older than this run's last mined
    /// transaction: on a live network it names a block at or after
    /// `seen_block`, and a node that does not have that block yet is asked
    /// again rather than answering from the past.
    fn read(&self, mut a: Vec<String>) -> Res<String> {
        if self.anvil || self.seen_block == 0 {
            return self.run(&a);
        }
        a.push("--block".into());
        a.push(String::new());
        let mut tries = 0;
        loop {
            tries += 1;
            let at = self.block()?;
            if let Some(last) = a.last_mut() {
                *last = at.to_string();
            }
            match self.run(&a) {
                Err(e)
                    if tries < 10
                        && ["not found", "unknown block", "header", "future block"]
                            .iter()
                            .any(|m| e.to_string().to_lowercase().contains(m)) =>
                {
                    std::thread::sleep(Duration::from_secs(2));
                }
                r => return r,
            }
        }
    }

    /// A read-only `eth_call` as `from` (nothing is sent).
    fn call_from(&self, from: &[u8; 20], to: &[u8; 20], sig: &str, args: &[String]) -> Res<String> {
        let mut a = vec![
            "call".to_string(),
            "--from".into(),
            ax(from),
            ax(to),
            sig.into(),
        ];
        a.extend(args.iter().cloned());
        self.read(a)
    }

    /// The head, never before this run's last mined transaction.
    fn block(&self) -> Res<u64> {
        let head: u64 = self.run(&["block-number".into()])?.parse()?;
        Ok(head.max(self.seen_block))
    }

    fn block_hash(&self, n: u64) -> Res<Hash32> {
        parse_h32(&self.run(&[
            "block".into(),
            n.to_string(),
            "--field".into(),
            "hash".into(),
        ])?)
    }

    /// Make the next transaction land in block `target` or later: mine
    /// empty blocks on an anvil, wait for real ones elsewhere.
    fn reach(&self, target: u64) -> Res<()> {
        loop {
            let latest = self.block()?;
            if latest + 1 >= target {
                return Ok(());
            }
            if self.anvil {
                let n = target - 1 - latest;
                self.run(&["rpc".into(), "anvil_mine".into(), format!("0x{n:x}")])?;
                return Ok(());
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }
}

// ------------------------------------------------------------------ data

/// A shard to settle: its spec, input bytes and everything a rerun needs.
/// The adapter's valid recipe failures (SER-R2-F2).
fn allowed_failures() -> onyx_coordinator::recompute::FailureAllowList {
    onyx_source_btc::valid_failure_codes().into_iter().collect()
}

struct Data {
    /// The API metric (and on-chain metric) the shard is served under.
    metric: &'static str,
    /// The recipe's registered floor (0: none; ADR-0030 REG-F).
    floor: u8,
    reg: onyx_core::StaticRegistry,
    cas: Arc<MemCas>,
    host: Host,
    spec: ShardSpec,
    source: Vec<u8>,
    blocks: u64,
    label: String,
    /// The end block's 80-byte header (the one the snapshot's tip hash
    /// commits to), which `--export-node` hands onyx-node so it reads the
    /// data time itself (AUD-2-F2, AUD-2R-F2).
    end_header: Vec<u8>,
}

/// The first 80 bytes of a block or header given as hex.
fn header_bytes(hex_header: &str) -> Res<Vec<u8>> {
    Ok(hex::decode(
        hex_header.get(..160).ok_or("header hex too short")?,
    )?)
}

/// The 24 live blocks from the node files when `live` names a directory
/// that has them, else the committed 3-block fixture.
/// `zero_base` adds explicit zero starting balances for every labelled
/// entity: the same flows (plus end balances), but a different shard id, so a
/// second round on the same blocks is a new shard (M8-L55-F1). Only used on
/// the committed fixture, where no labelled entity's net flow is negative
/// (OKX +148,783,915 sats, Bitwise 0), so the recipe accepts a zero base.
fn data(live: Option<(&Path, u64, u64)>, zero_base: bool) -> Res<Data> {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let fixtures = root.join("fixtures");
    let reg = fixture_registry();
    let labels = load_label_set(&fixtures, "labels/test.onyx.entities.btc.v1.json")?;
    let wasm = std::fs::read(root.join("recipes/out/btc_entity_balance.wasm"))?;
    let live = live.filter(|(dir, from, _)| dir.join(format!("{from}.raw.hex")).exists());
    let (source, snapshot, blocks, label, end_hex) = match live {
        Some((dir, from, to)) => {
            let pin = NodePin {
                min_bits: MAINNET_MIN_BITS,
                tip: None,
            };
            let (source, snap) = load_node_range(dir, from, to, &pin)?;
            let end = std::fs::read_to_string(dir.join(format!("{to}.raw.hex")))?;
            (
                source,
                snap,
                to - from + 1,
                format!("live blocks {from}-{to}"),
                end.trim().to_string(),
            )
        }
        None => {
            let adapter = BtcFixtureAdapter::new(&fixtures, "btc/blocks-921080-921082.json");
            let snap = adapter.range_snapshot()?;
            let source = adapter.load(&snap)?;
            let f: Value = serde_json::from_str(&std::fs::read_to_string(
                fixtures.join("btc/blocks-921080-921082.json"),
            )?)?;
            let end = f["blocks"]
                .as_array()
                .and_then(|b| b.last())
                .and_then(|b| b["header"].as_str())
                .ok_or("fixture end block has no header")?;
            (
                source,
                snap,
                3,
                "fixture blocks 921080-921082".to_string(),
                end.to_string(),
            )
        }
    };
    let params = if zero_base {
        let entities: std::collections::BTreeSet<&str> =
            labels.labels.iter().map(|l| l.entity.as_str()).collect();
        borsh::to_vec(&BalanceParams {
            base: entities
                .into_iter()
                .map(|e| EntityBase {
                    entity: e.into(),
                    balance_sats: 0,
                })
                .collect(),
        })?
    } else {
        Vec::new()
    };
    let label = if zero_base {
        format!("{label}, zero starting balances")
    } else {
        label
    };
    let cas = Arc::new(MemCas::new());
    cas.put(Kind::Recipe, &wasm)?;
    cas.put_label_set(&labels)?;
    cas.put(Kind::Blob, &[])?;
    let spec = ShardSpec {
        recipe: Some(keccak256(&wasm)),
        runtime: Some(RuntimeProfile {
            abi_version: 1,
            host: HOST_ID.into(),
            fuel_limit: 4_000_000_000,
            max_memory_bytes: 256 << 20,
            max_output_bytes: 1 << 20,
            max_call_depth: 512,
        }),
        label_set: Some(LabelSetRef {
            name: labels.name.clone(),
            version: labels.version,
            content_hash: labels.content_hash(),
        }),
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot,
        },
        schema: keccak256(b"onyx.btc.entity-flow-row.v1"),
        rights_hash: reg
            .source_rights(&SourceId(SOURCE_ID.into()))
            .ok_or("source not registered")?
            .hash(),
        params,
        registry_root: reg.root(),
    };
    Ok(Data {
        metric: METRIC,
        floor: 0,
        reg,
        cas,
        host: Host::new()?,
        spec,
        source,
        blocks,
        label,
        end_header: header_bytes(&end_hex)?,
    })
}

// ------------------------------------------------------------------ setup

struct Deployed {
    usdc: [u8; 20],
    settlement: [u8; 20],
    /// The block `OnyxSettlement` was deployed in: where the chain indexer
    /// starts (ADR-0033).
    settlement_block: u64,
    disputes: [u8; 20],
    /// `OnyxRegistry` (ADR-0030), when `--registry-broadcast` was given.
    registry: Option<[u8; 20]>,
}

fn deployed(path: &Path) -> Res<Deployed> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let find = |name: &str| -> Res<[u8; 20]> {
        let tx = v["transactions"]
            .as_array()
            .ok_or("no transactions")?
            .iter()
            .find(|t| t["contractName"] == name && t["transactionType"] == "CREATE")
            .ok_or_else(|| format!("{name} not deployed"))?;
        parse_addr(tx["contractAddress"].as_str().ok_or("no address")?)
    };
    let settlement = find("OnyxSettlement")?;
    // The deploy receipt's block (forge writes it as a hex quantity).
    let settlement_block = v["receipts"]
        .as_array()
        .ok_or("no receipts in the broadcast")?
        .iter()
        .find(|r| {
            r["contractAddress"].as_str().map(str::to_ascii_lowercase) == Some(ax(&settlement))
        })
        .and_then(|r| match &r["blockNumber"] {
            Value::String(h) => u64::from_str_radix(h.trim_start_matches("0x"), 16).ok(),
            n => n.as_u64(),
        })
        .ok_or("no deploy receipt for OnyxSettlement in the broadcast")?;
    Ok(Deployed {
        usdc: find("MockUSDC")?,
        settlement,
        settlement_block,
        disputes: find("OnyxDisputes")?,
        registry: None,
    })
}

/// anvil's dev accounts: address and key (public test keys).
fn accounts(path: &Path) -> Res<Vec<([u8; 20], [u8; 32])>> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let addrs = v["available_accounts"].as_array().ok_or("no accounts")?;
    let keys = v["private_keys"].as_array().ok_or("no keys")?;
    addrs
        .iter()
        .zip(keys)
        .map(|(a, k)| -> Res<_> {
            Ok((
                parse_addr(a.as_str().ok_or("address")?)?,
                parse_h32(k.as_str().ok_or("key")?)?.0,
            ))
        })
        .collect()
}

fn cfg_tuple(c: &RoundConfig) -> String {
    let v = &c.validation;
    let d = &c.dispute;
    format!(
        "({},{},{},{},{},{},({},{},{},{},{},{},{},{},{},{}),({},{},{},{},{},{},{},{},{}))",
        c.commit_close,
        c.reveal_close,
        c.min_bond,
        hx(&c.validator_set_root),
        hx(&c.canary_commit),
        c.beacon_height,
        v.verdict_commit_close,
        v.verdict_reveal_close,
        v.canary_open_close,
        v.pending_close,
        v.rate_ppm,
        v.miner_reward,
        v.validator_fee,
        v.validator_slash,
        v.missed_slash,
        v.min_canary_ppm,
        d.challenge_window,
        d.min_challenge_bond,
        d.panel_size,
        d.panel_commit_window,
        d.panel_reveal_window,
        d.appeal_window,
        d.max_appeals,
        d.panel_fee,
        d.panel_slash,
    )
}

const OPEN_ROUND: &str = "openRound(uint64,(uint64,uint64,uint128,bytes32,bytes32,uint64,(uint64,uint64,uint64,uint64,uint64,uint128,uint128,uint128,uint128,uint64),(uint64,uint128,uint32,uint64,uint64,uint64,uint32,uint128,uint128)),bytes32[],(bytes32,bytes32,bytes32)[])";
const REVEAL: &str =
    "reveal(uint64,bytes32,address,(bytes32,uint64,bytes32,bytes32,bytes32,bytes32,bytes32,bytes32),bytes32)";
const SHARD_STATUS: &str = "shardStatus(uint64,bytes32)(uint8)";
/// `OnyxLedger.ShardStatus`.
const PENDING: u8 = 2;
const FINALIZED: u8 = 3;

/// Round timing relative to `base` (the block before `openRound`), and the
/// economics, in mock USDC units. Every window passes `roundConfigValid`.
fn round_cfg(base: u64, canary_salt: &Hash32) -> Res<RoundConfig> {
    Ok(RoundConfig {
        commit_close: base + 40,
        reveal_close: base + 70,
        min_bond: BOND,
        validator_set_root: Hash32::ZERO,
        canary_commit: canary_commit(&[], canary_salt).ok_or("canary list")?,
        beacon_height: base + 70,
        // Wide enough for the drawn validators sending one after another on
        // Base Sepolia (about 9 blocks each through a public node): the
        // 80-block verdict window closed after 6 of them (NS17, 2026-10-27).
        validation: ValidationConfig {
            verdict_commit_close: base + 330,
            verdict_reveal_close: base + 560,
            canary_open_close: base + 560,
            pending_close: base + 660,
            rate_ppm: RATE_PPM,
            miner_reward: USDC,
            validator_fee: 50_000,
            validator_slash: 5 * USDC,
            missed_slash: 500_000,
            min_canary_ppm: 50_000,
        },
        // Panel windows fit 3 votes sent one after another on Base Sepolia
        // (about 9 blocks each); the challenge window must outlast a panel.
        dispute: DisputeConfig {
            challenge_window: 200,
            min_challenge_bond: 2 * USDC,
            panel_size: 3,
            panel_commit_window: 60,
            panel_reveal_window: 60,
            appeal_window: 60,
            max_appeals: 1,
            panel_fee: 100_000,
            panel_slash: USDC,
        },
    })
}

fn usd(units: u128) -> String {
    format!("{}.{:06}", units / USDC, units % USDC)
}

fn btc(sats: i128) -> String {
    let sign = if sats < 0 { "-" } else { "" };
    let a = sats.unsigned_abs();
    format!("{sign}{}.{:08}", a / 100_000_000, a % 100_000_000)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn hex_bytes(v: &Value) -> Res<Vec<u8>> {
    Ok(hex::decode(
        v.as_str().ok_or("not a string")?.trim_start_matches("0x"),
    )?)
}

// ------------------------------------------------------------------ rounds

/// Everyone on the local chain.
struct Net {
    chain: Chain,
    dep: Deployed,
    coord: [u8; 20],
    miners: Vec<Miner>,
    validators: Vec<Validator>,
    /// Steps for the `--json` report: transactions and seconds since the
    /// previous mark, and the block reached.
    trace: Vec<Value>,
    mark_txs: usize,
    mark_t: Instant,
}

/// An assigned task and its verdict: (validator index, miner, verdict).
type Task = (usize, [u8; 20], Verdict);

/// What a round left behind, for the summary.
struct Played {
    cfg: RoundConfig,
    shard: Hash32,
    open_tx: String,
    seed: Hash32,
    /// (validator, miner, verdict) for every assigned task.
    verdicts: Vec<Task>,
    recipe_secs: f64,
    judge_secs: f64,
}

impl Net {
    /// Close a report step: what happened on chain since the last mark.
    fn mark(&mut self, step: &str) -> Res<()> {
        let block = self.chain.block()?;
        let secs = self.mark_t.elapsed().as_secs_f64();
        self.trace.push(json!({
            "step": step,
            "txs": self.chain.txs - self.mark_txs,
            "secs": (secs * 10.0).round() / 10.0,
            "block": block,
        }));
        self.mark_txs = self.chain.txs;
        self.mark_t = Instant::now();
        Ok(())
    }

    fn st(&self) -> [u8; 20] {
        self.dep.settlement
    }

    /// What `a` can withdraw from the settlement now.
    fn credit(&self, a: &[u8; 20]) -> Res<u128> {
        first_u128(
            &self
                .chain
                .call(&self.st(), "balanceOf(address)(uint256)", &[ax(a)])?,
        )
    }

    /// Register every floored metric (`METRIC_MIN_LEVELS`) with its recipe
    /// AND its floor in one owner call (REG-F-F1/F3), then show the floor on
    /// chain: as the coordinator, a dry-run `attestShard` of a pool shard
    /// below the floor is refused (`BelowMetricLevel`) and one at the floor
    /// passes. Nothing is attested: both are `eth_call`s.
    fn register_floored_metrics(
        &mut self,
        reg: &[u8; 20],
        owner: &[u8; 20],
        coord: &[u8; 20],
        round: u64,
        runtime: &[u8],
    ) -> Res<()> {
        for (metric, floor) in METRIC_MIN_LEVELS {
            let mk = metric_key(&MetricId(metric.into()));
            let recipe = floored_recipe_id(metric)?;
            self.chain.send(
                owner,
                reg,
                "registerMetric(bytes32,uint8,bytes32,uint8)",
                &[hx(&mk), "1".into(), hx(&recipe), floor.number().to_string()],
            )?;
            let on_chain =
                self.chain
                    .call(reg, "recipeMinLevel(bytes32)(uint8)", &[hx(&recipe)])?;
            if on_chain.trim() != floor.number().to_string() {
                return Err(format!("{metric}: floor on chain is {on_chain}").into());
            }
            let tuple = format!(
                "({},{},{},{},{},{},{},{})",
                hx(&recipe),
                hx(&keccak256(runtime)),
                hx(&Hash32::ZERO),
                hx(&keccak256(b"demo floor check: source")),
                hx(&keccak256(b"demo floor check: schema")),
                hx(&keccak256(b"demo floor check: rights")),
                hx(&keccak256(b"demo floor check: params")),
                hx(&keccak256(b"demo floor check: registry")),
            );
            let sig = "attestShard(uint64,(bytes32,bytes32,bytes32,bytes32,bytes32,bytes32,bytes32,bytes32),bytes,bytes32,uint64,uint8)";
            let args = |level: u8| {
                vec![
                    round.to_string(),
                    tuple.clone(),
                    format!("0x{}", hex::encode(runtime)),
                    hx(&mk),
                    "1".to_string(),
                    level.to_string(),
                ]
            };
            let below = format!(
                "0x{}",
                hex::encode(&keccak256(b"BelowMetricLevel()").0[..4])
            );
            match self.chain.call_from(coord, reg, sig, &args(1)) {
                Err(e)
                    if e.to_string().contains(&below)
                        || e.to_string().contains("BelowMetricLevel") => {}
                Err(e) => {
                    return Err(format!(
                        "{metric}: below-floor attest failed for another reason: {e}"
                    )
                    .into())
                }
                Ok(_) => {
                    return Err(
                        format!("{metric}: an attestation at level 1 was not refused").into(),
                    )
                }
            }
            self.chain
                .call_from(coord, reg, sig, &args(floor.number()))
                .map_err(|e| format!("{metric}: an attestation at the floor was refused: {e}"))?;
            println!(
                "  registry: {metric} registered with its recipe and floor {} in one call; on chain a level-1 attestation is refused (BelowMetricLevel), level {} passes",
                floor.number(),
                floor.number()
            );
        }
        Ok(())
    }

    fn send_st(&mut self, from: &[u8; 20], sig: &str, args: &[String]) -> Res<String> {
        let st = self.st();
        self.chain.send(from, &st, sig, args)
    }

    fn status(&self, round: u64, shard: &Hash32) -> Res<u8> {
        Ok(self
            .chain
            .call(&self.st(), SHARD_STATUS, &[round.to_string(), hx(shard)])?
            .parse()?)
    }

    /// Token, bond, reward pool and staked validators.
    fn setup(&mut self) -> Res<()> {
        let (coord, usdc, st) = (self.coord, self.dep.usdc, self.st());
        let u = |n: u128| n.to_string();
        let max = "115792089237316195423570985008687907853269984665640564039457584007913129639935";
        let c = &mut self.chain;
        c.send(
            &coord,
            &usdc,
            "mint(address,uint256)",
            &[ax(&coord), u(1_000 * USDC)],
        )?;
        c.send(
            &coord,
            &usdc,
            "approve(address,uint256)",
            &[ax(&st), max.into()],
        )?;
        c.send(&coord, &st, "postCoordinatorBond(uint128)", &[u(STAKE)])?;
        c.send(&coord, &st, "fundRewards(uint256)", &[u(10 * USDC)])?;
        for m in &self.miners {
            c.send(
                &coord,
                &usdc,
                "mint(address,uint256)",
                &[ax(&m.address), u(100 * USDC)],
            )?;
            c.send(
                &m.address,
                &usdc,
                "approve(address,uint256)",
                &[ax(&st), max.into()],
            )?;
        }
        for v in &self.validators {
            c.send(
                &coord,
                &usdc,
                "mint(address,uint256)",
                &[ax(&v.address), u(STAKE)],
            )?;
            c.send(
                &v.address,
                &usdc,
                "approve(address,uint256)",
                &[ax(&st), max.into()],
            )?;
            c.send(&v.address, &st, "stake(uint128)", &[u(STAKE)])?;
        }
        Ok(())
    }

    /// Open `round` with the shard and check that Rust derives the
    /// contract's round tag. The coordinator runs the shard's preflight
    /// first (SER-R1-F2, SER-R2-F1): `openRound` on chain trusts the
    /// coordinator, so this driver refuses a shard over half its fuel limit
    /// or with a failure the schema does not list before it sends anything.
    fn open(
        &mut self,
        round: u64,
        d: &Data,
        canary_salt: &Hash32,
    ) -> Res<(RoundConfig, Hash32, String)> {
        let fuel = onyx_coordinator::recompute::preflight(
            &d.spec,
            d.source.clone(),
            &d.reg,
            &*d.cas,
            &d.host,
            &onyx_labels::attest::AttestCache::new(),
            &allowed_failures(),
        )
        .map_err(|e| format!("shard preflight refused: {e}"))?;
        let limit = d.spec.runtime.as_ref().map_or(0, |r| r.fuel_limit);
        println!("  preflight: {fuel} of {limit} fuel (at most half allowed)");
        let shard = d.spec.shard_id();
        let coord = self.coord;
        if let Some(reg) = self.dep.registry {
            // ADR-0030: the metric's one recipe and the label set's level are
            // registered by the registry's owner, a key that is never the
            // coordinator (REG-R1-F3); the coordinator then attests its
            // preflight with the shard-id PREIMAGE, from which the chain
            // recomputes the shard id and checks the recipe and the label
            // floor (REG-R1-F1). A later attestation is refused on chain.
            let mk = metric_key(&MetricId(d.metric.into()));
            let recipe = d.spec.recipe.ok_or("the shard has no recipe")?;
            let pre = ShardPreimage::of(&d.spec);
            let runtime = runtime_bytes(d.spec.runtime.as_ref().ok_or("the shard has no runtime")?);
            if pre.shard_id() != shard {
                return Err("the shard preimage does not give the shard id".into());
            }
            let owner = parse_addr(self.chain.call(&reg, "owner()(address)", &[])?.trim())?;
            if owner == coord {
                return Err("the registry's owner is the coordinator (REG-R1-F3)".into());
            }
            let level = shard_level(d)?;
            let kind =
                self.chain
                    .call(&reg, "metrics(bytes32)(uint8,bytes32,uint64)", &[hx(&mk)])?;
            let fresh = kind.lines().next().map(str::trim) == Some("0");
            if fresh {
                // The metric and its recipe's floor in ONE call (REG-F-F1);
                // the entity-flow metric has no floor of its own (its label
                // set floors it); the reserve metric's is its base's level.
                self.chain.send(
                    &owner,
                    &reg,
                    "registerMetric(bytes32,uint8,bytes32,uint8)",
                    &[hx(&mk), "1".into(), hx(&recipe), d.floor.to_string()],
                )?;
                if d.metric == METRIC {
                    self.register_floored_metrics(&reg, &owner, &coord, round, &runtime)?;
                }
            }
            let ls = pre.label_set_hash;
            if ls != Hash32::ZERO {
                let have = self
                    .chain
                    .call(&reg, "labelSetLevel(bytes32)(uint8)", &[hx(&ls)])?;
                if have.trim() == "0" {
                    // The label set's own level is the shard's floor.
                    let fixtures =
                        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures"));
                    let ll = label_set_proof_level(
                        &fixtures,
                        d.spec.label_set.as_ref().ok_or("labels")?,
                    );
                    self.chain.send(
                        &owner,
                        &reg,
                        "registerLabelSet(bytes32,uint8)",
                        &[hx(&ls), ll.number().to_string()],
                    )?;
                }
            }
            let tuple = format!(
                "({},{},{},{},{},{},{},{})",
                hx(&pre.recipe_id),
                hx(&pre.runtime_hash),
                hx(&pre.label_set_hash),
                hx(&pre.source_hash),
                hx(&pre.schema_id),
                hx(&pre.rights_hash),
                hx(&pre.params_hash),
                hx(&pre.registry_root)
            );
            self.chain.send(
                &coord,
                &reg,
                "attestShard(uint64,(bytes32,bytes32,bytes32,bytes32,bytes32,bytes32,bytes32,bytes32),bytes,bytes32,uint64,uint8)",
                &[
                    round.to_string(),
                    tuple,
                    // REG-R2-F1: the runtime bytes; the chain reads the fuel
                    // limit out of them and checks they hash to the preimage.
                    format!("0x{}", hex::encode(&runtime)),
                    hx(&mk),
                    fuel.to_string(),
                    level.number().to_string(),
                ],
            )?;
            println!(
                "  registry: {}preflight and level {} attested on chain before the open (shard id recomputed from its preimage)",
                if fresh {
                    format!(
                        "recipe registered for {} by the owner (floor {}); ",
                        d.metric, d.floor
                    )
                } else {
                    String::new()
                },
                level.number()
            );
        }
        let mut cfg = round_cfg(self.chain.block()?, canary_salt)?;
        let tx = self.send_st(
            &coord,
            OPEN_ROUND,
            &[
                round.to_string(),
                cfg_tuple(&cfg),
                format!("[{}]", hx(&shard)),
                format!(
                    "[({},{},{})]",
                    hx(&d.spec.fields_hash()),
                    hx(&keccak256(&d.spec.params)),
                    hx(&d.spec.registry_root)
                ),
            ],
        )?;
        let st = self.st();
        let r = round.to_string();
        let tag = parse_h32(&self.chain.call(
            &st,
            "roundTag(uint64)(bytes32)",
            std::slice::from_ref(&r),
        )?)?;
        // The contract writes the validator set root itself (from its registry).
        cfg.validator_set_root = parse_h32(&self.chain.call(
            &st,
            "roundSetRoot(uint64)(bytes32)",
            &[r],
        )?)?;
        let domain = self.chain.call(&st, "domain()(string)", &[])?;
        let ledger = parse_h32(&self.chain.call(&st, "ledgerId()(bytes32)", &[])?)?;
        if ledger != evm_ledger_id(self.chain.chain_id, &st) {
            return Err("ledger id differs between Rust and the contract".into());
        }
        if round_tag(domain.trim_matches('"'), &ledger, round, &cfg, &[shard]) != tag {
            return Err("round tag differs between Rust and the contract".into());
        }
        Ok((cfg, tag, tx))
    }

    /// Commit every work (bonded, signed over the tag), then reveal it.
    fn commit_reveal(
        &mut self,
        round: u64,
        cfg: &RoundConfig,
        tag: &Hash32,
        works: &[Work],
        cas: &MemCas,
    ) -> Res<()> {
        for (i, w) in works.iter().enumerate() {
            let m = self.miners[i].address;
            let c = self.miners[i].sign_commit(w, BOND, tag);
            self.send_st(
                &m,
                "commit(uint64,bytes32,address,bytes32,uint128,bytes)",
                &[
                    round.to_string(),
                    hx(&w.shard),
                    ax(&m),
                    hx(&c.commit_hash),
                    BOND.to_string(),
                    format!("0x{}", hex::encode(c.sig)),
                ],
            )?;
        }
        self.chain.reach(cfg.commit_close)?;
        for (i, w) in works.iter().enumerate() {
            let m = self.miners[i].address;
            self.miners[i].publish_rows(w, cas)?;
            let f = w.commitment.fields();
            self.send_st(
                &m,
                REVEAL,
                &[
                    round.to_string(),
                    hx(&w.shard),
                    ax(&m),
                    format!(
                        "({},{},{},{},{},{},{},{})",
                        hx(&f.root),
                        f.leaf_count,
                        hx(&f.recipe_id),
                        hx(&f.runtime_hash),
                        hx(&f.label_set_hash),
                        hx(&f.source_hash),
                        hx(&f.schema_id),
                        hx(&f.rights_hash)
                    ),
                    hx(&w.salt),
                ],
            )?;
        }
        Ok(())
    }

    /// Open the (empty) canary list, draw the seed (checked against Rust),
    /// and have every assigned validator rerun, commit and reveal a verdict.
    fn validate(
        &mut self,
        round: u64,
        cfg: &RoundConfig,
        tag: &Hash32,
        canary_salt: &Hash32,
        works: &[Work],
        d: &Data,
    ) -> Res<(Hash32, Vec<Task>, f64)> {
        let (coord, st, r) = (self.coord, self.st(), round.to_string());
        self.chain.reach(cfg.reveal_close)?;
        self.send_st(
            &coord,
            "openCanaries(uint64,(bytes32,address,bytes32)[],bytes32)",
            &[r.clone(), "[]".into(), hx(canary_salt)],
        )?;
        self.chain.reach(cfg.beacon_height + 1)?;
        self.send_st(&coord, "drawSeed(uint64)", std::slice::from_ref(&r))?;
        let seed = parse_h32(&self.chain.call(
            &st,
            "seedOf(uint64)(bytes32)",
            std::slice::from_ref(&r),
        )?)?;
        if sample_seed(tag, &self.chain.block_hash(cfg.beacon_height)?) != seed {
            return Err("seed differs between Rust and the contract".into());
        }
        let shard = d.spec.shard_id();
        let mut verdicts = Vec::new();
        let t = Instant::now();
        for vi in 0..self.validators.len() {
            for (mi, w) in works.iter().enumerate() {
                let (v, m) = (&self.validators[vi], self.miners[mi].address);
                if !assigned(&seed, &v.address, &shard, &m, RATE_PPM) {
                    continue;
                }
                let verdict = v.judge(
                    &d.spec,
                    d.source.clone(),
                    &w.commitment.root,
                    &d.reg,
                    &*d.cas,
                    &d.host,
                );
                let c = v.commit(&seed, &shard, &m, &verdict);
                let from = v.address;
                self.send_st(
                    &from,
                    "commitVerdict(uint64,bytes32,address,bytes32)",
                    &[r.clone(), hx(&shard), ax(&m), hx(&c.commit)],
                )?;
                verdicts.push((vi, m, verdict));
            }
        }
        let judge_secs = t.elapsed().as_secs_f64();
        self.chain.reach(cfg.validation.verdict_commit_close)?;
        for (vi, m, verdict) in &verdicts {
            let (kind, vroot) = match verdict {
                Verdict::Agree => (0u8, Hash32::ZERO),
                Verdict::Disagree { root } => (1, *root),
                Verdict::Abstain => (2, Hash32::ZERO),
                Verdict::NoValidRoot => (3, Hash32::ZERO),
            };
            let v = &self.validators[*vi];
            let (from, salt) = (v.address, v.salt(&seed, &shard, m));
            self.send_st(
                &from,
                "revealVerdict(uint64,bytes32,address,uint8,bytes32,bytes32)",
                &[
                    r.clone(),
                    hx(&shard),
                    ax(m),
                    kind.to_string(),
                    hx(&vroot),
                    hx(&salt),
                ],
            )?;
        }
        Ok((seed, verdicts, judge_secs))
    }

    /// One whole round up to `finalizeShard`: open, commit/reveal, validate,
    /// finalize. `works` are the miners' (possibly dishonest) works.
    fn play(&mut self, round: u64, d: &Data, works: &[Work], recipe_secs: f64) -> Res<Played> {
        let canary_salt = keccak256(format!("demo-local canary salt {round}").as_bytes());
        let (cfg, tag, open_tx) = self.open(round, d, &canary_salt)?;
        self.mark(&format!("r{round}.open"))?;
        self.commit_reveal(round, &cfg, &tag, works, &d.cas)?;
        self.mark(&format!("r{round}.commit_reveal"))?;
        let (seed, verdicts, judge_secs) =
            self.validate(round, &cfg, &tag, &canary_salt, works, d)?;
        self.mark(&format!("r{round}.verdicts"))?;
        self.chain.reach(cfg.validation.verdict_reveal_close)?;
        let coord = self.coord;
        self.send_st(
            &coord,
            "finalizeShard(uint64,bytes32)",
            &[round.to_string(), hx(&d.spec.shard_id())],
        )?;
        self.mark(&format!("r{round}.finalize"))?;
        Ok(Played {
            cfg,
            shard: d.spec.shard_id(),
            open_tx,
            seed,
            verdicts,
            recipe_secs,
            judge_secs,
        })
    }

    /// Withdraw everyone's credit; returns what each address received.
    fn withdraw_all(&mut self, who: &[[u8; 20]]) -> Res<Vec<u128>> {
        let (usdc, st) = (self.dep.usdc, self.st());
        let mut got = Vec::new();
        for a in who {
            let bal = |c: &Chain| -> Res<u128> {
                first_u128(&c.call(&usdc, "balanceOf(address)(uint256)", &[ax(a)])?)
            };
            let before = bal(&self.chain)?;
            let credit = first_u128(&self.chain.call(
                &st,
                "balanceOf(address)(uint256)",
                &[ax(a)],
            )?)?;
            if credit > 0 {
                self.chain.send(a, &st, "withdraw()", &[])?;
            }
            got.push(bal(&self.chain)?.saturating_sub(before));
        }
        Ok(got)
    }
}

/// Every miner runs the recipe; returns the works and seconds per run.
fn mine(miners: &[Miner], round: u64, d: &Data) -> Res<(Vec<Work>, f64)> {
    let t = Instant::now();
    let works: Vec<Work> = miners
        .iter()
        .map(|m| m.work(round, &d.spec, d.source.clone(), &d.reg, &*d.cas, &d.host))
        .collect::<Result<_, _>>()?;
    Ok((works, t.elapsed().as_secs_f64() / miners.len() as f64))
}

/// `w` with its root replaced (a miner that reveals a made-up result): same
/// shard fields, so the contract accepts the reveal, and a fresh commit hash.
fn forged(w: &Work, miner: &[u8; 20]) -> Work {
    let mut f = w.clone();
    f.commitment.root = keccak256(b"a made-up root");
    f.commit_hash = onyx_core::commit_hash(
        f.round,
        &f.shard,
        &f.commitment.binding_hash(),
        miner,
        &f.salt,
    );
    f
}

// ------------------------------------------------------------------ serve and check

/// The running server, the report lines, OKX's checked row, the report and
/// (fork, testnet) the self-settling facilitator, whose key the reporter shares.
type Served = (
    tokio::task::JoinHandle<std::io::Result<()>>,
    Vec<String>,
    EntityFlowRow,
    Value,
    Option<Arc<SelfSettle<HttpJsonRpc<Post>>>>,
);

/// Self-settled payments (fork, testnet): x402 pays `OnyxRevenue` in real
/// USDC and the API settles itself (ADR-0026).
struct Paid {
    revenue: [u8; 20],
    usdc: [u8; 20],
    /// The customer.
    payer_key: [u8; 32],
    /// `OnyxRevenue`'s reporter: the API's own key, set with `setReporter`
    /// by the deployer (the owner) and funded with gas only (M8-L58-F5).
    operator_key: [u8; 32],
    /// `OnyxRegistry`'s owner: never the coordinator (REG-R1-F3); registers
    /// the metric's recipe and the label set's level, funded with gas only.
    registry_owner_key: [u8; 32],
    /// The level-2 receipt signer (ROADMAP B3, THREATS REVLOG-4): a hot key
    /// of its own per `OnyxRegistry` deployment, never the deployer
    /// (coordinator), the reporter, the registry owner or any other actor;
    /// `None` without a registry.
    receipt_key: Option<ReceiptKey>,
}

/// Wait until the node's `finalized` block shows the shard's root (anvil:
/// mine past it; Base Sepolia: L2 finality, about a quarter hour).
async fn wait_finalized(net: &Net, o: &Opts, round: u64, shard: &Hash32) -> Res<u64> {
    let rpc = HttpJsonRpc::new(o.rpc.clone(), node_post(o.mode));
    let t = Instant::now();
    loop {
        let ret = rpc
            .call(
                "eth_call",
                json!([{"to": ax(&net.st()),
                        "data": format!("0x{}", hex::encode(get_finalized_root_calldata(round, shard)))},
                       "finalized"]),
            )
            .await?;
        // Empty return data: the contract itself is not in the finalized
        // block yet (deployed within the last finality window), so not yet.
        let data = hex_bytes(&ret)?;
        if !data.is_empty() && decode_finalized_root(&data)?.is_some() {
            return Ok(t.elapsed().as_secs());
        }
        if net.chain.anvil {
            let b = net.chain.block()?;
            net.chain.reach(b + 66)?;
        } else {
            if t.elapsed() > Duration::from_secs(3_600) {
                return Err("the root did not reach the finalized block within an hour".into());
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    }
}

/// The demo's catalog: the served shard (with its leaf count), the OKX market
/// and the metric's price.
/// The shard's proof level: its weakest input (L2R2-F3), the rerun (level 1)
/// and the label set's level. Attested on chain before the open (ADR-0030).
/// The committed build of a floored metric's recipe (REG-F-F3).
fn floored_recipe_id(metric: &str) -> Res<Hash32> {
    let name = match metric {
        METRIC_POOL_BLOCKS => "btc_pool_blocks",
        onyx_source_btc::METRIC_ENTITY_RESERVE => "btc_entity_reserve",
        _ => return Err(format!("no recipe known for floored metric {metric}").into()),
    };
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    Ok(keccak256(&std::fs::read(
        root.join(format!("recipes/out/{name}.wasm")),
    )?))
}

fn shard_level(d: &Data) -> Res<ProofLevel> {
    let fixtures = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures"));
    let labels = label_set_proof_level(&fixtures, d.spec.label_set.as_ref().ok_or("labels")?);
    // An input the rerun does not check (a reserve shard's base) floors it too.
    let input = onyx_source_btc::input_level(&d.spec).unwrap_or(ProofLevel::Recomputed);
    Ok(ProofLevel::Recomputed.weakest(labels).weakest(input))
}

/// The shard as the operator holds it (spec, rights, row count, the label
/// set's level), before any round: the chain indexer gives it a round once
/// `OnyxSettlement` finalized it (ADR-0033).
fn demo_meta(d: &Data, round: u64, leaf_count: u64) -> Res<ShardMeta> {
    let rights = d
        .reg
        .source_rights(&SourceId(SOURCE_ID.into()))
        .ok_or("source not registered")?
        .clone();
    // L2R2-F3: the shard is as strong as its weakest input; the reviewed test
    // label set is level 4, so the shard is level 4 (rows are still served).
    let fixtures = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures"));
    let label_level = label_set_proof_level(&fixtures, d.spec.label_set.as_ref().ok_or("labels")?);
    // The in-process demo keeps the finalization price clock (no
    // `with_snapshot_unix`), so its payment step still sells the fixture's
    // old blocks; `--export-node` writes `snapshotUnix` and onyx-node prices
    // from the data's time (AUD-2-F2).
    let mut meta = ShardMeta::from_spec(round, &d.spec, METRIC, rights)
        .ok_or("rights do not match the spec")?
        .with_label_set_level(label_level);
    meta.leaf_count = Some(leaf_count);
    Ok(meta)
}

/// What the operator holds for the chain indexer: the round-1 shard only.
fn demo_held(d: &Data, leaf_count: u64) -> Res<Holdings> {
    let mut held = Holdings::new();
    held.insert(HeldShard {
        meta: demo_meta(d, 0, leaf_count)?,
        label_set: d.spec.label_set.clone(),
    })?;
    Ok(held)
}

/// The chain indexer over this run's settlement contract (ADR-0033), its
/// cursor in the API state directory.
fn demo_indexer(
    net: &Net,
    o: &Opts,
    d: &Data,
    leaf_count: u64,
    path: PathBuf,
) -> Res<Indexer<HttpJsonRpc<Post>>> {
    let cfg = IndexerConfig {
        // The demo shard pins the v1 test label set (dev mode).
        allow_test_labels: true,
        registry: net.dep.registry,
        ..IndexerConfig::new(net.chain.chain_id, net.st(), net.dep.settlement_block)
    };
    Ok(Indexer::open(
        cfg,
        HttpJsonRpc::new(o.rpc.clone(), logs_post(o.mode)),
        demo_held(d, leaf_count)?,
        d.cas.clone() as Arc<dyn Cas + Send + Sync>,
        Some(path),
    )?)
}

/// The indexer's cursor file for this run's API state.
fn indexer_path(o: &Opts, paid: Option<&Paid>) -> Res<PathBuf> {
    let mock_pay_to = parse_addr(DEMO_PAY_TO)?;
    Ok(o.deploy_dir(paid.map_or(&mock_pay_to, |p| &p.revenue))
        .join("indexer.json"))
}

/// Markets and prices; the shard itself comes from the indexer.
fn demo_catalog_base(d: &Data) -> Catalog {
    let shard = d.spec.shard_id();
    let mut catalog = Catalog::new();
    catalog.add_market(
        "okx-net-flow-demo",
        Market {
            round: ROUND,
            shard,
            subject: SUBJECT.into(),
        },
    );
    catalog.set_pricing(
        METRIC,
        PricingPolicy {
            base_price_micro_usdc: PRICE_MICRO_USDC,
            fresh_window_secs: 86_400,
        },
    );
    catalog
}

/// The demo's catalog with the shard listed directly (the receipt path's
/// second API, which runs after the indexer found the shard).
fn demo_catalog(d: &Data, leaf_count: u64) -> Res<Catalog> {
    let mut catalog = demo_catalog_base(d);
    catalog.add_shard(demo_meta(d, ROUND, leaf_count)?);
    Ok(catalog)
}

/// The stub the local mode's mock view holds at the mock `payTo`.
const MOCK_PAY_TO_CODE: &[u8] = b"\x60\x80\x60\x40\x52";

/// Local mode only: a view of Base Sepolia holding [`MOCK_PAY_TO_CODE`] at the
/// mock facilitator's `payTo`, so the client's pin runs its checks (chain id,
/// code hash) the same way as on a real node.
struct MockPayToView;

impl JsonRpc for MockPayToView {
    fn call<'a>(&'a self, method: &'a str, _: Value) -> BoxFuture<'a, Result<Value, String>> {
        let r = match method {
            "eth_chainId" => Ok(json!("0x14a34")),
            "eth_getCode" => Ok(json!(format!("0x{}", hex::encode(MOCK_PAY_TO_CODE)))),
            m => Err(format!("mock view: {m}")),
        };
        Box::pin(async move { r })
    }
}

/// The deployment manifest `--export-node` writes (crates/onyx-node
/// manifest.rs): the settlement with its runtime code hash and deploy block
/// (where the chain indexer starts, ADR-0033) and, when the run deployed
/// `OnyxRegistry`, the registry pin (address and runtime code hash, L16), so
/// the node floors every shard at the level the chain attested for it.
fn node_manifest(
    chain_id: u64,
    st: &[u8; 20],
    settlement_code: &Hash32,
    deploy_block: u64,
    registry: Option<([u8; 20], Hash32)>,
) -> Value {
    let mut m = json!({
        "chainId": chain_id,
        "settlement": ax(st),
        "settlementCodeHash": hx(settlement_code),
        "deployBlock": deploy_block,
    });
    if let Some((reg, code)) = registry {
        m["registry"] = json!(ax(&reg));
        m["registryCodeHash"] = json!(hx(&code));
    }
    m
}

/// L16-1: the code at the registry address must be this checkout's
/// `OnyxRegistry` build (forge's `deployedBytecode`, with only its
/// immutables, `owner` and `ledger`, filled in), before the export pins its
/// hash. The hash is read from the chain, so without this a proxy or other
/// bytecode at the broadcast's address would be pinned as it is.
fn registry_matches_build(code: &[u8], artifact: &Value) -> Res<()> {
    let db = &artifact["deployedBytecode"];
    let obj = db["object"]
        .as_str()
        .ok_or("OnyxRegistry artifact: no deployedBytecode")?;
    let want = hex::decode(obj.strip_prefix("0x").unwrap_or(obj))?;
    if want.is_empty() || code.len() != want.len() {
        return Err(format!(
            "the registry's runtime code is {} bytes, the OnyxRegistry build {}",
            code.len(),
            want.len()
        )
        .into());
    }
    let refs = db["immutableReferences"]
        .as_object()
        .ok_or("OnyxRegistry artifact: no immutableReferences")?;
    let mut masked = code.to_vec();
    for r in refs
        .values()
        .flat_map(|v| v.as_array().into_iter().flatten())
    {
        let (Some(start), Some(len)) = (r["start"].as_u64(), r["length"].as_u64()) else {
            return Err("OnyxRegistry artifact: a bad immutable reference".into());
        };
        let (start, end) = (start as usize, (start + len) as usize);
        if end > want.len() || start > end {
            return Err("OnyxRegistry artifact: an immutable reference out of range".into());
        }
        masked[start..end].copy_from_slice(&want[start..end]);
    }
    if masked != want {
        return Err("the registry's runtime code is not this checkout's OnyxRegistry build".into());
    }
    Ok(())
}

/// The forge artifact of this checkout's `OnyxRegistry`, found from the
/// registry's broadcast (`contracts/broadcast/<script>/<chain>/run-latest.json`).
/// Review L16-R3: no broadcast is an error, never a skipped build check (the
/// registry address comes only from the broadcast today; a later source of
/// it must not pin unchecked code).
fn registry_artifact(broadcast: Option<&std::path::Path>) -> Res<Value> {
    let b = broadcast.ok_or(
        "a registry with no --registry-broadcast: its build cannot be checked, so it is not pinned",
    )?;
    let contracts = b
        .ancestors()
        .nth(4)
        .ok_or("--registry-broadcast is not under contracts/broadcast")?;
    Ok(serde_json::from_str(&std::fs::read_to_string(
        contracts.join("out/OnyxRegistry.sol/OnyxRegistry.json"),
    )?)?)
}

/// x402 terms: Base Sepolia test USDC to `pay_to`.
/// Everything `onyx-node` needs to serve this run (`--export-node`): the
/// deployment manifest (settlement, the registry when deployed and, when
/// paid, OnyxRevenue, each with its runtime code hash, [`node_manifest`]),
/// the catalog file, the row batch in a directory
/// store, and a `node.env` with the non-secret settings.
#[allow(clippy::too_many_arguments)]
fn export_node(
    dir: &Path,
    o: &Opts,
    d: &Data,
    root: &Hash32,
    leaf_count: u64,
    chain_id: u64,
    st: &[u8; 20],
    deploy_block: u64,
    settlement_code: &Hash32,
    registry: Option<([u8; 20], Hash32)>,
    paid: Option<&Paid>,
) -> Res<()> {
    use onyx_node::catalog_file::{CatalogFile, HeldEntry, MarketEntry, Pricing, CATALOG_VERSION};
    std::fs::create_dir_all(dir)?;
    let mut manifest = node_manifest(chain_id, st, settlement_code, deploy_block, registry);
    if let Some(p) = paid {
        let m: Value = serde_json::from_str(&std::fs::read_to_string(
            o.deploy_dir(&p.revenue).join("deployment.json"),
        )?)?;
        for k in ["revenue", "revenueCodeHash", "usdc"] {
            manifest[k] = m[k].clone();
        }
    }
    std::fs::write(
        dir.join("deployment.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;
    let rights = d
        .reg
        .source_rights(&SourceId(SOURCE_ID.into()))
        .ok_or("source not registered")?
        .clone();
    // The shard is held, not listed: onyx-node's indexer serves it once it
    // finds the Finalized event and the rows hash to the contract's root.
    let file = CatalogFile {
        version: CATALOG_VERSION,
        shards: vec![],
        held: vec![HeldEntry::new(
            METRIC,
            &d.spec,
            &rights,
            Some(leaf_count),
            Some(&d.end_header),
        )?],
        markets: vec![MarketEntry {
            id: "okx-net-flow-demo".into(),
            round: ROUND,
            shard: hx(&d.spec.shard_id()),
            subject: SUBJECT.into(),
        }],
        default_pricing: None,
        pricing: BTreeMap::from([(
            METRIC.to_string(),
            Pricing {
                base_price_micro_usdc: PRICE_MICRO_USDC,
                fresh_window_secs: 86_400,
            },
        )]),
        default_dataset_pricing: None,
        dataset_pricing: BTreeMap::new(),
    };
    // The file must build the catalog and holdings the run serves.
    file.to_catalog()?;
    if file.holdings_at(now_unix())?.len() != 1 {
        return Err("the exported catalog does not hold the shard".into());
    }
    std::fs::write(
        dir.join("catalog.json"),
        serde_json::to_string_pretty(&file)? + "\n",
    )?;
    let rows = d
        .cas
        .get(Kind::Rows, root)
        .ok_or("the served row batch is not in the store")?;
    let store = onyx_coordinator::cas::FsCas::new(dir.join("cas"));
    store.put(Kind::Rows, &rows)?;
    // onyx-node-btc writes every spec's params into the row store at start
    // for its serving check; a read-only mount (Docker, deployment.md) needs
    // them there already.
    for p in file.params_blobs()? {
        store.put(Kind::Blob, &p)?;
    }
    let local_rpc = o.rpc.starts_with("http://127.0.0.1") || o.rpc.starts_with("http://localhost");
    let env_name = if chain_id == 31_337 { "dev" } else { "testnet" };
    let payments = if paid.is_some() { "self-settle" } else { "off" };
    // A remote node URL can carry an API key: never written here. The
    // service reads it from a credential file (ONYX_RPC_URL_FILE, OPS-L1-F4).
    let rpc_line = if local_rpc {
        format!("ONYX_RPC_URL={}", o.rpc)
    } else {
        "# ONYX_RPC_URL=<set it yourself, or ONYX_RPC_URL_FILE; never commit a URL with an API key>"
            .into()
    };
    // Review L14-R1: a testnet node must name ONYX_RIGHTS. The export says
    // fallback (no label-derived metric served) until the label-set rights
    // are cleared in writing (launch-checklist LC-1); a dev node keeps the
    // default.
    // Comments on their own line: an EnvironmentFile= or --env-file value
    // keeps everything after the `=`.
    let rights_line: Vec<String> = if env_name == "testnet" {
        vec![
            "# LC-1: ONYX_RIGHTS=cleared only once the label-set rights are cleared in writing"
                .into(),
            "ONYX_RIGHTS=fallback".into(),
        ]
    } else {
        Vec::new()
    };
    // node.env: run onyx-node by hand from this directory.
    let mut env = vec![
        format!("ONYX_ENV={env_name}"),
        format!("ONYX_LISTEN=127.0.0.1:{}", o.port + 1),
        format!("ONYX_PUBLIC_URL=http://127.0.0.1:{}", o.port + 1),
        format!("ONYX_DEPLOYMENT={}", dir.join("deployment.json").display()),
        format!("ONYX_CATALOG={}", dir.join("catalog.json").display()),
        format!("ONYX_CAS_DIR={}", dir.join("cas").display()),
        format!("ONYX_STATE_DIR={}", dir.join("state").display()),
        // The demo shard pins the v1 test label set.
        "ONYX_ALLOW_TEST_LABELS=1".into(),
        // The served shards come from the chain (ADR-0033).
        "ONYX_INDEXER=on".into(),
        format!("ONYX_PAYMENTS={payments}"),
        rpc_line.clone(),
    ];
    env.extend(rights_line.clone());
    if paid.is_some() {
        if let Some(k) = &o.keys {
            env.push(format!(
                "ONYX_OPERATOR_KEY_FILE={}",
                k.join("operator.key").display()
            ));
        }
    }
    write_private(&dir.join("node.env"), &(env.join("\n") + "\n"))?;
    // node.service.env: for /etc/onyx/node.env under the systemd unit or
    // Docker. No state, row store or key paths: the unit and the docker
    // command set those, and a value here would override them (OPS-L1-F5).
    let mut service = vec![
        format!("ONYX_ENV={env_name}"),
        "ONYX_LISTEN=127.0.0.1:8080".into(),
        "# ONYX_PUBLIC_URL=https://<your host> (http:// only to localhost)".into(),
        "ONYX_DEPLOYMENT=/etc/onyx/deployment.json".into(),
        "ONYX_CATALOG=/etc/onyx/catalog.json".into(),
        "ONYX_ALLOW_TEST_LABELS=1".into(),
        "ONYX_INDEXER=on".into(),
        format!("ONYX_PAYMENTS={payments}"),
        rpc_line,
    ];
    service.extend(rights_line);
    write_private(&dir.join("node.service.env"), &(service.join("\n") + "\n"))?;
    println!("  exported onyx-node inputs to {}", dir.display());
    Ok(())
}

/// Write `text` to `path` readable by the owner only (0600 on Unix): an env
/// file may later gain a node URL with an API key (OPS-L1-F4).
fn write_private(path: &Path, text: &str) -> Res<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(text.as_bytes())?;
    Ok(())
}

fn demo_terms(pay_to: String) -> PaymentTerms {
    PaymentTerms {
        network: BASE_SEPOLIA.into(),
        asset: USDC_BASE_SEPOLIA.into(),
        pay_to,
        max_timeout_seconds: 60,
        extra: Some(json!({"name": "USDC", "version": "2"})),
    }
}

/// Serve the finalized shard from the chain and check one row as a client:
/// x402 payment, `ClientNode::verify` (the default check) against the client's own
/// `getFinalizedRoot`, and the contract's `verifyRow`.
async fn serve_and_check(
    net: &Net,
    o: &Opts,
    d: &Data,
    root: &Hash32,
    leaf_count: u64,
    paid: Option<&Paid>,
    l2: Option<(ObservationFeed, Vec<String>)>,
) -> Res<Served> {
    let st = net.st();
    let chain_id = net.chain.chain_id;
    let rpc = || HttpJsonRpc::new(o.rpc.clone(), node_post(o.mode));
    // The operator pins the code hash recorded at deploy time (here read
    // with cast at the latest block, so this pin cannot fail; on Base
    // Sepolia pin it from the deploy output); ChainRoots then checks the code
    // at the finalized block against it (M8-L55-F6).
    let pin = parse_h32(&net.chain.run(&["codehash".into(), ax(&st)])?)?;
    let roots = Arc::new(ChainRoots::connect(st, chain_id, Some(pin), rpc()).await?);
    let shard = d.spec.shard_id();
    // ADR-0033: the API is not handed its shard; the chain indexer finds it.
    let mut catalog = demo_catalog_base(d);
    let l2_feed = l2.as_ref().map(|(f, _)| f.clone());
    if let Some(feed) = &l2_feed {
        catalog.add_observations(feed.clone())?;
        // The recorded round keeps its template-fixed decay start (the price clock);
        // the rehearsal window is stretched so it stays a paid sale whenever
        // the demo runs (L2API-R1-F4).
        let settled = feed
            .records
            .values()
            .map(|r| r.checked.settled_unix)
            .max()
            .unwrap_or_default();
        catalog.set_pricing(
            onyx_source_fiscaldata::METRIC,
            PricingPolicy {
                base_price_micro_usdc: 2_500,
                fresh_window_secs: onyx_source_fiscaldata::rehearsal_fresh_window_secs(
                    settled,
                    now_unix(),
                ),
            },
        );
    }
    if let Some(dir) = &o.export_node {
        // L16 (THREATS L2R2-F2): pin the registry in the export. Its code must
        // be this checkout's build (L16-1) and pass the node's own start check
        // (code hash, and `ledger()` is this settlement) before it is written.
        let registry = match net.dep.registry {
            Some(reg) => {
                let code = hex::decode(
                    net.chain
                        .run(&["code".into(), ax(&reg)])?
                        .trim()
                        .trim_start_matches("0x"),
                )?;
                registry_matches_build(
                    &code,
                    &registry_artifact(o.registry_broadcast.as_deref())?,
                )?;
                let code_hash = keccak256(&code);
                onyx_api::levels::check_registry(&rpc(), reg, code_hash, st).await?;
                Some((reg, code_hash))
            }
            None => None,
        };
        export_node(
            dir,
            o,
            d,
            root,
            leaf_count,
            chain_id,
            &st,
            net.dep.settlement_block,
            &pin,
            registry,
            paid,
        )?;
    }
    let payer_key = match paid {
        Some(p) => p.payer_key,
        None => keccak256(DEMO_PAYER_SEED.as_bytes()).0,
    };
    let payer = ax(&exact_evm::address(&payer_key).ok_or("payer key")?);
    let mock_pay_to = parse_addr(DEMO_PAY_TO)?;
    let sd = o.deploy_dir(paid.map_or(&mock_pay_to, |p| &p.revenue));
    std::fs::create_dir_all(&sd)?;
    // Local: a mock facilitator moves nothing. Fork/testnet: the API settles
    // the payment itself into OnyxRevenue and credits the shard (ADR-0026);
    // its receipt log and pending settles are kept on disk (REV-11).
    let (pay_to, facilitator, settle): (String, Arc<dyn Facilitator>, _) = match paid {
        None => (
            DEMO_PAY_TO.to_string(),
            Arc::new(MockFacilitator::new([payer])),
            None,
        ),
        Some(p) => {
            let sender = Sender::new(rpc(), chain_id, p.operator_key).ok_or("operator key")?;
            let f = Arc::new(
                SelfSettle::new(
                    sender,
                    p.revenue,
                    p.usdc,
                    BASE_SEPOLIA,
                    // Base Sepolia mines every 2 s. The server waits this plus
                    // the settle's node calls (`settle_budget`), which stays
                    // under the client's HTTP deadline (M8-L58-F2).
                    SETTLE_DEADLINE,
                )
                .persist(sd.join("self-settle"))?,
            );
            (ax(&p.revenue), f.clone() as Arc<dyn Facilitator>, Some(f))
        }
    };
    // THREATS L2API-4 (ROADMAP B1): every served level-2 fact's revenue book
    // is opened from the reporter key (the API's operator key, one nonce
    // sequence with its settles) BEFORE the API serves level 2. Fail closed:
    // a book that cannot be opened stops the run here, since its paid
    // requests would answer 503 (`settle` reverts `NotRegistered`). Opening
    // reads the book first, so running it again sends nothing.
    let mut book_lines = Vec::new();
    if let (Some(p), Some(f), Some(feed)) = (paid, &settle, &l2_feed) {
        let facts: Vec<Hash32> = feed
            .records
            .values()
            .map(|r| r.checked.registry_fact)
            .collect();
        let sender = f.shared_sender();
        let opened = open_observation_books(&sender, &p.revenue, &facts, SETTLE_DEADLINE)
            .await
            .map_err(|e| format!("level 2 is not served: {e}"))?;
        let again = open_observation_books(&sender, &p.revenue, &facts, SETTLE_DEADLINE)
            .await
            .map_err(|e| format!("level 2 is not served: {e}"))?;
        if again.iter().any(|(_, o)| *o != Opened::AlreadyOpen) {
            return Err("opening the observation books again sent a transaction".into());
        }
        for (fact, o) in &opened {
            book_lines.push(match o {
                Opened::Now { tx, block } => format!(
                    "   OnyxRevenue: the reporter opened the observation book of fact {}... before serving it (registerObservation tx {tx}, block {block}); opening again: already open, nothing sent",
                    &hx(fact)[..10]
                ),
                Opened::AlreadyOpen => format!(
                    "   OnyxRevenue: the observation book of fact {}... was already open; nothing sent",
                    &hx(fact)[..10]
                ),
            });
        }
    }
    let mut state = ApiState::new(
        catalog,
        roots,
        d.cas.clone() as Arc<dyn Cas + Send + Sync>,
        Some(Payments {
            terms: demo_terms(pay_to),
            facilitator,
        }),
        Arc::new(now_unix),
        format!("http://127.0.0.1:{}", o.port),
        ApiConfig {
            allow_test_labels: true,
            ..ApiConfig::default()
        },
    )?;
    // The chain indexer (ADR-0033): scan Finalized events from the deploy
    // block to the finalized block, check the contract's view and the held
    // rows, and only then put the shard in the API's catalog. A fresh cursor:
    // this run deployed new contracts.
    let idx_path = indexer_path(o, paid)?;
    let _ = std::fs::remove_file(&idx_path);
    let idx = Arc::new(demo_indexer(net, o, d, leaf_count, idx_path)?);
    // IDX-R1-4: the deploy block is where the settlement's code appeared.
    idx.check_deploy_block().await?;
    if state
        .catalog
        .read()
        .map_err(|_| "catalog lock")?
        .shard(ROUND, &shard)
        .is_some()
    {
        return Err("the API was handed its shard before the indexer ran".into());
    }
    let t_idx = Instant::now();
    let (mut ranges, mut splits, mut scanned) = (0, 0, None);
    loop {
        match idx.poll(&state.catalog).await {
            Ok(r) => {
                ranges += r.ranges;
                splits += r.splits;
                scanned = r
                    .scanned
                    .map(|(a, b)| (scanned.map_or(a, |(s, _)| s), b))
                    .or(scanned);
                if r.admitted.contains(&(ROUND, shard)) {
                    break;
                }
            }
            Err(e) if t_idx.elapsed() < Duration::from_secs(600) => {
                println!("   indexer: {e}; trying again");
            }
            Err(e) => return Err(format!("indexer: {e}").into()),
        }
        if t_idx.elapsed() > Duration::from_secs(600) {
            return Err(format!("the indexer did not admit the shard: {:?}", idx.known()).into());
        }
        tokio::time::sleep(Duration::from_millis(if net.chain.anvil {
            200
        } else {
            10_000
        }))
        .await;
    }
    let ev = idx
        .events()
        .into_iter()
        .find(|e| (e.round, e.shard) == (ROUND, shard))
        .ok_or("no event for the admitted shard")?;
    let admitted = state
        .catalog
        .read()
        .map_err(|_| "catalog lock")?
        .shard(ROUND, &shard)
        .cloned()
        .ok_or("the indexer did not fill the catalog")?;
    if admitted.proof_level != shard_level(d)? || admitted.leaf_count != Some(leaf_count) {
        return Err(format!(
            "the indexed shard has level {} and {:?} rows, want level {} and {leaf_count}",
            admitted.proof_level.number(),
            admitted.leaf_count,
            shard_level(d)?.number()
        )
        .into());
    }
    let (from, to) = scanned.unwrap_or((net.dep.settlement_block, net.dep.settlement_block));
    println!(
        "6a. indexer: read blocks {from}..{to} (deploy to finalized) in {ranges} eth_getLogs calls ({splits} split); Finalized(round {ROUND}, shard {}...) at block {}: the contract's getFinalizedRoot and the held rows agree, so the API serves it (level {})",
        &hx(&shard)[..10],
        ev.block,
        admitted.proof_level.number()
    );
    state = state.with_indexer(idx.clone() as Arc<dyn IndexerHealth>);
    match &settle {
        // Payments that could not settle here go to the reporter's queue,
        // which the self-settle keeps on disk.
        Some(f) => state.receipts = f.receipts(),
        None => {
            state = state.with_receipt_log(
                sd.join("receipts.jsonl"),
                &Binding {
                    chain_id,
                    contract: mock_pay_to,
                },
            )?
        }
    }
    // The client pins the contract it pays (REV-4): from the deployment
    // manifest (fork, testnet: written right after the deploy, with the
    // runtime code hash its own node must show), or the local mock's address.
    let pay_pin = match paid {
        Some(_) => {
            let m: Value =
                serde_json::from_str(&std::fs::read_to_string(sd.join("deployment.json"))?)?;
            PayToPin::from_manifest(&m)?.check_code(&rpc()).await?
        }
        // Local: the mock facilitator has no contract and the terms name
        // Base Sepolia, so the pin is exercised against a mock view of that
        // chain holding a stub at the mock's address (REV-L3-F9 needs a code
        // hash and a checked chain id; fork and testnet check the real node).
        None => {
            PayToPin::from_manifest(&json!({
                "chainId": 84_532, "revenue": DEMO_PAY_TO, "usdc": USDC_BASE_SEPOLIA,
                "revenueCodeHash": hx(&keccak256(MOCK_PAY_TO_CODE)),
            }))?
            .check_code(&MockPayToView)
            .await?
        }
    };
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", o.port)).await?;
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router(Arc::new(state)).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    let base_url = format!("http://127.0.0.1:{}", o.port);
    let uri = format!("{base_url}/v1/metrics/{METRIC}?subject={SUBJECT}");
    let get = |headers: Vec<(&'static str, String)>| {
        let uri = uri.clone();
        tokio::task::spawn_blocking(move || http("GET", &uri, &headers, &[]))
    };
    let header = |hs: &[(String, String)], name: &str| {
        hs.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    let (status, hs, _) = get(Vec::new()).await??;
    if status != 402 {
        return Err(format!("unpaid request answered {status}, want 402").into());
    }
    let pr: PaymentRequired =
        decode_header(&header(&hs, PAYMENT_REQUIRED).ok_or("no PAYMENT-REQUIRED")?)?;
    // The pin refuses any other payTo before anything is signed (REV-4).
    let accepted = pay_pin.select(&pr)?.clone();
    let mut swapped = pr.clone();
    for r in &mut swapped.accepts {
        r.pay_to = ax(&[0x11; 20]);
    }
    if pay_pin.select(&swapped).is_ok() {
        return Err("the payTo pin accepted a changed payTo".into());
    }
    let auth = exact_evm::Authorization {
        from: exact_evm::address(&payer_key).ok_or("demo key")?,
        to: parse_addr(&accepted.pay_to)?,
        value: accepted.amount.parse()?,
        valid_after: 0,
        // Inside the server's cap of now + maxTimeoutSeconds (60) + 300 s slack
        // (OPS-L3-F4), with room for a local chain's clock running ahead.
        valid_before: now_unix() + 300,
        nonce: keccak256(format!("onyx demo {} {}", ax(&st), now_unix()).as_bytes()).0,
    };
    let payment = encode_header(&PaymentPayload {
        x402_version: 2,
        resource: Some(pr.resource),
        payload: exact_evm::sign(&payer_key, &auth, &accepted).ok_or("payment signature")?,
        accepted: accepted.clone(),
    });
    let (status, hs, body) = get(vec![(PAYMENT_SIGNATURE, payment)]).await??;
    if status != 200 {
        return Err(format!(
            "paid request answered {status}: {}",
            String::from_utf8_lossy(&body)
        )
        .into());
    }
    let settled: SettleResponse =
        decode_header(&header(&hs, PAYMENT_RESPONSE).ok_or("no PAYMENT-RESPONSE")?)?;
    let b: Value = serde_json::from_slice(&body)?;
    // The client asks its own node for the root, never the server: the
    // default check (`ClientNode::verify`, M7 loop 3) reads
    // `getFinalizedRoot` at the finalized block and refuses a root that is
    // not final there.
    let spec = &d.spec;
    let verified = ClientNode::new(rpc(), chain_id, st)
        .verify(
            &b,
            &Expect::Metric {
                metric: METRIC,
                def: MetricDef {
                    source: SOURCE_ID,
                    chain: Some(CHAIN_ID),
                    window: Window::Blocks(d.blocks),
                    recipe_id: spec.recipe.ok_or("recipe")?,
                    runtime_hash: spec.runtime.as_ref().ok_or("runtime")?.hash(),
                    schema_id: spec.schema,
                    params_hash: keccak256(&spec.params),
                    label_set: spec.label_set.as_ref().ok_or("labels")?.hash(),
                    registry_root: spec.registry_root,
                },
                subject: SUBJECT,
                as_of: None,
                min_as_of: 0,
            },
        )
        .await?;
    if !verified.chain_checked || verified.root != *root {
        return Err("client check incomplete".into());
    }
    // The contract checks the same row and proof itself.
    let p = &b["proof"];
    let siblings: Vec<String> = p["siblings"]
        .as_array()
        .ok_or("siblings")?
        .iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect();
    let on_chain = net.chain.call(
        &st,
        "verifyRow(uint64,bytes32,bytes,uint64,uint64,bytes32[])(bool)",
        &[
            ROUND.to_string(),
            hx(&shard),
            b["row"].as_str().ok_or("row")?.to_string(),
            p["index"].as_u64().ok_or("index")?.to_string(),
            p["leafCount"].as_u64().ok_or("leafCount")?.to_string(),
            format!("[{}]", siblings.join(",")),
        ],
    )?;
    if on_chain != "true" {
        return Err("the contract's verifyRow refused the served row".into());
    }
    let mut level_line = None;
    if let Some(reg) = net.dep.registry {
        // ADR-0030: the level a consumer may rely on, bound to the same row
        // check on chain; it must equal the level the coordinator attested.
        let out = net.chain.call(
            &reg,
            "verifyRowLeveled(uint64,bytes32,bytes,uint64,uint64,bytes32[])(bool,uint8,bytes32)",
            &[
                ROUND.to_string(),
                hx(&shard),
                b["row"].as_str().ok_or("row")?.to_string(),
                p["index"].as_u64().ok_or("index")?.to_string(),
                p["leafCount"].as_u64().ok_or("leafCount")?.to_string(),
                format!("[{}]", siblings.join(",")),
            ],
        )?;
        let mut it = out.lines().map(str::trim);
        let (ok, lv, m) = (it.next(), it.next(), it.next());
        let want = shard_level(d)?.number().to_string();
        let want_metric = hx(&metric_key(&MetricId(METRIC.into())));
        if ok != Some("true") || lv != Some(want.as_str()) || m != Some(want_metric.as_str()) {
            return Err(format!(
                "the registry's verifyRowLeveled gave {out:?}, want true, level {want} and metric {want_metric}"
            )
            .into());
        }
        level_line = Some(format!(
            "   the registry's verifyRowLeveled says true at proof level {want} (attested before the open, ADR-0030)"
        ));
    }
    let row: OutputRow = borsh::from_slice(&hex_bytes(&b["row"])?)?;
    let okx: EntityFlowRow = borsh::from_slice(&row.value)?;
    let mut lines = vec![
        format!(
            "6. API on {base_url}: unpaid -> 402; paid {} micro-USDC with x402 ({}, settled: {}) -> 200",
            accepted.amount,
            if paid.is_some() { "self-settled into OnyxRevenue" } else { "local mock facilitator" },
            settled.success
        ),
        "   the client checked the answer against its own getFinalizedRoot call; the contract's verifyRow says true".into(),
        format!(
            "   the client paid only after pinning payTo to {} ({}); a changed payTo is refused before signing (checked, REV-4)",
            accepted.pay_to,
            if paid.is_some() { "deployment manifest, runtime code hash checked on its own node" } else { "the local mock's address" }
        ),
    ];
    lines.extend(level_line);
    if paid.is_none() {
        // REV-11: the mock path leaves a receipt, synced before the answer
        // returned. Read a copy as a restarted server would (the running
        // server keeps writing the original).
        let copy = sd.join("receipts.check.jsonl");
        std::fs::copy(sd.join("receipts.jsonl"), &copy)?;
        let n = ReceiptLog::open(
            &copy,
            &Binding {
                chain_id,
                contract: mock_pay_to,
            },
        )?
        .pending();
        std::fs::remove_file(&copy)?;
        let mut lock = copy.into_os_string();
        lock.push(".lock");
        std::fs::remove_file(lock)?;
        if n != 1 {
            return Err(format!("{n} receipts on disk after one paid call, want 1").into());
        }
        lines.push(
            "   its receipt was on disk before the answer returned (receipts.jsonl, REV-11); posting it needs OnyxRevenue: fork and testnet modes".into(),
        );
    }
    let mut l2_report = Value::Null;
    if let (Some(feed), Some((_, mut l2_lines))) = (l2_feed, l2) {
        let (out, report) = level2_serve_and_check(
            net,
            rpc(),
            &feed,
            &base_url,
            &pay_pin,
            &payer_key,
            &sd,
            chain_id,
            &mock_pay_to,
            paid,
        )
        .await?;
        lines.append(&mut l2_lines);
        lines.append(&mut book_lines);
        lines.extend(out);
        l2_report = report;
    }
    if let Some(p) = paid {
        // The payment landed in OnyxRevenue and was credited to this shard
        // in the settle transaction itself (checked, not printed).
        // The API sent the settle, not this driver, so on a live network a
        // lagging node may not show it yet: read again for up to a minute.
        let price: u128 = accepted.amount.parse()?;
        let mut tries = 0;
        let (revenue, held) = loop {
            tries += 1;
            let book = net.chain.call(
                &p.revenue,
                "books(uint64,bytes32)(bool,uint32,uint32,uint64,bytes32,uint128,uint128,uint128)",
                &[ROUND.to_string(), hx(&shard)],
            )?;
            let revenue = first_u128(book.lines().nth(7).unwrap_or(""))?;
            let held = first_u128(&net.chain.call(
                &p.usdc,
                "balanceOf(address)(uint256)",
                &[ax(&p.revenue)],
            )?)?;
            if (revenue == price && held >= price) || net.chain.anvil || tries >= 15 {
                break (revenue, held);
            }
            tokio::time::sleep(Duration::from_secs(4)).await;
        };
        if revenue != price || held < price {
            return Err(format!(
                "OnyxRevenue booked {revenue} for the shard and holds {held}; want {price}"
            )
            .into());
        }
        lines.push(format!(
            "   settle tx {}: OnyxRevenue holds the USDC and booked {} micro-USDC to the shard in the same transaction",
            settled.transaction, revenue
        ));
    }
    let report = json!({
        "unpaid_status": 402,
        "paid_status": 200,
        "price_micro_usdc": accepted.amount,
        "facilitator": if paid.is_some() { "self-settled into OnyxRevenue" } else { "local mock (no token moves)" },
        "settled": settled.success,
        "client_checked_chain_root": verified.chain_checked,
        "contract_verify_row": true,
        "level2": l2_report,
    });
    Ok((server, lines, okx, report, settle))
}

// ------------------------------------------------------------------ level 2

/// Local mode: run the recorded U.S. Treasury Debt to the Penny round
/// (adapters/source-fiscaldata, recorded HTTPS responses, simulated
/// observers), load its results store as the API would (every round
/// re-checked), and record it on chain: the registry owner registers the
/// metric's template (kind from the template, REG-R2-F2) and the coordinator
/// records the fact's result (ADR-0030). Fork mode runs it with `--level2`
/// (ROADMAP B1): the paid sale self-settles into the fact's revenue book,
/// which `serve_and_check` opens from the reporter key before serving
/// (`OnyxRevenue.registerObservation`, R-REV-12; THREATS L2API-4). Testnet
/// mode skips it until level 2 is enabled on Base Sepolia (ROADMAP B2-B6).
fn level2_prepare(
    net: &mut Net,
    paid: Option<&Paid>,
    level2: bool,
) -> Res<Option<(ObservationFeed, Vec<String>)>> {
    use onyx_source_fiscaldata::round::{
        recorded_draw, results_store, run, run_receipted, store, Page,
    };
    if paid.is_some() && !level2 {
        return Ok(None);
    }
    let rec = onyx_source_fiscaldata::recorded();
    let pages: Vec<Page> = rec
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body.clone()),
        })
        .collect();
    let t = onyx_source_fiscaldata::template();
    let reg = net.dep.registry;
    if let Some(reg) = reg {
        let mk = metric_key(&t.metric);
        let owner = parse_addr(net.chain.call(&reg, "owner()(address)", &[])?.trim())?;
        let kind = MetricKind::of(&t) as u8;
        net.chain.send(
            &owner,
            &reg,
            "registerMetric(bytes32,uint8,bytes32,uint8)",
            &[hx(&mk), kind.to_string(), hx(&t.id()), "0".into()],
        )?;
    }
    let mut st = store();
    // ROADMAP B3: the deployment's own receipt key on a fork or testnet; the
    // public test key only in local mode (a fresh local anvil).
    let receipt = match (paid, reg) {
        (Some(p), Some(_)) => p
            .receipt_key
            .clone()
            .ok_or("a paid run with a registry needs its own receipt key (ROADMAP B3)")?,
        _ => ReceiptKey::public_test(),
    };
    // The draw is announced on the registry before its beacon height and the
    // beacon fixed from the anvil's block hash (L2RND-R1); without a registry
    // the recorded draw is replayed.
    let coord = net.coord;
    let r = match reg {
        Some(reg) => {
            let mut chain = DemoDraw {
                chain: &mut net.chain,
                reg,
                coord,
                receipt_signer: receipt.address(),
            };
            run_receipted(
                &rec.record_date,
                onyx_source_fiscaldata::RECORDED_WINDOW_START,
                &pages,
                &mut st,
                &mut chain,
                &receipt,
            )?
        }
        None => run(
            &rec.record_date,
            onyx_source_fiscaldata::RECORDED_WINDOW_START,
            &pages,
            &mut st,
            &mut RecordedDraw::new(recorded_draw()),
        )?,
    };
    // The driver pins the template it registers (L2API-R4-F1), as an
    // operator pins the reviewed registration.
    let pins = onyx_api::observations::TemplatePins::from([(t.metric.0.clone(), t.id())]);
    let feed = ObservationStore::from_json(&results_store(&r)?.to_string(), now_unix(), &pins)?;
    let mut lines = vec![format!(
        "6b. Level 2: the recorded Treasury round ({} observers) loads as the API's results store; every round re-checked against the template",
        r.observers.len()
    )];
    if let Some(reg) = reg {
        // Only a value outcome is recorded on chain (SEC-R1-F1).
        let rec = st
            .onchain_record(&r.settlement.result_hash())
            .ok_or("the Treasury round settled no value")?;
        // Review REVLOG HIGH-1: never record a round the transcript log
        // refuses (a posted commit or receipted reveal left out).
        let q = onyx_source_fiscaldata::round::transcript_query(
            &r,
            st.onchain_fact(&r.spec)?.registry_key(),
        );
        let logged = DemoDraw {
            chain: &mut net.chain,
            reg,
            coord,
            receipt_signer: receipt.address(),
        }
        .transcript_complete(&q)?;
        if !logged {
            return Err("transcriptComplete is false for the Treasury round: not recorded".into());
        }
        net.chain.send(
            &coord,
            &reg,
            "recordObservation(bytes32,bytes32,uint64,bytes32,bytes32,bytes32,bytes32)",
            &[
                hx(&rec.metric_key),
                hx(&rec.subject_key),
                rec.window_end.to_string(),
                hx(&rec.spec_id),
                hx(&rec.result),
                hx(&rec.draw_pool),
                hx(&rec.cfg_hash),
            ],
        )?;
        lines.push(format!(
            "   registry: the owner registered the feed template (dated feed); the coordinator announced the draw in block {} for beacon block {}, fixed the beacon from the chain, and recorded the fact's result",
            r.onchain_draw.announced_at, r.onchain_draw.beacon_height
        ));
    }
    Ok(Some((feed, lines)))
}

/// The level-2 draw on the demo's `OnyxRegistry` (L2RND-R1): the coordinator
/// announces it, empty blocks are mined to the beacon height, and the
/// coordinator fixes the beacon from the block hash.
/// Blocks below the head the demo reads the transcript log at: a local
/// anvil has no finality tag worth reading, so a pinned depth (review
/// REVLOG-R3-1; `CastChain::finality` for rehearsals and testnets).
const DEMO_CONFIRMATIONS: u64 = 2;

struct DemoDraw<'a> {
    chain: &'a mut Chain,
    reg: [u8; 20],
    coord: [u8; 20],
    /// The receipt signer each draw names (ROADMAP B3).
    receipt_signer: [u8; 20],
}

impl DemoDraw<'_> {
    fn fact(&self, f: &OnchainFact) -> Result<Hash32, String> {
        let out = self
            .chain
            .call(
                &self.reg,
                "factKey(bytes32,bytes32,uint64)(bytes32)",
                &[
                    hx(&f.metric_key),
                    hx(&f.subject_key),
                    f.window_end.to_string(),
                ],
            )
            .map_err(|e| e.to_string())?;
        parse_h32(&out).map_err(|e| e.to_string())
    }
}

impl DemoDraw<'_> {
    /// Relay a commit or receipt to the registry's transcript log (ADR-0027
    /// amendment 8) from the coordinator's account (anyone may relay),
    /// unless its slot is filled: a slot filled first is `Posted::Before`.
    #[allow(clippy::too_many_arguments)]
    fn post(
        &mut self,
        what: &str,
        f: &OnchainFact,
        attempt: u32,
        observer: &[u8; 20],
        round_id: &Hash32,
        commit: &Hash32,
        sig: &[u8; 65],
    ) -> Result<Posted, String> {
        let (reg, coord) = (self.reg, self.coord);
        let k = self.fact(f)?;
        let held = |d: &Self| -> Result<Option<Hash32>, String> {
            let h = if what == "postCommit" {
                d.commit_log(&k, attempt, observer, round_id, None)?.0
            } else {
                let slot = transcript_log_key(&k, attempt, observer, round_id);
                let out = d
                    .chain
                    .call(&reg, "receiptLog(bytes32)(bytes32)", &[hx(&slot)])
                    .map_err(|e| e.to_string())?;
                parse_h32(&out).map_err(|e| e.to_string())?
            };
            Ok((h != Hash32::ZERO).then_some(h))
        };
        if let Some(c) = held(self)? {
            return Ok(Posted::Before(c));
        }
        let sent = self.chain.send(
            &coord,
            &reg,
            &format!("{what}(bytes32,uint32,address,bytes32,bytes32,bytes)"),
            &[
                hx(&k),
                attempt.to_string(),
                ax(observer),
                hx(round_id),
                hx(commit),
                format!("0x{}", hex::encode(sig)),
            ],
        );
        match (sent, held(self)?) {
            (Ok(_), _) => Ok(Posted::Now),
            (Err(_), Some(c)) => Ok(Posted::Before(c)),
            (Err(e), None) => Err(e.to_string()),
        }
    }

    /// `commitLogOf` at `block` (the latest when `None`): the commit in the
    /// slot (zero when none), its block time and the observer's signature,
    /// from contract state (review REVLOG-R2 HIGH-1: never `eth_getLogs`).
    fn commit_log(
        &self,
        k: &Hash32,
        attempt: u32,
        observer: &[u8; 20],
        round_id: &Hash32,
        block: Option<u64>,
    ) -> Result<(Hash32, u64, Option<[u8; 65]>), String> {
        let mut a = vec!["call".to_string()];
        if let Some(b) = block {
            a.extend(["--block".to_string(), b.to_string()]);
        }
        a.extend([
            ax(&self.reg),
            "commitLogOf(bytes32,uint32,address,bytes32)(bytes32,uint64,bytes)".into(),
            hx(k),
            attempt.to_string(),
            ax(observer),
            hx(round_id),
        ]);
        let out = self.chain.run(&a).map_err(|e| e.to_string())?;
        onyx_coordinator::cast_chain::parse_commit_log(&out)
    }

    /// `drawOf` (and `drawCount`) at `block`, the latest when `None`; a node
    /// without `block` errors (`cast call --block N`).
    fn draw_of_at(
        &self,
        f: &OnchainFact,
        attempt: u32,
        block: Option<u64>,
    ) -> Result<Option<DrawRecord>, String> {
        let reg = self.reg;
        let k = self.fact(f)?;
        // Review R8-1: `drawOf` and `drawCount` from ONE block.
        let block = match block {
            Some(b) => b,
            None => self.height()?,
        };
        let at_block = |sig: &str, args: &[String]| -> Result<String, String> {
            let mut a = vec!["call".to_string()];
            a.extend(["--block".to_string(), block.to_string()]);
            a.extend([ax(&reg), sig.to_string()]);
            a.extend(args.iter().cloned());
            self.chain.run(&a).map_err(|e| e.to_string())
        };
        let out = at_block(
            "drawOf(bytes32,uint32)((bytes32,uint64,uint64,bytes32))",
            &[hx(&k), attempt.to_string()],
        )?;
        let parts: Vec<&str> = out
            .trim_matches(|c| c == '(' || c == ')')
            .split(", ")
            .collect();
        let [commit, height, at, beacon] = parts[..] else {
            return Err(format!("drawOf returned {out}"));
        };
        let commit = parse_h32(commit).map_err(|e| e.to_string())?;
        if commit == Hash32::ZERO {
            return Ok(None);
        }
        let num = |t: &str| first_u128(t).map(|n| n as u64).map_err(|e| e.to_string());
        Ok(Some(DrawRecord {
            chain_id: self.chain.chain_id,
            registry: reg,
            attempt,
            draw_count: first_u128(&at_block("drawCount(bytes32)(uint32)", &[hx(&k)])?)
                .map_err(|e| e.to_string())? as u32,
            announced_at: num(at)?,
            commit,
            beacon_height: num(height)?,
            beacon: parse_h32(beacon).map_err(|e| e.to_string())?,
        }))
    }

    /// The confirmed head: the latest block minus [`DEMO_CONFIRMATIONS`]
    /// (review REVLOG-R2 LOW-4).
    fn confirmed(&self) -> Result<u64, String> {
        Ok(self.height()?.saturating_sub(DEMO_CONFIRMATIONS))
    }
}

impl ChainView for DemoDraw<'_> {
    fn height(&self) -> Result<u64, String> {
        self.chain.block().map_err(|e| e.to_string())
    }
    fn block_hash(&self, height: u64) -> Result<Option<Hash32>, String> {
        if height > self.height()? {
            return Ok(None);
        }
        self.chain
            .block_hash(height)
            .map(Some)
            .map_err(|e| e.to_string())
    }
}

impl DrawChain for DemoDraw<'_> {
    fn draw_count(&self, f: &OnchainFact) -> Result<u32, String> {
        let k = self.fact(f)?;
        let out = self
            .chain
            .call(&self.reg, "drawCount(bytes32)(uint32)", &[hx(&k)])
            .map_err(|e| e.to_string())?;
        Ok(first_u128(&out).map_err(|e| e.to_string())? as u32)
    }
    fn draw_budget(&self, f: &OnchainFact) -> Result<(u32, u32), String> {
        let out = self
            .chain
            .call(
                &self.reg,
                "drawBudget(bytes32,bytes32)(uint32,uint32)",
                &[hx(&f.metric_key), hx(&f.subject_key)],
            )
            .map_err(|e| e.to_string())?;
        let v: Vec<u32> = out
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| first_u128(l).map(|n| n as u32).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;
        let [strikes, allowance] = v[..] else {
            return Err(format!("drawBudget returned {out}"));
        };
        Ok((strikes, allowance))
    }
    fn announce(&mut self, d: &OnchainDraw) -> Result<(), String> {
        let (reg, coord) = (self.reg, self.coord);
        // ADR-0027 amendment 8: every draw names its receipt signer; ROADMAP
        // B3: the deployment's own key on a fork or testnet (the public test
        // key in local mode). A different one on the registry is rotated.
        let want = self.receipt_signer;
        let have = self
            .chain
            .call(&reg, "receiptSigner()(address)", &[])
            .map_err(|e| e.to_string())?;
        if parse_addr(have.trim()).map_err(|e| e.to_string())? != want {
            self.chain
                .send(&coord, &reg, "setReceiptSigner(address)", &[ax(&want)])
                .map_err(|e| e.to_string())?;
        }
        self.chain
            .send(
                &coord,
                &reg,
                "announceDraw(bytes32,bytes32,uint64,uint32,bytes32,uint64)",
                &[
                    hx(&d.fact.metric_key),
                    hx(&d.fact.subject_key),
                    d.fact.window_end.to_string(),
                    d.attempt.to_string(),
                    hx(&d.commit),
                    d.beacon_height.to_string(),
                ],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn fix_beacon(&mut self, d: &OnchainDraw) -> Result<DrawRecord, String> {
        let (reg, coord) = (self.reg, self.coord);
        // fixBeacon must land above the beacon height.
        self.chain
            .reach(d.beacon_height + 1)
            .map_err(|e| e.to_string())?;
        let k = self.fact(&d.fact)?;
        // Reverts `BeaconAlreadyFixed` like the registry when anyone fixed
        // it first; `draw_and_open` reads the draw before and after any
        // error (L2RND-R2-F4, review REVLOG-R5).
        self.chain
            .send(
                &coord,
                &reg,
                "fixBeacon(bytes32,uint32)",
                &[hx(&k), d.attempt.to_string()],
            )
            .map_err(|e| e.to_string())?;
        self.draw_of(&d.fact, d.attempt)?
            .ok_or_else(|| format!("drawOf: no draw at attempt {}", d.attempt))
    }
    fn draw_of(&self, f: &OnchainFact, attempt: u32) -> Result<Option<DrawRecord>, String> {
        self.draw_of_at(f, attempt, None)
    }
    /// At the confirmed head (review R6), every call named `--block N`.
    fn draw_of_final(
        &self,
        f: &OnchainFact,
        attempt: u32,
    ) -> Result<(u64, Option<DrawRecord>), String> {
        let b = self.confirmed()?;
        Ok((b, self.draw_of_at(f, attempt, Some(b))?))
    }
    fn post_commit(
        &mut self,
        f: &OnchainFact,
        attempt: u32,
        observer: &[u8; 20],
        round_id: &Hash32,
        commit: &Hash32,
        sig: &[u8; 65],
    ) -> Result<Posted, String> {
        self.post("postCommit", f, attempt, observer, round_id, commit, sig)
    }
    fn post_receipt(
        &mut self,
        f: &OnchainFact,
        attempt: u32,
        observer: &[u8; 20],
        round_id: &Hash32,
        commit: &Hash32,
        sig: &[u8; 65],
    ) -> Result<Posted, String> {
        self.post("postReceipt", f, attempt, observer, round_id, commit, sig)
    }
    fn log_block(&self) -> Result<Option<LogBlock>, String> {
        // One answer: number and time of the same block.
        let b = self
            .chain
            .rpc(
                "eth_getBlockByNumber",
                json!([format!("0x{:x}", self.confirmed()?), false]),
            )
            .map_err(|e| e.to_string())?;
        let q = |k: &str| qty(&b[k]).map(|v| v as u64).map_err(|e| e.to_string());
        Ok(Some(LogBlock {
            number: q("number")?,
            time: q("timestamp")?,
        }))
    }
    fn posted_commit(
        &self,
        block: u64,
        f: &OnchainFact,
        attempt: u32,
        observer: &[u8; 20],
        round_id: &Hash32,
    ) -> Result<Option<PostedCommit>, String> {
        let k = self.fact(f)?;
        let (commit, posted_at, sig) =
            self.commit_log(&k, attempt, observer, round_id, Some(block))?;
        if commit == Hash32::ZERO {
            return Ok(None);
        }
        Ok(Some(PostedCommit {
            commit,
            sig: sig.ok_or("commitLogOf: a posted commit without its signature")?,
            posted_at,
        }))
    }
    fn transcript_complete(&self, q: &TranscriptLogQuery) -> Result<bool, String> {
        let out = self
            .chain
            .call(
                &self.reg,
                "transcriptComplete(bytes32,uint32,bytes32,uint64,uint64,bytes)(bool)",
                &[
                    hx(&q.registry_fact),
                    q.attempt.to_string(),
                    hx(&q.round_id),
                    q.commit_open.to_string(),
                    q.commit_close.to_string(),
                    format!("0x{}", hex::encode(q.packed_entries())),
                ],
            )
            .map_err(|e| e.to_string())?;
        match out.trim() {
            "true" => Ok(true),
            "false" => Ok(false),
            o => Err(format!("transcriptComplete returned {o}")),
        }
    }
    /// ROADMAP B3: the draw's snapshotted receipt signer, so a rotated key
    /// is refused before any receipt (review B2-R1-L2).
    fn draw_receipt_signer(
        &self,
        f: &OnchainFact,
        attempt: u32,
    ) -> Result<Option<[u8; 20]>, String> {
        let k = self.fact(f)?;
        let out = self
            .chain
            .call(
                &self.reg,
                "drawReceiptSigner(bytes32,uint32)(address)",
                &[hx(&k), attempt.to_string()],
            )
            .map_err(|e| e.to_string())?;
        let a = parse_addr(out.trim()).map_err(|e| e.to_string())?;
        Ok((a != [0; 20]).then_some(a))
    }
}

/// One `eth_call` to `reg` with `data`, its return bytes.
async fn ask_raw(rpc: &HttpJsonRpc<Post>, reg: &[u8; 20], data: Vec<u8>) -> Res<Vec<u8>> {
    let ret = rpc
        .call(
            "eth_call",
            json!([{ "to": ax(reg), "data": format!("0x{}", hex::encode(data)) }, "latest"]),
        )
        .await?;
    hex_bytes(&ret)
}

/// ROADMAP C11: the returns of one served round's sealed-reveal calls
/// (`drawRevealKey`, `revealLogComplete`, `attemptMatches`, each served post),
/// asked by the client with its own arguments.
async fn ask_sealed(
    rpc: &HttpJsonRpc<Post>,
    reg: &[u8; 20],
    calls: onyx_api::client::SealedCalls,
) -> Res<onyx_api::client::SealedRets> {
    let mut posts = Vec::new();
    for (r, p) in calls.posts {
        posts.push((ask_raw(rpc, reg, r).await?, ask_raw(rpc, reg, p).await?));
    }
    Ok(onyx_api::client::SealedRets {
        time_locked: ask_raw(rpc, reg, calls.time_locked).await?,
        reveal_key: ask_raw(rpc, reg, calls.reveal_key).await?,
        reveal_log: ask_raw(rpc, reg, calls.reveal_log).await?,
        matches: match calls.matches {
            Some(m) => Some(ask_raw(rpc, reg, m).await?),
            None => None,
        },
        posts,
    })
}

/// Pay for the Treasury value and check it as a client: 402, x402 payment
/// (local mock), `verify_observation` with the client's own copy of the
/// registered template, and `OnyxRegistry.observationLevel` on its own node
/// with arguments it recomputed. The receipt is billed to the fact's
/// observation book.
#[allow(clippy::too_many_arguments)]
async fn level2_serve_and_check(
    net: &Net,
    rpc: HttpJsonRpc<Post>,
    feed: &ObservationFeed,
    base_url: &str,
    pay_pin: &onyx_api::client::VerifiedPin,
    payer_key: &[u8; 32],
    sd: &std::path::Path,
    chain_id: u64,
    mock_pay_to: &[u8; 20],
    paid: Option<&Paid>,
) -> Res<(Vec<String>, Value)> {
    let rec = onyx_source_fiscaldata::recorded();
    let subject_text = onyx_source_fiscaldata::subject(&rec.record_date);
    let uri = format!(
        // `outcome=all`: with every round of the fact that settled no value
        // (SEC-R2-F1), checked below against the fact's draws on chain.
        "{base_url}/v1/observations/{}?subject={subject_text}&outcome=all",
        onyx_source_fiscaldata::METRIC
    );
    let get = |headers: Vec<(&'static str, String)>| {
        let uri = uri.clone();
        tokio::task::spawn_blocking(move || http("GET", &uri, &headers, &[]))
    };
    let header = |hs: &[(String, String)], name: &str| {
        hs.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    let (status, hs, _) = get(Vec::new()).await??;
    if status != 402 {
        return Err(format!("unpaid level-2 request answered {status}, want 402").into());
    }
    let pr: PaymentRequired =
        decode_header(&header(&hs, PAYMENT_REQUIRED).ok_or("no PAYMENT-REQUIRED")?)?;
    let accepted = pay_pin.select(&pr)?.clone();
    let auth = exact_evm::Authorization {
        from: exact_evm::address(payer_key).ok_or("demo key")?,
        to: parse_addr(&accepted.pay_to)?,
        value: accepted.amount.parse()?,
        valid_after: 0,
        valid_before: now_unix() + 300,
        nonce: keccak256(format!("onyx demo level 2 {}", now_unix()).as_bytes()).0,
    };
    let payment = encode_header(&PaymentPayload {
        x402_version: 2,
        resource: Some(pr.resource),
        payload: exact_evm::sign(payer_key, &auth, &accepted).ok_or("payment signature")?,
        accepted: accepted.clone(),
    });
    let (status, _, body) = get(vec![(PAYMENT_SIGNATURE, payment)]).await??;
    if status != 200 {
        return Err(format!(
            "paid level-2 request answered {status}: {}",
            String::from_utf8_lossy(&body)
        )
        .into());
    }
    let b: Value = serde_json::from_slice(&body)?;
    // The client's own copy of the registered template, never the server's.
    let template = onyx_source_fiscaldata::template();
    let subject = onyx_core::Subject::Aggregate(subject_text.clone());
    let v = verify_observation(
        &b,
        &ObservationExpect {
            template: &template,
            subject: &subject,
            window_end: None,
            // The client's freshness floor: the recorded round's window.
            min_window_end: onyx_source_fiscaldata::RECORDED_WINDOW_START,
            now: now_unix(),
        },
    )?;
    let mut out = vec![format!(
        "   API: unpaid -> 402; paid {} micro-USDC -> 200: U.S. total public debt on {} = ${} (level 2)",
        accepted.amount, rec.record_date, v.value
    ),
    "   the client rebuilt the round from its own copy of the template: spec, round id, every commit opened, reveals re-aggregated into the result hash".into()];
    let mut on_chain_level = Value::Null;
    let mut draws = Value::Null;
    if let Some(reg) = net.dep.registry {
        // L2RND-R2-F6: observationLevel, drawMatches and the draw bound,
        // each asked by the client with its own arguments.
        let mut rets = Vec::new();
        for data in v.onchain_calls() {
            let ret = rpc
                .call(
                    "eth_call",
                    json!([{
                        "to": ax(&reg),
                        "data": format!("0x{}", hex::encode(data)),
                    }, "latest"]),
                )
                .await?;
            rets.push(hex_bytes(&ret)?);
        }
        let sealed = ask_sealed(&rpc, &reg, v.sealed_calls()).await?;
        verify_observation_onchain(&v, &rets[0], &rets[1], &rets[2], &rets[3], &sealed)
            .map_err(|e| format!("the registry refused the round: {e}"))?;
        // ADR-0027 amendment 8 (SIG-R1-F1): each observer posted its reveal
        // receipt, so a server that drops one reveal is caught on chain.
        let mut dropped = v.checked.log.clone();
        let first = dropped
            .entries
            .first_mut()
            .ok_or("a round without observers")?;
        first.revealed = false;
        let caught =
            transcript_complete_from_return(&ask_raw(&rpc, &reg, dropped.calldata()).await?)?;
        if caught {
            return Err("the registry accepted a transcript without a receipted reveal".into());
        }
        // SEC-R2-F1: every draw of the fact, from the chain: the served
        // value round is the latest, and each earlier draw is a published
        // round without a value or a draw whose beacon was never fixed.
        let attempts = v
            .attempts
            .as_ref()
            .ok_or("no attempts in an outcome=all answer")?;
        let ask = |data: Vec<u8>| {
            let rpc = &rpc;
            async move {
                let ret = rpc
                    .call(
                        "eth_call",
                        json!([{ "to": ax(&reg), "data": format!("0x{}", hex::encode(data)) }, "latest"]),
                    )
                    .await?;
                hex_bytes(&ret)
            }
        };
        let count_ret = ask(attempts.draw_count_calldata()).await?;
        let count = onyx_api::client::draw_count_from_return(&count_ret)?;
        let mut draw_rets = Vec::new();
        for i in 0..attempts.draws_without_value(count) {
            draw_rets.push(ask(attempts.draw_of_calldata(i)).await?);
        }
        let mut complete_rets = Vec::new();
        for data in attempts.transcript_complete_calldatas() {
            complete_rets.push(ask(data).await?);
        }
        let mut sealed_rets = Vec::new();
        for calls in attempts.sealed_calls() {
            sealed_rets.push(ask_sealed(&rpc, &reg, calls).await?);
        }
        let tally = onyx_api::client::verify_attempts_onchain(
            attempts,
            &count_ret,
            &draw_rets,
            &ask(attempts.draw_budget_calldata()).await?,
            &complete_rets,
            &sealed_rets,
        )
        .map_err(|e| format!("the registry refused the fact's draws: {e}"))?;
        let (strikes, allowance) = draw_budget_from_return(&rets[2])?;
        on_chain_level = json!(observation_level_from_return(&rets[0])?);
        out.push(
            "   the registry's observationLevel, asked by the client with its own arguments, says 2".into(),
        );
        out.push(
            "   the registry's drawMatches says the round's draw is the fact's latest, announced before its beacon block, beacon from the chain".into(),
        );
        out.push(format!(
            "   the registry's drawBudget: draw {} of at most {allowance} for the subject ({strikes} open without a recorded value)",
            v.checked.draw.attempt + 1
        ));
        out.push(format!(
            "   the registry's transcriptComplete: every drawn observer's posted commit and reveal receipt is in the served round ({} observers); the same round with one receipted reveal dropped is refused",
            v.checked.log.entries.len()
        ));
        out.push(format!(
            "   the fact's draws on chain: {} draw(s), {} without a value, {} published round(s) without a value checked",
            tally.draws, tally.without_value, tally.published
        ));
        draws = json!({ "draws": tally.draws, "withoutValue": tally.without_value });
    }
    // Fork (ROADMAP B1): the API self-settled the sale into the fact's
    // observation book in the settle transaction (no receipt is queued).
    if let Some(p) = paid {
        let price: u128 = accepted.amount.parse()?;
        let book = net.chain.call(
            &p.revenue,
            "books(uint64,bytes32)(bool,uint32,uint32,uint64,bytes32,uint128,uint128,uint128)",
            &[OBSERVATION_ROUND.to_string(), hx(&v.checked.registry_fact)],
        )?;
        let l: Vec<&str> = book.lines().collect();
        let revenue = first_u128(l.get(7).copied().unwrap_or(""))?;
        if l.first().copied() != Some("true") || revenue != price {
            return Err(format!(
                "the fact's observation book reads {book:?}; want open with {price} micro-USDC"
            )
            .into());
        }
        out.push(format!(
            "   self-settled into OnyxRevenue: booked {revenue} micro-USDC to the fact's observation book in the settle transaction (R-REV-12)"
        ));
        return Ok((
            out,
            json!({
                "metric": onyx_source_fiscaldata::METRIC,
                "subject": subject_text,
                "value": v.value,
                "paid_micro_usdc": accepted.amount,
                "client_verified": true,
                "registry_level": on_chain_level,
                "draws": draws,
                "result_hash": v.checked.result.to_string(),
                "observation_book_micro_usdc": revenue.to_string(),
            }),
        ));
    }
    // The sale's receipt is billed to the fact's observation book.
    let copy = sd.join("receipts.l2check.jsonl");
    std::fs::copy(sd.join("receipts.jsonl"), &copy)?;
    let receipts = ReceiptLog::open(
        &copy,
        &Binding {
            chain_id,
            contract: *mock_pay_to,
        },
    )?
    .peek(16);
    std::fs::remove_file(&copy)?;
    let mut lock = copy.into_os_string();
    lock.push(".lock");
    std::fs::remove_file(lock)?;
    if !receipts
        .iter()
        .any(|r| r.round == OBSERVATION_ROUND && r.shard == v.checked.registry_fact)
    {
        return Err("no receipt billed to the fact's observation book".into());
    }
    out.push(
        "   its receipt is billed to the fact's observation book (OnyxRevenue.registerObservation, R-REV-12)".into(),
    );
    if feed.metric() != onyx_source_fiscaldata::METRIC {
        return Err("served another metric".into());
    }
    Ok((
        out,
        json!({
            "metric": onyx_source_fiscaldata::METRIC,
            "subject": subject_text,
            "value": v.value,
            "paid_micro_usdc": accepted.amount,
            "client_verified": true,
            "registry_level": on_chain_level,
            "draws": draws,
            "result_hash": v.checked.result.to_string(),
        }),
    ))
}

// ------------------------------------------------------------------ receipt path

/// A standard x402 `exact` facilitator, as x402.org runs one: it submits the
/// client's EIP-3009 authorization to the token itself
/// (`transferWithAuthorization`, to = the `payTo`) and knows nothing of
/// shards. The USDC reaches `OnyxRevenue` unallocated; the API records a
/// receipt naming the shard served and the reporter attributes it with
/// `credit` (ADR-0025 item 2, REV-11). Here it sends from the API operator's
/// key, shared with the self-settle and the reporter (one nonce sequence).
struct TokenFacilitator {
    sender: Arc<Sender<HttpJsonRpc<Post>>>,
    usdc: [u8; 20],
    revenue: [u8; 20],
}

impl TokenFacilitator {
    fn parse(
        &self,
        req: &FacilitatorRequest,
    ) -> Result<(exact_evm::Authorization, Vec<u8>), &'static str> {
        let r = &req.payment_requirements;
        if req.payment_payload.accepted != *r
            || r.network != BASE_SEPOLIA
            || parse_addr(&r.pay_to).ok() != Some(self.revenue)
            || parse_addr(&r.asset).ok() != Some(self.usdc)
        {
            return Err("invalid_payment_requirements");
        }
        let a = exact_evm::precheck(&req.payment_payload.payload, r, now_unix())?;
        let sig = req.payment_payload.payload["signature"]
            .as_str()
            .and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())
            .ok_or("invalid_payload")?;
        Ok((a, sig))
    }
}

impl Facilitator for TokenFacilitator {
    fn verify<'a>(
        &'a self,
        req: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>> {
        Box::pin(async move {
            let (a, sig) = match self.parse(req) {
                Ok(x) => x,
                Err(e) => {
                    return Ok(VerifyResponse {
                        is_valid: false,
                        invalid_reason: Some(e.into()),
                        payer: None,
                    })
                }
            };
            // The token judges the authorization (an `eth_call` of the transfer).
            let ok = self
                .sender
                .call(&self.usdc, &transfer_with_authorization_calldata(&a, &sig))
                .await
                .is_ok();
            Ok(VerifyResponse {
                is_valid: ok,
                invalid_reason: (!ok).then(|| "invalid_exact_evm_payload_signature".into()),
                payer: Some(ax(&a.from)),
            })
        })
    }

    fn settle<'a>(
        &'a self,
        req: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        Box::pin(async move {
            let (a, sig) = self
                .parse(req)
                .map_err(|e| FacilitatorError::Unavailable(e.into()))?;
            let m = self
                .sender
                .send(
                    &self.usdc,
                    &transfer_with_authorization_calldata(&a, &sig),
                    0,
                    SETTLE_DEADLINE,
                )
                .await
                .map_err(|e| FacilitatorError::Unavailable(e.to_string()))?;
            Ok(SettleResponse {
                success: true,
                error_reason: None,
                payer: Some(ax(&a.from)),
                transaction: m.hash,
                network: BASE_SEPOLIA.into(),
            })
        })
    }

    fn settle_budget(&self) -> Option<Duration> {
        Some(SETTLE_DEADLINE + Duration::from_secs(20))
    }
}

/// One shard's revenue book in `OnyxRevenue`.
struct Book {
    miners: u32,
    validators: u32,
    label_set: bool,
    per_miner: u128,
    revenue: u128,
}

fn book(net: &Net, revenue: &[u8; 20], shard: &Hash32) -> Res<Book> {
    let out = net.chain.call(
        revenue,
        "books(uint64,bytes32)(bool,uint32,uint32,uint64,bytes32,uint128,uint128,uint128)",
        &[ROUND.to_string(), hx(shard)],
    )?;
    let l: Vec<&str> = out.lines().collect();
    let at = |i: usize| l.get(i).copied().unwrap_or("");
    Ok(Book {
        miners: u32::try_from(first_u128(at(1))?)?,
        validators: u32::try_from(first_u128(at(2))?)?,
        label_set: parse_h32(at(4))? != Hash32([0; 32]),
        per_miner: first_u128(at(5))?,
        revenue: first_u128(at(7))?,
    })
}

/// REV-11 and REV-4 end to end (fork, testnet): the customer buys the row
/// again through an API that uses a standard facilitator. The pinned client
/// pays OnyxRevenue; the USDC arrives unallocated and the API's receipt is on
/// disk before the answer returns. The API is then stopped (a crash) and its
/// log reopened from disk; the reporter posts it with `credit`, the contract
/// attributes the fee to the shard, and the first miner claims and withdraws
/// exactly its share of both sales.
async fn receipt_path(
    net: &mut Net,
    o: &Opts,
    d: &Data,
    p: &Paid,
    settle: &Arc<SelfSettle<HttpJsonRpc<Post>>>,
    leaf_count: u64,
) -> Res<(Vec<String>, Value)> {
    let st = net.st();
    let shard = d.spec.shard_id();
    let rpc = || HttpJsonRpc::new(o.rpc.clone(), node_post(o.mode));
    let code = parse_h32(&net.chain.run(&["codehash".into(), ax(&st)])?)?;
    let roots = Arc::new(ChainRoots::connect(st, net.chain.chain_id, Some(code), rpc()).await?);
    let dir = o.deploy_dir(&p.revenue).join("standard-facilitator");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("receipts.jsonl");
    let bind = Binding {
        chain_id: net.chain.chain_id,
        contract: p.revenue,
    };
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let base_url = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    let state = ApiState::new(
        demo_catalog(d, leaf_count)?,
        roots,
        d.cas.clone() as Arc<dyn Cas + Send + Sync>,
        Some(Payments {
            terms: demo_terms(ax(&p.revenue)),
            facilitator: Arc::new(TokenFacilitator {
                sender: settle.shared_sender(),
                usdc: p.usdc,
                revenue: p.revenue,
            }),
        }),
        Arc::new(now_unix),
        base_url.clone(),
        ApiConfig {
            allow_test_labels: true,
            ..ApiConfig::default()
        },
    )?
    .with_receipt_log(&path, &bind)?;
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router(Arc::new(state)).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    let uri = format!("{base_url}/v1/metrics/{METRIC}?subject={SUBJECT}");
    let get = |headers: Vec<(&'static str, String)>| {
        let uri = uri.clone();
        tokio::task::spawn_blocking(move || http("GET", &uri, &headers, &[]))
    };
    let header = |hs: &[(String, String)], name: &str| {
        hs.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    let (status, hs, _) = get(Vec::new()).await??;
    if status != 402 {
        return Err(format!("unpaid request answered {status}, want 402").into());
    }
    let pr: PaymentRequired =
        decode_header(&header(&hs, PAYMENT_REQUIRED).ok_or("no PAYMENT-REQUIRED")?)?;
    let m: Value = serde_json::from_str(&std::fs::read_to_string(
        o.deploy_dir(&p.revenue).join("deployment.json"),
    )?)?;
    let pin = PayToPin::from_manifest(&m)?.check_code(&rpc()).await?;
    let accepted = pin.select(&pr)?.clone();
    let auth = exact_evm::Authorization {
        from: exact_evm::address(&p.payer_key).ok_or("payer key")?,
        to: p.revenue,
        value: accepted.amount.parse()?,
        valid_after: 0,
        // Inside the server's cap of now + maxTimeoutSeconds (60) + 300 s slack
        // (OPS-L3-F4), with room for a local chain's clock running ahead.
        valid_before: now_unix() + 300,
        nonce: keccak256(format!("onyx demo receipt path {} {}", ax(&st), now_unix()).as_bytes()).0,
    };
    let payment = encode_header(&PaymentPayload {
        x402_version: 2,
        resource: Some(pr.resource),
        payload: exact_evm::sign(&p.payer_key, &auth, &accepted).ok_or("payment signature")?,
        accepted: accepted.clone(),
    });
    let (status, hs, body) = get(vec![(PAYMENT_SIGNATURE, payment)]).await??;
    if status != 200 {
        return Err(format!(
            "paid request (standard facilitator) answered {status}: {}",
            String::from_utf8_lossy(&body)
        )
        .into());
    }
    let settled: SettleResponse =
        decode_header(&header(&hs, PAYMENT_RESPONSE).ok_or("no PAYMENT-RESPONSE")?)?;
    // A crash right after the answer: the server and its log are gone.
    server.abort();
    let _ = server.await;
    let log = Arc::new(ReceiptLog::open(&path, &bind)?);
    if log.pending() != 1 {
        return Err(format!("{} receipts on disk after the crash, want 1", log.pending()).into());
    }
    let price: u128 = accepted.amount.parse()?;
    let before = book(net, &p.revenue, &shard)?;
    // The reporter commits a batch only once its `credit` is final
    // (REV-L3-F8): on anvil, blocks are mined until it is; on Base Sepolia,
    // L2 finality takes about a quarter hour.
    // Its credits in flight are kept beside the receipts (REV-L4-F1).
    let reporter = Reporter::new(settle.shared_sender(), p.revenue, log.clone())
        .with_state_file(o.deploy_dir(&p.revenue).join("credits-in-flight.jsonl"))?;
    let t = Instant::now();
    let posted = loop {
        let p = reporter.run_once().await?;
        if !p.waiting {
            break p;
        }
        if net.chain.anvil {
            let b = net.chain.block()?;
            net.chain.reach(b + 66)?;
        } else {
            if t.elapsed() > Duration::from_secs(3_600) {
                return Err("the credit did not reach the finalized block within an hour".into());
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    };
    if posted.applied != 1 || log.pending() != 0 {
        return Err(format!(
            "the reporter applied {} of {} receipts ({} left)",
            posted.applied,
            posted.posted,
            log.pending()
        )
        .into());
    }
    let after = book(net, &p.revenue, &shard)?;
    let per_sale = split(
        price,
        &RevenueSplit::default(),
        &Recipients {
            miners: after.miners,
            validators: after.validators,
            label_set: after.label_set,
        },
    )
    .per_miner;
    if after.revenue != before.revenue + price || after.per_miner != 2 * per_sale {
        return Err(format!(
            "after the credit the shard booked {} (want {}), {} per miner (want {})",
            after.revenue,
            before.revenue + price,
            after.per_miner,
            2 * per_sale
        )
        .into());
    }
    // The first miner claims its share of both sales and withdraws it.
    let m0 = net.miners[0].address;
    let usdc_of = |net: &Net| -> Res<u128> {
        first_u128(
            &net.chain
                .call(&p.usdc, "balanceOf(address)(uint256)", &[ax(&m0)])?,
        )
    };
    let held = usdc_of(net)?;
    net.chain.send(
        &m0,
        &p.revenue,
        "claim(uint64,bytes32,address)",
        &[ROUND.to_string(), hx(&shard), ax(&m0)],
    )?;
    let withdraw_tx = net.chain.send(&m0, &p.revenue, "withdraw()", &[])?;
    let got = usdc_of(net)? - held;
    if got != after.per_miner {
        return Err(format!(
            "miner 0 withdrew {got} micro-USDC, want {} (its share of two sales)",
            after.per_miner
        )
        .into());
    }
    let tx = posted.tx.clone().unwrap_or_default();
    let lines = vec![
        format!(
            "6b. receipt path (REV-11): an API with a standard facilitator served the row again; transfer {} moved {price} micro-USDC into OnyxRevenue unallocated; the receipt was on disk before the answer",
            settled.transaction
        ),
        format!(
            "   the API was stopped right after answering (a crash); reopened, its log held the receipt; the reporter posted it: credit tx {tx} applied 1 of 1"
        ),
        format!(
            "   the shard has now earned {} micro-USDC from 2 sales; miner 0 claimed and withdrew {got} micro-USDC ({} per sale x 2; split {}/{} miners/validators, label set {}) in tx {withdraw_tx}",
            after.revenue,
            per_sale,
            after.miners,
            after.validators,
            if after.label_set { "pinned" } else { "none" }
        ),
    ];
    let report = json!({
        "facilitator": "standard x402 (token transferWithAuthorization)",
        "transfer_tx": settled.transaction,
        "receipt_on_disk_after_crash": true,
        "credit_tx": tx,
        "applied": posted.applied,
        "shard_revenue_micro_usdc": after.revenue.to_string(),
        "per_miner_micro_usdc": after.per_miner.to_string(),
        "miner0_withdrew_micro_usdc": got.to_string(),
    });
    Ok((lines, report))
}

// ------------------------------------------------------------------ dispute run

/// Round 2 (`--dispute`), on the 3-block fixture shard: miner B reveals a
/// made-up root. Assigned validators disagree, so the shard is not taken on
/// unanimity: it goes Pending. A rerun case draws a panel from the standby
/// validators; each member reruns the recipe and votes; the majority decides
/// the true root, B loses its bond and A is paid.
fn dispute_round(net: &mut Net) -> Res<(Vec<String>, Value)> {
    const R2: u64 = 2;
    let d = data(None, true)?;
    let (mut works, recipe_secs) = mine(&net.miners, R2, &d)?;
    net.mark("r2.mine")?;
    let honest = works[0].commitment.root;
    let cheater = net.miners[1].address;
    works[1] = forged(&works[1], &cheater);
    let p = net.play(R2, &d, &works, recipe_secs)?;
    let mut out = Vec::new();
    let disagree = p
        .verdicts
        .iter()
        .filter(|(_, m, v)| *m == cheater && matches!(v, Verdict::Disagree { .. }))
        .count();
    let status = net.status(R2, &p.shard)?;
    if status != PENDING {
        return Err(format!("dispute round: status {status}, want Pending (2)").into());
    }
    out.push(format!(
        "7. dispute run (round 2, {}): miner B revealed a made-up root; {disagree} validator(s) disagreed; the shard is Pending, not final",
        d.label
    ));
    let (coord, dsp) = (net.coord, net.dep.disputes);
    let r = R2.to_string();
    net.chain.send(
        &coord,
        &dsp,
        "openRerun(uint64,bytes32)",
        &[r.clone(), hx(&p.shard)],
    )?;
    let case = parse_h32(&net.chain.call(
        &dsp,
        "caseOf(uint64,bytes32)(bytes32)",
        &[r.clone(), hx(&p.shard)],
    )?)?;
    net.chain
        .send(&coord, &dsp, "draw(bytes32)", &[hx(&case)])?;
    let panel = net.chain.call(
        &dsp,
        "panel(bytes32,uint32)(address[],uint64,uint64,address)",
        &[hx(&case), "0".into()],
    )?;
    let mut lines = panel.lines();
    let members: Vec<[u8; 20]> = lines
        .next()
        .unwrap_or("[]")
        .trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(parse_addr)
        .collect::<Res<_>>()?;
    let drawn_at: u64 = first_u128(lines.next().unwrap_or("0"))?.try_into()?;
    net.mark("r2.rerun_case_and_draw")?;
    let dc = p.cfg.dispute;
    if members.is_empty() {
        // No eligible panel: the coordinator reruns and posts the root (q13 a).
        net.chain.send(
            &coord,
            &dsp,
            "arbitrate(bytes32,bytes32)",
            &[hx(&case), hx(&honest)],
        )?;
        out.push(
            "   no panel could be drawn: the coordinator reran the shard and posted the root"
                .into(),
        );
    } else {
        let mut votes = Vec::new();
        for m in &members {
            let vi = net
                .validators
                .iter()
                .position(|v| v.address == *m)
                .ok_or("panel member is not a demo validator")?;
            let v = &net.validators[vi];
            // Each member reruns the recipe itself.
            let root = match v.judge(&d.spec, d.source.clone(), &honest, &d.reg, &*d.cas, &d.host) {
                Verdict::Agree => honest,
                Verdict::Disagree { root } => root,
                _ => return Err("a panel member could not rerun the shard".into()),
            };
            let vote = v.panel_vote(&case, 0, &root);
            let salt = v.panel_salt(&case, 0);
            net.chain.send(
                m,
                &dsp,
                "commitVote(bytes32,bytes32)",
                &[hx(&case), hx(&vote.commit)],
            )?;
            votes.push((*m, root, salt));
        }
        net.chain.reach(drawn_at + dc.panel_commit_window)?;
        for (m, root, salt) in &votes {
            net.chain.send(
                m,
                &dsp,
                "revealVote(bytes32,bytes32,bytes32)",
                &[hx(&case), hx(root), hx(salt)],
            )?;
        }
        net.chain
            .reach(drawn_at + dc.panel_commit_window + dc.panel_reveal_window + dc.appeal_window)?;
        net.chain
            .send(&coord, &dsp, "resolve(bytes32)", &[hx(&case)])?;
        out.push(format!(
            "   a panel of {} standby validators was drawn; each reran the recipe and voted for the true root; no appeal",
            members.len()
        ));
    }
    net.mark("r2.panel_votes_and_resolve")?;
    let before: Vec<u128> = [net.miners[0].address, cheater]
        .iter()
        .map(|a| net.credit(a))
        .collect::<Res<_>>()?;
    let v_before: Vec<u128> = net
        .validators
        .iter()
        .map(|v| net.credit(&v.address))
        .collect::<Res<_>>()?;
    net.chain.send(
        &coord,
        &dsp,
        "settleCase(bytes32,uint256)",
        &[hx(&case), "64".into()],
    )?;
    net.mark("r2.settle_case")?;
    let after: Vec<u128> = [net.miners[0].address, cheater]
        .iter()
        .map(|a| {
            first_u128(
                &net.chain
                    .call(&net.st(), "balanceOf(address)(uint256)", &[ax(a)])?,
            )
        })
        .collect::<Res<_>>()?;
    let status = net.status(R2, &p.shard)?;
    let fin = net.chain.call(
        &net.st(),
        "getFinalizedRoot(uint64,bytes32)(bytes32,bytes32,uint64)",
        &[r, hx(&p.shard)],
    )?;
    let final_root = parse_h32(fin.lines().next().unwrap_or(""))?;
    if status != FINALIZED || final_root != honest {
        return Err(format!("dispute round: status {status}, root {final_root}").into());
    }
    // Validators, checked against the contract rules (M8-L56-F2): a right
    // verdict earns `validator_fee` once per distinct revealed root it judged
    // (A's true root and B's made-up root are two keys); each panel member
    // that revealed the decided root earns `panel_fee` from the round budget.
    let forged_root = works[1].commitment.root;
    let vfee = p.cfg.validation.validator_fee;
    for (i, v) in net.validators.iter().enumerate() {
        let mut keys = std::collections::BTreeSet::new();
        for (vi, m, verdict) in &p.verdicts {
            if *vi != i {
                continue;
            }
            let right = match verdict {
                Verdict::Agree => *m != cheater,
                Verdict::Disagree { root } => *m == cheater && *root == honest,
                _ => false,
            };
            if right {
                keys.insert(if *m == cheater { forged_root } else { honest });
            }
        }
        let mut want = vfee * keys.len() as u128;
        if members.contains(&v.address) {
            want += dc.panel_fee;
        }
        let got = net.credit(&v.address)? - v_before[i];
        if got != want {
            return Err(format!(
                "dispute round: validator {i} credited {} USDC, want {}",
                usd(got),
                usd(want)
            )
            .into());
        }
    }
    // Credits checked, not just printed (M8-L55-F2).
    let reward = p.cfg.validation.miner_reward;
    let (a_got, b_got) = (after[0] - before[0], after[1] - before[1]);
    if a_got != BOND + reward || b_got != 0 {
        return Err(format!(
            "dispute payouts: A +{}, B +{}; want A +{} and B +0",
            usd(a_got),
            usd(b_got),
            usd(BOND + reward)
        )
        .into());
    }
    out.push(format!(
        "   decided: the true root {}... is final; checked: miner A credited +{} USDC (bond + reward), miner B +0 (its {} USDC bond was taken); every validator's verdict and panel fees match the rules",
        &hx(&honest)[..10],
        usd(a_got),
        usd(BOND)
    ));
    let report = json!({
        "round": R2,
        "data": d.label,
        "shard": hx(&p.shard),
        "honest_root": hx(&honest),
        "forged_root": hx(&forged_root),
        "validators_disagreed": disagree,
        "status_before_case": "Pending",
        "panel_size": members.len(),
        "panel_fee_micro_usdc": dc.panel_fee.to_string(),
        "final_root": hx(&final_root),
        "honest_miner_credit_micro_usdc": a_got.to_string(),
        "cheater_credit_micro_usdc": b_got.to_string(),
        "cheater_bond_taken_micro_usdc": BOND.to_string(),
    });
    Ok((out, report))
}

// ------------------------------------------------------------------ main

// ------------------------------------------------------------------ keys and budget

/// `--key-address NAME --keys DIR`, nothing else: print the address of the
/// named key (created if missing). The key itself is never printed.
fn key_address(args: &[String]) -> Res<()> {
    let (mut name, mut dir) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--key-address" => name = it.next().cloned(),
            "--keys" => dir = it.next().map(PathBuf::from),
            other => return Err(format!("--key-address takes only --keys; got {other:?}").into()),
        }
    }
    let name = name.ok_or("--key-address needs a name")?;
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("a key name is letters, digits and dashes".into());
    }
    let k = load_or_create_key(&dir.ok_or("--keys <dir> is required")?, &name)?;
    println!("{}", ax(&exact_evm::address(&k).ok_or("key")?));
    Ok(())
}

/// Read `<dir>/<name>.key` (hex), creating it from the OS's randomness if
/// missing: the directory 0700, the file 0600. The key is never printed.
fn load_or_create_key(dir: &Path, name: &str) -> Res<[u8; 32]> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let path = dir.join(format!("{name}.key"));
    if !path.exists() {
        let mut k = [0u8; 32];
        loop {
            std::fs::File::open("/dev/urandom")?.read_exact(&mut k)?;
            if exact_evm::address(&k).is_some() {
                break;
            }
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        f.write_all(hex::encode(k).as_bytes())?;
    }
    let mode = std::fs::metadata(&path)?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "{} is readable by others (mode {mode:o}): refused",
            path.display()
        )
        .into());
    }
    Ok(parse_h32(&std::fs::read_to_string(&path)?)?.0)
}

/// The deployer key from `ONYX_DEPLOYER_KEY` (the script sets it from
/// `.env`'s BASE_SEPOLIA_PRIVATE_KEY, or an anvil dev key in fork mode).
/// Read once, then removed from this process's environment, so no child
/// (`cast`) inherits it (M8-L58-F7).
fn deployer_key() -> Res<[u8; 32]> {
    static KEY: std::sync::OnceLock<Option<[u8; 32]>> = std::sync::OnceLock::new();
    let k = KEY.get_or_init(|| {
        let v = std::env::var("ONYX_DEPLOYER_KEY").ok();
        std::env::remove_var("ONYX_DEPLOYER_KEY");
        let k = parse_h32(v.as_deref()?).ok()?;
        exact_evm::address(&k.0).map(|_| k.0)
    });
    k.ok_or_else(|| "ONYX_DEPLOYER_KEY is not set to a valid 32-byte hex key".into())
}

/// `scheme://host` of a node URL: a path or query can hold an API key, so
/// errors and logs show only this (M8-L58-F7).
fn redact_url(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let tail = if rest.len() > host.len() { "/…" } else { "" };
    format!("{scheme}://{host}{tail}")
}

use onyx_source_btc::budget::{
    BUDGET_MARGIN, GAS_COORDINATOR, GAS_DEPLOY, GAS_LEVEL2_COORDINATOR, GAS_LEVEL2_OPERATOR,
    GAS_MINER, GAS_OPERATOR, GAS_OWNER, GAS_VALIDATOR, TXS_COORDINATOR, TXS_LEVEL2_COORDINATOR,
    TXS_LEVEL2_OPERATOR, TXS_LEVEL2_OWNER, TXS_MINER, TXS_OPERATOR, TXS_OWNER, TXS_VALIDATOR,
};
/// Test USDC the payer needs (x402 is gasless for the payer).
const PAYER_USDC: u128 = 4 * PRICE_MICRO_USDC as u128;

/// What a fork or testnet run needs, at the node's current fees.
struct Plan {
    fee_cap: u128,
    l1_per_tx: u128,
    miner_wei: u128,
    validator_wei: u128,
    /// Gas for the API operator's settles (M8-L58-F5).
    operator_wei: u128,
    /// Gas for the registry owner's registrations (REG-R1-F3).
    owner_wei: u128,
    /// The registry owner's registrations.
    owner_txs: u128,
    /// Coordinator work plus the ETH it sends the actors.
    deployer_after_deploy: u128,
    deploy_wei: u128,
}

impl Plan {
    fn total(&self) -> u128 {
        self.deploy_wei + self.deployer_after_deploy
    }
}

/// `level2`: the fork's `--level2` run (ROADMAP B1) adds the level-2 round's
/// coordinator, owner and operator transactions.
fn plan(chain: &Chain, level2: bool) -> Res<Plan> {
    let base = qty(&chain.rpc("eth_getBlockByNumber", json!(["latest", false]))?["baseFeePerGas"])?;
    let tip = chain
        .rpc("eth_maxPriorityFeePerGas", json!([]))
        .ok()
        .and_then(|v| qty(&v).ok())
        .unwrap_or(1_000_000);
    let fee_cap = base * 2 + tip;
    // A 600-byte transaction's L1 fee bound (Fjord oracle); 0 off OP Stack.
    let l1_per_tx = chain
        .run(&[
            "call".into(),
            GAS_PRICE_ORACLE.into(),
            "getL1FeeUpperBound(uint256)(uint256)".into(),
            "600".into(),
        ])
        .ok()
        .and_then(|o| first_u128(&o).ok())
        .unwrap_or(0);
    let role = |gas: u128, txs: u128| BUDGET_MARGIN * (gas * fee_cap + txs * l1_per_tx);
    let miner_wei = role(GAS_MINER, TXS_MINER);
    let validator_wei = role(GAS_VALIDATOR, TXS_VALIDATOR);
    let l2 = u128::from(level2);
    let operator_wei = role(
        GAS_OPERATOR + l2 * GAS_LEVEL2_OPERATOR,
        TXS_OPERATOR + l2 * TXS_LEVEL2_OPERATOR,
    );
    let owner_txs = TXS_OWNER + l2 * TXS_LEVEL2_OWNER;
    let owner_wei = role(owner_txs * (GAS_OWNER / TXS_OWNER), owner_txs);
    let funding =
        MINERS as u128 * miner_wei + VALIDATORS as u128 * validator_wei + operator_wei + owner_wei;
    Ok(Plan {
        fee_cap,
        l1_per_tx,
        miner_wei,
        validator_wei,
        operator_wei,
        owner_wei,
        deployer_after_deploy: funding
            + role(
                GAS_COORDINATOR + l2 * GAS_LEVEL2_COORDINATOR,
                TXS_COORDINATOR + l2 * TXS_LEVEL2_COORDINATOR,
            ),
        owner_txs,
        // Deploy transactions are large: price their L1 data generously.
        deploy_wei: BUDGET_MARGIN * GAS_DEPLOY * fee_cap + 200 * l1_per_tx,
    })
}

fn eth(wei: u128) -> String {
    format!(
        "{}.{:09}",
        wei / 1_000_000_000_000_000_000,
        wei % 1_000_000_000_000_000_000 / 1_000_000_000
    )
}

fn print_plan(p: &Plan) {
    println!(
        "Budget at the current fee cap {} gwei/gas (L1 data fee bound {} wei per tx), margin x{BUDGET_MARGIN}:",
        eth(p.fee_cap * 1_000_000_000),
        p.l1_per_tx
    );
    println!("  deploy (3 scripts)           {} ETH", eth(p.deploy_wei));
    println!(
        "  {MINERS} miners x {} ETH, {VALIDATORS} validators x {} ETH (sent by the deployer)",
        eth(p.miner_wei),
        eth(p.validator_wei)
    );
    println!(
        "  API operator (settles only)  {} ETH (sent by the deployer)",
        eth(p.operator_wei)
    );
    println!(
        "  registry owner ({} registrations) {} ETH (sent by the deployer)",
        p.owner_txs,
        eth(p.owner_wei)
    );
    println!(
        "  deployer after deploy        {} ETH (coordinator work + actor funding)",
        eth(p.deployer_after_deploy)
    );
    println!("  total the deployer needs     {} ETH", eth(p.total()));
    println!(
        "  test USDC the deployer needs {} USDC (the customer's payments; bonds use the free mock token)",
        usd(PAYER_USDC)
    );
}

/// Gas and fees of forge's deploy transactions (broadcast receipts).
fn deploy_spend(paths: &[&Path]) -> Res<Spend> {
    let mut sp = Spend::default();
    for p in paths {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(p)?)?;
        for r in v["receipts"].as_array().ok_or("no receipts")? {
            let used = qty(&r["gasUsed"])?;
            sp.txs += 1;
            sp.gas += used;
            sp.l2_wei += used * qty(&r["effectiveGasPrice"])?;
            sp.l1_wei += r.get("l1Fee").and_then(|f| qty(f).ok()).unwrap_or(0);
        }
    }
    Ok(sp)
}

/// The `OnyxRegistry` forge's `DeployRegistry` broadcast created.
fn registry_address(path: &Path) -> Res<[u8; 20]> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let tx = v["transactions"]
        .as_array()
        .ok_or("no transactions")?
        .iter()
        .find(|t| t["contractName"] == "OnyxRegistry" && t["transactionType"] == "CREATE")
        .ok_or("OnyxRegistry not deployed")?;
    parse_addr(tx["contractAddress"].as_str().ok_or("no address")?)
}

fn revenue_address(path: &Path) -> Res<[u8; 20]> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let tx = v["transactions"]
        .as_array()
        .ok_or("no transactions")?
        .iter()
        .find(|t| t["contractName"] == "OnyxRevenue" && t["transactionType"] == "CREATE")
        .ok_or("OnyxRevenue not deployed")?;
    parse_addr(tx["contractAddress"].as_str().ok_or("no address")?)
}

/// Fund every actor with its planned ETH, and the customer with test USDC
/// (fork: minted by impersonating Circle's master minter inside the fork;
/// testnet: sent from the deployer's own test USDC).
fn fund(
    net: &mut Net,
    p: &Plan,
    payer: &[u8; 20],
    operator: &[u8; 20],
    owner: &[u8; 20],
    usdc: &[u8; 20],
    mode: Mode,
) -> Res<()> {
    let coord = net.coord;
    let actors: Vec<([u8; 20], u128)> = net
        .miners
        .iter()
        .map(|m| (m.address, p.miner_wei))
        .chain(net.validators.iter().map(|v| (v.address, p.validator_wei)))
        .chain(std::iter::once((*operator, p.operator_wei)))
        .chain(std::iter::once((*owner, p.owner_wei)))
        .collect();
    let have = first_u128(
        &net.chain
            .call(usdc, "balanceOf(address)(uint256)", &[ax(payer)])?,
    )?;
    // Re-check both balances right before sending anything (M8-L58-F6):
    // a shortfall stops the run before any ETH leaves the deployer.
    if mode == Mode::Testnet {
        let need = PAYER_USDC.saturating_sub(have);
        let own = first_u128(&net.chain.call(
            usdc,
            "balanceOf(address)(uint256)",
            &[ax(&coord)],
        )?)?;
        if own < need {
            return Err(format!(
                "the deployer {} holds {} test USDC; the customer needs {} (get it from faucet.circle.com, Base Sepolia)",
                ax(&coord),
                usd(own),
                usd(need)
            )
            .into());
        }
        let mut eth_need = 0u128;
        for (a, want) in &actors {
            let h = qty(&net.chain.rpc("eth_getBalance", json!([ax(a), "latest"]))?)?;
            eth_need += want.saturating_sub(h);
        }
        let own_eth = qty(&net
            .chain
            .rpc("eth_getBalance", json!([ax(&coord), "latest"]))?)?;
        if own_eth < eth_need {
            return Err(format!(
                "the deployer holds {} ETH; funding the actors needs {} ETH",
                eth(own_eth),
                eth(eth_need)
            )
            .into());
        }
    }
    for (a, want) in actors {
        let h = qty(&net.chain.rpc("eth_getBalance", json!([ax(&a), "latest"]))?)?;
        if h < want {
            net.chain.send_value(&coord, &a, want - h)?;
        }
    }
    if have >= PAYER_USDC {
        return Ok(());
    }
    match mode {
        Mode::Fork => {
            let minter = parse_addr(&net.chain.call(usdc, "masterMinter()(address)", &[])?)?;
            net.chain
                .rpc("anvil_impersonateAccount", json!([ax(&minter)]))?;
            net.chain.rpc(
                "anvil_setBalance",
                json!([ax(&minter), "0xde0b6b3a7640000"]),
            )?;
            net.chain.send_unlocked(
                &minter,
                usdc,
                "configureMinter(address,uint256)",
                &[ax(&coord), PAYER_USDC.to_string()],
            )?;
            net.chain
                .rpc("anvil_stopImpersonatingAccount", json!([ax(&minter)]))?;
            net.chain.send(
                &coord,
                usdc,
                "mint(address,uint256)",
                &[ax(payer), PAYER_USDC.to_string()],
            )?;
        }
        Mode::Testnet => {
            let own = first_u128(&net.chain.call(
                usdc,
                "balanceOf(address)(uint256)",
                &[ax(&coord)],
            )?)?;
            if own < PAYER_USDC - have {
                return Err(format!(
                    "the deployer {} holds {} test USDC; the customer needs {} (get it from faucet.circle.com, Base Sepolia)",
                    ax(&coord),
                    usd(own),
                    usd(PAYER_USDC - have)
                )
                .into());
            }
            net.chain.send(
                &coord,
                usdc,
                "transfer(address,uint256)",
                &[ax(payer), (PAYER_USDC - have).to_string()],
            )?;
        }
        Mode::Local => {}
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Res<()> {
    let t0 = Instant::now();
    // `--key-address NAME --keys DIR`: make (or read) a named key in the keys
    // folder and print its address only (the testnet script needs the
    // registry owner's address before deploying, REG-R1-F3).
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.iter().any(|a| a == "--key-address") {
        return key_address(&raw);
    }
    let o = opts()?;
    if o.mode != Mode::Local {
        // Take the key out of the environment before any child starts.
        let _ = deployer_key();
    }
    match o.mode {
        Mode::Local | Mode::Fork => {
            loopback(&o.rpc)?;
        }
        Mode::Testnet => {
            if !o.rpc.starts_with("https://") || o.rpc.contains('@') {
                return Err("testnet mode needs an https:// RPC URL without credentials".into());
            }
        }
    }
    let cast = std::env::var_os("ONYX_CAST")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cast"));
    // Fail before a minute of work if the API port is taken (M8-L55-F6).
    std::net::TcpListener::bind(("127.0.0.1", o.port))
        .map_err(|e| format!("API port {} is not free: {e}", o.port))?;
    let work = match (&o.work, &o.accounts) {
        (Some(w), _) => w.clone(),
        (None, Some(a)) => a
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or("--accounts must sit in the run's temp dir")?
            .to_path_buf(),
        (None, None) => return Err("--accounts or --work is required".into()),
    }
    .canonicalize()?;
    // cast loads a `.env` from its working directory and the project (git)
    // root: the work dir must sit outside any repo and see no `.env`
    // (M8-L56-F5).
    if let Some(bad) = work
        .ancestors()
        .find(|d| d.join(".git").exists() || d.join(".env").exists())
    {
        return Err(format!(
            "the run's temp dir {} is inside {} (a repo or a .env): refused",
            work.display(),
            bad.display()
        )
        .into());
    }
    let mut chain = Chain {
        cast,
        rpc: o.rpc.clone(),
        work,
        txs: 0,
        chain_id: 0,
        anvil: false,
        keys: BTreeMap::new(),
        spent: BTreeMap::new(),
        next_nonce: BTreeMap::new(),
        seen_block: 0,
        log: Vec::new(),
        fake: None,
    };
    let chain_id: u64 = chain.run(&["chain-id".into()])?.parse()?;
    let client = chain
        .rpc("web3_clientVersion", json!([]))?
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    let anvil = client.contains("anvil");
    match (o.mode, chain_id, anvil) {
        (Mode::Local, ANVIL, true) | (Mode::Fork, BASE_SEPOLIA_ID, true) => {}
        (Mode::Testnet, BASE_SEPOLIA_ID, false) => {}
        (m, id, a) => {
            return Err(format!(
                "{m:?} mode refuses chain {id} ({}): local wants anvil 31337, fork an anvil fork of Base Sepolia 84532, testnet Base Sepolia 84532",
                if a { "an anvil" } else { "not an anvil" }
            )
            .into())
        }
    }
    chain.chain_id = chain_id;
    chain.anvil = anvil;
    let result = run(&o, chain, t0).await;
    result
}

/// Everything after the node checks; the run log is written either way.
async fn run(o: &Opts, mut chain: Chain, t0: Instant) -> Res<()> {
    if o.mode != Mode::Local {
        let budget = plan(&chain, o.level2)?;
        print_plan(&budget);
        // The API and the self-settle path talk to the node through
        // onyx-api's own client (HTTPS on testnet): check it answers.
        let id = HttpJsonRpc::new(o.rpc.clone(), node_post(o.mode))
            .call("eth_chainId", json!([]))
            .await?;
        if qty(&id)? != u128::from(chain.chain_id) {
            return Err(format!("onyx-api's node client sees chain {id}").into());
        }
        if o.plan {
            // With a deployer key, say whether it holds enough (before any
            // deploy, so a short balance stops the run early).
            println!(
                "  onyx-api's node client reaches the node (chain {})",
                chain.chain_id
            );
            if let Ok(k) = deployer_key() {
                let a = exact_evm::address(&k).ok_or("deployer key")?;
                let have = qty(&chain.rpc("eth_getBalance", json!([ax(&a), "latest"]))?)?;
                let usdc = first_u128(&chain.run(&[
                    "call".into(),
                    USDC_BASE_SEPOLIA.into(),
                    "balanceOf(address)(uint256)".into(),
                    ax(&a),
                ])?)?;
                println!(
                    "  deployer {} holds {} ETH and {} test USDC",
                    ax(&a),
                    eth(have),
                    usd(usdc)
                );
                if o.mode == Mode::Testnet {
                    let mut short = Vec::new();
                    if have < budget.total() {
                        short.push(format!(
                            "{} ETH more (Base Sepolia faucet)",
                            eth(budget.total() - have)
                        ));
                    }
                    if usdc < PAYER_USDC {
                        short.push(format!(
                            "{} test USDC more (faucet.circle.com, Base Sepolia)",
                            usd(PAYER_USDC - usdc)
                        ));
                    }
                    if !short.is_empty() {
                        return Err(format!("the deployer needs {}", short.join(" and ")).into());
                    }
                    println!("  the deployer holds enough; run again with --yes to send");
                }
            }
            return Ok(());
        }
    } else if o.plan {
        return Err("--plan is for fork and testnet modes".into());
    }
    let mut dep = deployed(&o.broadcast)?;
    if let Some(g) = &o.registry_broadcast {
        dep.registry = Some(registry_address(g)?);
    }
    let (coord, miner_keys, validator_keys, paid) = match o.mode {
        Mode::Local => {
            let accts = accounts(o.accounts.as_deref().ok_or("--accounts is required")?)?;
            if accts.len() < 1 + MINERS + VALIDATORS {
                return Err(format!("need {} anvil accounts", 1 + MINERS + VALIDATORS).into());
            }
            (
                accts[0].0,
                accts[1..=MINERS]
                    .iter()
                    .map(|(_, k)| *k)
                    .collect::<Vec<_>>(),
                accts[1 + MINERS..1 + MINERS + VALIDATORS]
                    .iter()
                    .map(|(_, k)| *k)
                    .collect::<Vec<_>>(),
                None,
            )
        }
        Mode::Fork | Mode::Testnet => {
            let dir = o.keys.as_deref().ok_or("--keys <dir> is required")?;
            let dk = deployer_key()?;
            let deployer = exact_evm::address(&dk).ok_or("deployer key")?;
            chain.keys.insert(deployer, dk);
            let mut get = |name: String| -> Res<[u8; 32]> {
                let k = load_or_create_key(dir, &name)?;
                chain.keys.insert(exact_evm::address(&k).ok_or("key")?, k);
                Ok(k)
            };
            let miners = (0..MINERS)
                .map(|i| get(format!("miner-{i}")))
                .collect::<Res<Vec<_>>>()?;
            let validators = (0..VALIDATORS)
                .map(|i| get(format!("validator-{i}")))
                .collect::<Res<Vec<_>>>()?;
            let payer_key = load_or_create_key(dir, "customer")?;
            // The API's hot key: a key of its own, made OnyxRevenue's
            // reporter below and funded with gas only. It holds no USDC and
            // owns nothing (M8-L58-F5).
            let operator_key = get("operator".into())?;
            let registry_owner_key = get("registry-owner".into())?;
            // ROADMAP B3 (THREATS REVLOG-4): the deployment's own hot
            // receipt key, one per OnyxRegistry deployment, kept with the
            // actor keys (0600); the run refuses to start if it is any other
            // key of the run (the reporter above all). Made only for a run
            // that plays level 2 (`--level2`, fork only until B6).
            let receipt_key = match dep.registry.filter(|_| o.level2) {
                Some(reg) => {
                    let name = format!("receipt-signer-{}", hex::encode(reg));
                    load_or_create_key(dir, &name)?;
                    let k = ReceiptKey::from_file(&dir.join(format!("{name}.key")))?;
                    // Review B2-R2-1: never an anvil dev key (public).
                    if k.is_well_known() {
                        return Err(format!(
                            "{name}.key holds a public anvil dev key: refused (THREATS REVLOG-4)"
                        )
                        .into());
                    }
                    let addr = |k: &[u8; 32]| exact_evm::address(k).unwrap_or_default();
                    let mut named = vec![
                        ("the deployer (the coordinator)", deployer),
                        ("the reporter (the API operator)", addr(&operator_key)),
                        ("the registry owner", addr(&registry_owner_key)),
                        ("the customer", addr(&payer_key)),
                    ];
                    named.extend(miners.iter().map(|m| ("a miner", addr(m))));
                    named.extend(validators.iter().map(|v| ("a validator", addr(v))));
                    named.extend(
                        onyx_source_fiscaldata::round::rehearsal_pool()
                            .iter()
                            .map(|o| ("an observer", o.address)),
                    );
                    k.check_distinct(&named)?;
                    Some(k)
                }
                None => None,
            };
            let rb = o
                .revenue_broadcast
                .as_deref()
                .ok_or("--revenue-broadcast is required")?;
            let paid = Paid {
                revenue: revenue_address(rb)?,
                usdc: parse_addr(USDC_BASE_SEPOLIA)?,
                payer_key,
                operator_key,
                registry_owner_key,
                receipt_key,
            };
            // REV-4: the deployment manifest a client pins `payTo` from (a
            // real client gets it from the project's published deploy
            // output; this run writes it right after deploying), with the
            // runtime code hash of the deployed OnyxRevenue.
            let code = parse_h32(&chain.run(&["codehash".into(), ax(&paid.revenue)])?)?;
            let sd = o.deploy_dir(&paid.revenue);
            std::fs::create_dir_all(&sd)?;
            std::fs::write(
                sd.join("deployment.json"),
                serde_json::to_string_pretty(&json!({
                    "chainId": chain.chain_id,
                    "revenue": ax(&paid.revenue),
                    "usdc": USDC_BASE_SEPOLIA,
                    "revenueCodeHash": hx(&code),
                }))? + "\n",
            )?;
            // The deploy's own cost, from forge's receipts.
            let mut spent: Vec<&Path> = vec![&o.broadcast, rb];
            if let Some(g) = &o.registry_broadcast {
                spent.push(g);
            }
            let ds = deploy_spend(&spent)?;
            println!(
                "  deploy: {} txs, {} L2 gas, {} ETH L2 + {} ETH L1 data",
                ds.txs,
                ds.gas,
                eth(ds.l2_wei),
                eth(ds.l1_wei)
            );
            chain.spent.insert(deployer, ds);
            (deployer, miners, validators, Some(paid))
        }
    };
    println!(
        "Onyx M8 run, {:?} mode, chain {} ({})",
        o.mode,
        chain.chain_id,
        if chain.anvil { "anvil" } else { "Base Sepolia" }
    );
    println!(
        "  settlement {}  disputes {}",
        ax(&dep.settlement),
        ax(&dep.disputes)
    );
    if let Some(p) = &paid {
        println!(
            "  revenue    {} (x402 payTo)  usdc {}",
            ax(&p.revenue),
            ax(&p.usdc)
        );
    }
    let mut net = Net {
        chain,
        dep,
        coord,
        miners: miner_keys
            .iter()
            .map(|k| Miner::new(*k).map(|m| m.with_allowed_failures(allowed_failures())))
            .collect::<Result<_, _>>()?,
        validators: validator_keys
            .iter()
            .map(|k| Validator::new(*k))
            .collect::<Result<_, _>>()?,
        trace: Vec::new(),
        mark_txs: 0,
        mark_t: Instant::now(),
    };
    let mut out = play_all(o, &mut net, paid.as_ref(), t0).await;
    if o.sweep {
        // Even after a failed run: what the actor keys hold goes back.
        if let Err(e) = sweep(&mut net) {
            out = out.and(Err(e));
        }
    }
    if let Some(path) = &o.log {
        write_log(path, &net, o.mode)?;
        println!("  run log: {}", path.display());
    }
    out
}

/// `--sweep` (M8-L58-F8): send what each actor key holds in ETH back to the
/// deployer, keeping only the worst-case fee of that transfer. The
/// customer's leftover test USDC stays (it holds no ETH for gas).
fn sweep(net: &mut Net) -> Res<()> {
    let deployer = net.coord;
    let budget = plan(&net.chain, false)?;
    // send_signed's limit for a plain transfer (21,000 x 5/4) at twice the
    // current fee cap (the base fee can rise before the send), plus the L1
    // data fee bound.
    let reserve = 2 * 26_250 * budget.fee_cap + budget.l1_per_tx;
    let actors: Vec<[u8; 20]> = net
        .chain
        .keys
        .keys()
        .copied()
        .filter(|a| *a != deployer)
        .collect();
    let (mut n, mut total) = (0u32, 0u128);
    for a in actors {
        let have = qty(&net.chain.rpc("eth_getBalance", json!([ax(&a), "latest"]))?)?;
        if have > reserve {
            net.chain.send_value(&a, &deployer, have - reserve)?;
            n += 1;
            total += have - reserve;
        }
    }
    println!(
        "  sweep: {n} actor keys returned {} ETH to the deployer",
        eth(total)
    );
    Ok(())
}

/// Every transaction with an explorer link (testnet) and the spend per sender.
fn write_log(path: &Path, net: &Net, mode: Mode) -> Res<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    let link = |h: &str| match mode {
        Mode::Testnet => format!("https://sepolia.basescan.org/tx/{h}"),
        _ => h.to_string(),
    };
    writeln!(f, "# Onyx run, {mode:?} mode, unix {}", now_unix())?;
    writeln!(
        f,
        "settlement {} disputes {}",
        ax(&net.dep.settlement),
        ax(&net.dep.disputes)
    )?;
    for (what, h) in &net.chain.log {
        writeln!(f, "{what}\t{}", link(h))?;
    }
    for (a, sp) in &net.chain.spent {
        writeln!(
            f,
            "spent {} txs {} gas {} l2 {} wei l1 {} wei",
            ax(a),
            sp.txs,
            sp.gas,
            sp.l2_wei,
            sp.l1_wei
        )?;
    }
    Ok(())
}

async fn play_all(o: &Opts, net: &mut Net, paid: Option<&Paid>, t0: Instant) -> Res<()> {
    // The reserve round's inputs (ADR-0034 Amendment 1), read before anything
    // is sent: fork and testnet runs require them (ROADMAP A2), local mode
    // skips the round without them.
    let reserve_in = match &o.reserve_dir {
        Some(dir) => reserve::prepare(dir)?,
        None => None,
    };
    if reserve_in.is_none() && o.mode != Mode::Local {
        return Err(format!(
            "{:?} mode settles and sells a reserve shard: the reserve tail block files 971,192-971,199 are missing (--reserve-dir, default fixtures-live/tail; ONYX_RESERVE_DIR in the script)",
            o.mode
        )
        .into());
    }
    if let Some(p) = paid {
        let budget = plan(&net.chain, o.level2)?;
        if o.mode == Mode::Fork {
            // Give the deployer exactly the planned post-deploy budget: the
            // run then proves the plan suffices.
            net.chain.rpc(
                "anvil_setBalance",
                json!([
                    ax(&net.coord),
                    format!("0x{:x}", budget.deployer_after_deploy)
                ]),
            )?;
        } else {
            let have = qty(&net
                .chain
                .rpc("eth_getBalance", json!([ax(&net.coord), "latest"]))?)?;
            if have < budget.deployer_after_deploy {
                return Err(format!(
                    "the deployer {} holds {} ETH; this run needs {} ETH after the deploy",
                    ax(&net.coord),
                    eth(have),
                    eth(budget.deployer_after_deploy)
                )
                .into());
            }
        }
        let payer = exact_evm::address(&p.payer_key).ok_or("payer key")?;
        let operator = exact_evm::address(&p.operator_key).ok_or("operator key")?;
        let owner = exact_evm::address(&p.registry_owner_key).ok_or("registry owner key")?;
        fund(net, &budget, &payer, &operator, &owner, &p.usdc, o.mode)?;
        let coord = net.coord;
        net.chain
            .send(&coord, &p.revenue, "setReporter(address)", &[ax(&operator)])?;
        println!(
            "0. funded {MINERS} miners, {VALIDATORS} validators and the API operator with ETH for gas; the customer holds {} test USDC; the operator {} is OnyxRevenue's reporter",
            usd(PAYER_USDC),
            ax(&operator)
        );
    }
    // 1. Setup.
    net.setup()?;
    net.mark("setup")?;
    println!(
        "1. setup: coordinator bond {} USDC, {VALIDATORS} validators staked {} USDC each ({} txs)",
        usd(STAKE),
        usd(STAKE),
        net.chain.txs
    );

    // 2-5. One honest round on the live blocks.
    let d = data(
        o.live_dir.as_deref().map(|p| (p, o.live_from, o.live_to)),
        false,
    )?;
    if !d.label.starts_with("live") {
        println!("  (no live block files: using the committed fixture)");
    }
    let (works, recipe_secs) = mine(&net.miners, ROUND, &d)?;
    net.mark("r1.mine")?;
    let root = works[0].commitment.root;
    let p = net.play(ROUND, &d, &works, recipe_secs)?;
    println!(
        "2. round {ROUND} opened ({}): shard {}..., tag computed by the contract and by Rust: equal",
        d.label,
        &hx(&p.shard)[..10]
    );
    println!(
        "3. {MINERS} miners ran the recipe ({:.1} s each), committed {} USDC bonds and revealed root {}...",
        p.recipe_secs,
        usd(BOND),
        &hx(&root)[..10]
    );
    let checkers: std::collections::BTreeSet<usize> =
        p.verdicts.iter().map(|(v, _, _)| *v).collect();
    let agrees = p
        .verdicts
        .iter()
        .filter(|(_, _, v)| matches!(v, Verdict::Agree))
        .count();
    println!(
        "4. seed {}... (contract and Rust equal); {} validators reran the recipe ({:.1} s): {agrees}/{} Agree; {} stand by for a dispute panel",
        &hx(&p.seed)[..10],
        checkers.len(),
        p.judge_secs,
        p.verdicts.len(),
        VALIDATORS - checkers.len()
    );
    let status = net.status(ROUND, &p.shard)?;
    if status != FINALIZED {
        return Err(format!(
            "shard status {status}, not Finalized (3); 2 = Pending (too few standbys this time: rerun the demo)"
        )
        .into());
    }
    let fin_block = net.chain.block()?;
    let final_at = fin_block + p.cfg.dispute.challenge_window;
    net.chain.reach(final_at)?;
    let (coord, dsp) = (net.coord, net.dep.disputes);
    let release_tx = net.chain.send(
        &coord,
        &dsp,
        "release(uint64,bytes32)",
        &[ROUND.to_string(), hx(&p.shard)],
    )?;
    net.mark("r1.challenge_window")?;
    // Standby validators gave no verdict and earn nothing (M8-L56-F2).
    for (i, v) in net.validators.iter().enumerate() {
        if !checkers.contains(&i) {
            let c = net.credit(&v.address)?;
            if c != 0 {
                return Err(format!("standby validator {i} was credited {} USDC", usd(c)).into());
            }
        }
    }
    let mut payees: Vec<[u8; 20]> = net.miners.iter().map(|m| m.address).collect();
    payees.extend(checkers.iter().map(|&v| net.validators[v].address));
    let got = net.withdraw_all(&payees)?;
    net.mark("r1.withdraw")?;
    let fees: u128 = got[MINERS..].iter().sum();
    // What actually moved, checked (M8-L55-F2): the earliest correct commit
    // (miner 0 commits first) gets bond and reward, the other its bond
    // (M8-L56-F1: in order, not sorted); each checker one fee.
    let reward = p.cfg.validation.miner_reward;
    let fee = p.cfg.validation.validator_fee;
    if got[..MINERS] != [BOND + reward, BOND] {
        return Err(format!(
            "miner payouts {:?}, want miner 0 (first commit) {} and miner 1 {}",
            got[..MINERS].iter().map(|g| usd(*g)).collect::<Vec<_>>(),
            usd(BOND + reward),
            usd(BOND)
        )
        .into());
    }
    if got[MINERS..].iter().any(|g| *g != fee) || fees != fee * checkers.len() as u128 {
        return Err(format!(
            "validator fees {:?}, want {} each for {} checkers",
            got[MINERS..].iter().map(|g| usd(*g)).collect::<Vec<_>>(),
            usd(fee),
            checkers.len()
        )
        .into());
    }
    println!(
        "5. finalized at block {fin_block}; no challenge in {} blocks; withdrawn and checked: first committer {} USDC (bond + reward), the other {} USDC (bond); {} checkers {} USDC each, standbys 0",
        p.cfg.dispute.challenge_window,
        usd(BOND + reward),
        usd(BOND),
        checkers.len(),
        usd(fee)
    );

    // 6. Revenue (fork, testnet): register the final shard in OnyxRevenue,
    // fixing who earns from it (ADR-0025).
    if let Some(pd) = paid {
        let f = works[0].commitment.fields();
        let coord = net.coord;
        net.chain.send(
            &coord,
            &pd.revenue,
            "register(uint64,bytes32,(bytes32,uint64,bytes32,bytes32,bytes32,bytes32,bytes32,bytes32))",
            &[
                ROUND.to_string(),
                hx(&p.shard),
                format!(
                    "({},{},{},{},{},{},{},{})",
                    hx(&f.root),
                    f.leaf_count,
                    hx(&f.recipe_id),
                    hx(&f.runtime_hash),
                    hx(&f.label_set_hash),
                    hx(&f.source_hash),
                    hx(&f.schema_id),
                    hx(&f.rights_hash)
                ),
            ],
        )?;
        println!("   OnyxRevenue: the shard is registered (its 2 miners and the checkers earn from its sales)");
    }
    // 5b. The reserve round settles right after round 1, so one wait for
    // the finalized block covers both roots (Base Sepolia: about a quarter
    // hour each otherwise).
    let reserve_settled = match &reserve_in {
        Some(r) => {
            let (out, s) = reserve::settle(net, r, paid)?;
            for l in out {
                println!("{l}");
            }
            Some(s)
        }
        None => None,
    };
    // Serve from the chain (ChainRoots reads the `finalized` block: anvil
    // reports latest - 64; Base Sepolia's L2 finality takes about a quarter
    // hour) and check as a client. The reserve root, finalized last, first.
    let mut waited = 0;
    if let Some(s) = &reserve_settled {
        waited += wait_finalized(net, o, reserve::R3, &s.shard).await?;
    }
    waited += wait_finalized(net, o, ROUND, &p.shard).await?;
    if !net.chain.anvil {
        if reserve_settled.is_some() {
            println!(
                "   the roots of rounds {ROUND} and {} reached Base Sepolia's finalized block after {waited} s (one wait for both)",
                reserve::R3
            );
        } else {
            println!("   the root reached Base Sepolia's finalized block after {waited} s");
        }
    }
    let _ = final_at;
    // Level 2 (local mode): the recorded Treasury round is recorded on chain
    // and served next to the shard (ADR-0027 amendment 4).
    let l2 = level2_prepare(net, paid, o.level2)?;
    let (server, lines, okx, served, settle) =
        serve_and_check(net, o, &d, &root, works[0].commitment.leaf_count, paid, l2).await?;
    net.mark("r1.serve_pay_verify")?;
    for l in lines {
        println!("{l}");
    }
    // REV-11 and REV-4 end to end (fork, testnet).
    let mut receipts = Value::Null;
    if let (Some(pd), Some(f)) = (paid, &settle) {
        let (out, report) = receipt_path(net, o, &d, pd, f, works[0].commitment.leaf_count).await?;
        net.mark("r1.receipt_path")?;
        for l in out {
            println!("{l}");
        }
        receipts = report;
    }
    // The live claims are checked, not just printed (M8-L55-F3).
    if d.label.starts_with("live") {
        if hx(&root) != LIVE_ROOT {
            return Err(format!(
                "live root {} is not the pinned v1 root {LIVE_ROOT}",
                hx(&root)
            )
            .into());
        }
        if (okx.received_sats, okx.sent_sats, okx.net_sats) != LIVE_OKX {
            return Err("OKX row differs from the Python recount".into());
        }
        println!(
            "   live data: root equals the pinned v1 live root; OKX equals the Python recount (checked)"
        );
    } else {
        println!(
            "   fixture mode: no live block files, so the live-root and recount checks are skipped"
        );
    }

    // 7. Optional: a dispute.
    let mut dispute = Value::Null;
    if o.dispute {
        let (out, report) = dispute_round(net)?;
        for l in out {
            println!("{l}");
        }
        dispute = report;
        // ADR-0033: the indexer resumes from its cursor and lists the
        // dispute round's shard as known; this operator does not hold its
        // rows, so it is not served. On anvil, mine past the finality depth.
        if net.chain.anvil {
            let b = net.chain.block()?;
            net.chain.reach(b + 66)?;
            let idx = demo_indexer(
                net,
                o,
                &d,
                works[0].commitment.leaf_count,
                indexer_path(o, paid)?,
            )?;
            let resumed = idx.next_block();
            let scratch = std::sync::RwLock::new(Catalog::new());
            // A start reads at most `max_jump` blocks past its cursor on its
            // first poll and up to the head on the second (IDX-R5-5).
            let mut r = idx.poll(&scratch).await?;
            let again = idx.poll(&scratch).await?;
            r.admitted.extend(again.admitted);
            let known = idx.known();
            let r2 = known
                .iter()
                .find(|k| k.round == ROUND + 1)
                .ok_or("the indexer did not see round 2's Finalized event")?;
            if r2.status != ShardStatus::NotHeld
                || !scratch
                    .read()
                    .map_err(|_| "catalog lock")?
                    .shard(ROUND + 1, &r2.shard)
                    .is_none()
            {
                return Err(
                    format!("round 2 is {:?}, want not held and not served", r2.status).into(),
                );
            }
            if !r.admitted.contains(&(ROUND, p.shard)) {
                return Err("the resumed indexer did not admit round 1 again".into());
            }
            println!(
                "   indexer resumed at block {resumed}: round {} shard {}... (the panel's root) is known but not held here, so not served; round {ROUND} admitted again from the saved event",
                ROUND + 1,
                &hx(&r2.shard)[..10]
            );
        }
    }

    // 8. The reserve shard settled in 5b: indexed, cleared by its serving
    // check, sold and checked as a client.
    let mut reserve_report = Value::Null;
    let mut reserve_summary = None;
    match (reserve_in, reserve_settled) {
        (Some(r), Some(s)) => {
            let (out, report) =
                reserve::serve(net, o, r, s, paid, settle.as_ref()).await?;
            for l in out {
                println!("{l}");
            }
            reserve_summary = Some((
                report["shard"].as_str().unwrap_or_default().to_string(),
                report["root"].as_str().unwrap_or_default().to_string(),
            ));
            reserve_report = report;
        }
        _ => println!(
            "8. reserve step skipped: no reserve tail block files (--reserve-dir, fixtures-live/tail)"
        ),
    }

    println!();
    println!("Summary");
    println!(
        "  data       {} ({} rows)",
        d.label, works[0].commitment.leaf_count
    );
    println!("  round      {ROUND}, shard {}", hx(&p.shard));
    println!("  root       {}", hx(&root));
    println!("  open tx    {}", p.open_tx);
    println!("  release tx {release_tx}");
    if let Some((shard, root)) = &reserve_summary {
        println!("  reserve    round {}, shard {shard}", reserve::R3);
        println!("  res. root  {root}");
    }
    println!(
        "  OKX        in {} BTC, out {} BTC, net {} BTC ({} txs)",
        btc(okx.received_sats.try_into()?),
        btc(okx.sent_sats.try_into()?),
        btc(okx.net_sats),
        okx.tx_count
    );
    println!(
        "  chain      {} transactions, {} blocks, {:.0} s",
        net.chain.txs,
        net.chain.block()?,
        t0.elapsed().as_secs_f64()
    );
    if o.mode != Mode::Local {
        let mut total = Spend::default();
        for sp in net.chain.spent.values() {
            total.txs += sp.txs;
            total.gas += sp.gas;
            total.l2_wei += sp.l2_wei;
            total.l1_wei += sp.l1_wei;
        }
        println!(
            "  spent      {} txs, {} L2 gas, {} ETH on L2 + {} ETH L1 data fees",
            total.txs,
            total.gas,
            eth(total.l2_wei),
            eth(total.l1_wei)
        );
        let role_gas = |addrs: Vec<[u8; 20]>| -> u128 {
            addrs
                .iter()
                .filter_map(|a| net.chain.spent.get(a))
                .map(|s| s.gas)
                .max()
                .unwrap_or(0)
        };
        println!(
            "  most gas   miner {}, validator {}, coordinator (with deploy) {}",
            role_gas(net.miners.iter().map(|m| m.address).collect()),
            role_gas(net.validators.iter().map(|v| v.address).collect()),
            role_gas(vec![net.coord])
        );
        // The transaction counts `fixtures/ops/rehearsal-gas.json` records
        // (review RESAPI-R1 F5: re-measured from this line).
        let role_txs = |addrs: Vec<[u8; 20]>| -> u64 {
            addrs
                .iter()
                .filter_map(|a| net.chain.spent.get(a))
                .map(|s| s.txs)
                .max()
                .unwrap_or(0)
        };
        let owner = paid
            .and_then(|k| exact_evm::address(&k.registry_owner_key))
            .map_or_else(Vec::new, |a| vec![a]);
        println!(
            "  most txs   miner {}, validator {}, coordinator (with deploy) {}, registry owner {}",
            role_txs(net.miners.iter().map(|m| m.address).collect()),
            role_txs(net.validators.iter().map(|v| v.address).collect()),
            role_txs(vec![net.coord]),
            role_txs(owner)
        );
    }
    if let Some(path) = &o.json {
        let live = d.label.starts_with("live");
        let report = json!({
            "generated_by": "scripts/demo-local.sh --dispute --json <path> (adapters/source-btc/examples/demo-local.rs)",
            "chain": match o.mode {
                Mode::Local => "local test chain (anvil, chain id 31337)",
                Mode::Fork => "anvil fork of Base Sepolia (nothing broadcast)",
                Mode::Testnet => "Base Sepolia (chain id 84532)",
            },
            "mode": if live { "live" } else { "fixture" },
            "data": d.label,
            "rows": works[0].commitment.leaf_count,
            "contracts": {"settlement": ax(&net.dep.settlement), "disputes": ax(&net.dep.disputes)},
            "miners": MINERS,
            "validators": VALIDATORS,
            "round": {
                "round": ROUND,
                "shard": hx(&p.shard),
                "root": hx(&root),
                "open_tx": p.open_tx,
                "release_tx": release_tx,
                "seed": hx(&p.seed),
                "checkers": checkers.len(),
                "agrees": agrees,
                "verdicts": p.verdicts.len(),
                "standbys": VALIDATORS - checkers.len(),
                "recipe_secs": (p.recipe_secs * 100.0).round() / 100.0,
                "challenge_window_blocks": p.cfg.dispute.challenge_window,
                "finalized_block": fin_block,
                "bond_micro_usdc": BOND.to_string(),
                "first_miner_micro_usdc": got[0].to_string(),
                "other_miner_micro_usdc": got[1].to_string(),
                "validator_fee_micro_usdc": fee.to_string(),
                "live_root_matches_pinned_v1_root": live,
                "label_set": "test.onyx.entities.btc v1",
                "okx": {
                    "received_sats": okx.received_sats.to_string(),
                    "sent_sats": okx.sent_sats.to_string(),
                    "net_sats": okx.net_sats.to_string(),
                    "tx_count": okx.tx_count,
                    "matches_python_recount": live,
                },
                "served": served,
                "receipt_path": receipts,
            },
            "dispute": dispute,
            "reserve": reserve_report,
            "steps": net.trace,
            "totals": {
                "txs": net.chain.txs,
                "blocks": net.chain.block()?,
                "secs": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
            },
        });
        std::fs::write(path, serde_json::to_string_pretty(&report)? + "\n")?;
        println!("  report     {}", path.display());
    }
    if o.serve && o.soak_rounds > 0 {
        let plan = soak::prepare(net, o, o.soak_rounds)?;
        println!(
            "SOAK READY: {} rounds, one every {} s",
            plan.len(),
            o.soak_every
        );
        let mut server = server;
        for s in &plan {
            tokio::select! {
                r = &mut server => { r??; return Err("the demo server stopped".into()); }
                _ = tokio::signal::ctrl_c() => { return Ok(()); }
                _ = tokio::time::sleep(Duration::from_secs(o.soak_every)) => {}
            }
            println!("{}", soak::play(net, paid, s)?);
        }
        println!("SOAK ROUNDS DONE; serving until Ctrl-C.");
        tokio::select! {
            r = server => { r??; }
            _ = tokio::signal::ctrl_c() => {}
        }
    } else if o.serve {
        println!("Serving on http://127.0.0.1:{} until Ctrl-C.", o.port);
        tokio::select! {
            r = server => { r??; }
            _ = tokio::signal::ctrl_c() => {}
        }
    } else {
        server.abort();
    }
    println!("REHEARSAL PASSED");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(a: &str) -> Res<Opts> {
        let v: Vec<String> = a.split_whitespace().map(String::from).collect();
        parse_opts(&v)
    }

    /// L16 (THREATS L2R2-F2): when the run deployed `OnyxRegistry`, the
    /// exported manifest pins it (address and runtime code hash), so a node
    /// started from the export floors every shard at its attested level; a
    /// run with no registry writes neither field (they come together).
    #[test]
    fn the_node_export_pins_the_registry() {
        use onyx_node::config::Env;
        use onyx_node::manifest::Deployment;
        let st = [0x11; 20];
        let code = Hash32([0x22; 32]);
        let reg = ([0x33; 20], Hash32([0x44; 32]));
        let m = node_manifest(84_532, &st, &code, 7, Some(reg));
        let d = Deployment::from_json(&m, Env::Testnet, false).expect("manifest");
        assert_eq!(d.registry, Some(reg.0));
        assert_eq!(d.registry_code_hash, Some(reg.1));
        assert_eq!(
            (d.settlement, d.settlement_code_hash, d.deploy_block),
            (st, code, Some(7))
        );
        let m = node_manifest(31_337, &st, &code, 7, None);
        assert!(m.get("registry").is_none() && m.get("registryCodeHash").is_none());
        let d = Deployment::from_json(&m, Env::Dev, false).expect("manifest");
        assert_eq!((d.registry, d.registry_code_hash), (None, None));
    }

    /// L16-1: only this checkout's OnyxRegistry build, immutables aside, is
    /// pinned; a proxy or a changed byte is refused.
    #[test]
    fn only_the_registry_build_is_pinned() {
        let build = [0x60u8, 0x80, 0, 0, 0, 0, 0x56, 0x5b];
        let artifact = json!({"deployedBytecode": {
            "object": format!("0x{}", hex::encode(build)),
            "immutableReferences": {"7": [{"start": 2, "length": 4}]},
        }});
        let mut code = build;
        code[2..6].copy_from_slice(&[0xaa; 4]);
        registry_matches_build(&code, &artifact).expect("immutables filled in");
        let mut other = code;
        other[7] = 0x00;
        assert!(registry_matches_build(&other, &artifact).is_err());
        // A minimal proxy is another length.
        let proxy = hex::decode("363d3d373d3d3d363d73bebebebebebebebebebebebebebebebebebebebe5af43d82803e903d91602b57fd5bf3")
            .expect("hex");
        assert!(registry_matches_build(&proxy, &artifact).is_err());
        assert!(registry_matches_build(&code, &json!({})).is_err());
        let bad = json!({"deployedBytecode": {
            "object": format!("0x{}", hex::encode(build)),
            "immutableReferences": {"7": [{"start": 6, "length": 4}]},
        }});
        assert!(registry_matches_build(&code, &bad).is_err());
        // Review L16-R3: no broadcast, no artifact, no pin (never a skip).
        let e = registry_artifact(None).expect_err("no broadcast");
        assert!(e.to_string().contains("not pinned"), "{e}");
        assert!(registry_artifact(Some(std::path::Path::new("run-latest.json"))).is_err());
    }

    /// M8-L58-F1: an unknown flag or a typo never falls through to a run.
    #[test]
    fn unknown_flags_are_refused() {
        for bad in ["--dry-run", "--plna", "--dispute=yes", "-y", "plan"] {
            let e = parse(&format!("--mode fork --broadcast x {bad}"))
                .err()
                .expect("refused");
            assert!(e.to_string().contains("unknown argument"), "{bad}: {e}");
        }
        assert!(
            parse("--mode fork --broadcast x --port").is_err(),
            "missing value"
        );
        assert!(
            parse("--mode fork --broadcast x --rpc a --rpc b").is_err(),
            "twice"
        );
        let o = parse("--mode fork --broadcast x --dispute --serve --port 9000").expect("ok");
        assert!(o.dispute && o.serve && !o.plan && o.port == 9000);
    }

    /// Review LAUNCH-R1-B2: HTTP statuses by their wording, never digits
    /// inside revert data or a hash.
    #[test]
    fn transport_errors_are_matched_by_wording() {
        for e in [
            "cast rpc: HTTP error 429 with body: rate limited",
            "error code 1: status 503",
            "HTTP status server error (502 Bad Gateway) for url (https://x)",
            "Too Many Requests",
            "Gateway Timeout",
            "error sending request for url",
            "operation timed out",
        ] {
            assert!(is_transport(e), "{e}");
        }
        for e in [
            "cast send: execution reverted, data: \"0x08c379a0000000000000000000000000000000000000000000000000000000000000429503504\"",
            "server returned an error response: error code 3: execution reverted: 0xe450d38c0000000000000000000000000000000000000000000000000000000000000502",
            "nonce too low: next nonce 429, tx nonce 428",
        ] {
            assert!(!is_transport(e), "{e}");
        }
    }

    /// Review LAUNCH-R1-B1: the first `eth_sendRawTransaction` times out
    /// but lands; the transport retry of the same bytes gets "nonce too
    /// low". The send is resolved by the first version's receipt, never
    /// signed again at the next nonce (a second send).
    #[test]
    fn a_timed_out_send_that_landed_is_not_sent_twice() {
        use std::cell::RefCell;
        use std::rc::Rc;
        #[derive(Default)]
        struct Node {
            raws: Vec<String>,
            mined: Option<String>,
        }
        let node = Rc::new(RefCell::new(Node::default()));
        let n = node.clone();
        let fake: FakeNode = Box::new(move |args: &[String]| -> Res<String> {
            let [rpc, _, method, params] = args else {
                return Err("not an rpc".into());
            };
            if rpc != "rpc" {
                return Err("not an rpc".into());
            }
            let p: Value = serde_json::from_str(params)?;
            let mut n = n.borrow_mut();
            Ok(match method.as_str() {
                "eth_estimateGas" => "\"0x5208\"".into(),
                // A node that has seen the landed send counts it.
                "eth_getTransactionCount" => if n.mined.is_some() {
                    "\"0x8\""
                } else {
                    "\"0x7\""
                }
                .into(),
                "eth_getBlockByNumber" => r#"{"baseFeePerGas":"0x1"}"#.into(),
                "eth_maxPriorityFeePerGas" => "\"0x1\"".into(),
                "eth_sendRawTransaction" => {
                    let raw = p[0].as_str().ok_or("raw")?.to_string();
                    let bytes = hex::decode(raw.trim_start_matches("0x"))?;
                    n.raws.push(raw);
                    if n.raws.len() == 1 {
                        n.mined = Some(hx(&keccak256(&bytes)));
                        return Err("cast rpc: error sending request: operation timed out".into());
                    }
                    if n.mined.is_some() {
                        return Err("cast rpc: server returned an error response: error code -32000: nonce too low: next nonce 8, tx nonce 7".into());
                    }
                    format!("\"{}\"", hx(&keccak256(&bytes)))
                }
                "eth_getTransactionReceipt" => {
                    if n.mined.as_deref() == p[0].as_str() {
                        r#"{"status":"0x1","blockNumber":"0x10","gasUsed":"0x5208","effectiveGasPrice":"0x1"}"#.into()
                    } else {
                        "null".into()
                    }
                }
                m => return Err(format!("unexpected {m}").into()),
            })
        });
        let from = [1u8; 20];
        let mut chain = Chain {
            cast: PathBuf::from("cast"),
            rpc: "http://fake".into(),
            work: std::env::temp_dir(),
            txs: 0,
            chain_id: 84_532,
            anvil: false,
            keys: BTreeMap::from([(from, [7u8; 32])]),
            spent: BTreeMap::new(),
            next_nonce: BTreeMap::new(),
            seen_block: 0,
            log: Vec::new(),
            fake: Some(fake),
        };
        let hash = chain
            .send_signed(&from, &[2u8; 20], vec![], 1, "transfer ETH")
            .expect("resolved by the receipt");
        let n = node.borrow();
        assert_eq!(Some(&hash), n.mined.as_ref(), "the landed version");
        assert_eq!(n.raws.len(), 2, "sent, then the same bytes again");
        assert_eq!(n.raws[0], n.raws[1], "never signed again");
        assert_eq!(chain.next_nonce.get(&from), Some(&8));
        assert_eq!(chain.txs, 1);
    }

    /// M8-L58-F1: testnet mode sends only with an explicit `--yes`.
    #[test]
    fn testnet_needs_plan_or_yes() {
        let e = parse("--mode testnet --broadcast x --dispute")
            .err()
            .expect("refused");
        assert!(e.to_string().contains("--yes"), "{e}");
        assert!(
            parse("--mode testnet --broadcast x --plan")
                .expect("plan")
                .plan
        );
        let o = parse("--mode testnet --broadcast x --dispute --yes").expect("yes");
        assert!(o.dispute && !o.plan);
        assert!(
            parse("--broadcast x --sweep").is_err(),
            "no sweep in local mode"
        );
    }

    /// ROADMAP B1: `--level2` rehearses level 2 on a fork; a Base Sepolia
    /// run refuses it (ADR-0027 amendment 8 item 8: level 2 stays off
    /// Base Sepolia until B2-B6), before anything is deployed.
    #[test]
    fn level2_is_fork_only_on_paid_runs() {
        assert!(
            parse("--mode fork --broadcast x --level2")
                .expect("fork")
                .level2
        );
        assert!(!parse("--mode fork --broadcast x").expect("fork").level2);
        for args in [
            "--mode testnet --broadcast x --level2 --yes",
            "--mode testnet --broadcast x --level2 --plan",
        ] {
            let e = parse(args).err().expect("refused");
            assert!(e.to_string().contains("B2-B6"), "{e}");
        }
    }
}
