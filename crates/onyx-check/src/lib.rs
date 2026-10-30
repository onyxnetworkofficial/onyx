//! Check an Onyx API answer from outside Rust (NS8, ADR-0047).
//!
//! Agents and scripts in other languages call the `onyx-check` command with
//! the answer they got and the question they asked. It runs the network's own
//! client check ([`onyx_api::client`], docs/spec/delivery.md §Verification):
//! the shard id and binding are recomputed, the rows checked against the root,
//! and the answer matched to the question. By default it also reads
//! `getFinalizedRoot(round, shard)` on the caller's own chain node
//! ([`ClientNode::verify`]); skipping that read takes the explicit
//! `--unchecked` flag, and the report then says `chainChecked: false`.
//! No check here is new: this crate only parses the question, calls
//! `onyx_api::client` and writes the result as JSON.

use onyx_api::client::{
    verify_response_unchecked, ClientNode, Expect, MetricDef, PayToPin, PinError, Verified,
    VerifyError, Window,
};
use onyx_api::roots::JsonRpc;
use onyx_api::x402::{check_network, PaymentRequired, PaymentRequirements};
use onyx_core::Hash32;
use serde_json::{json, Value};

/// Largest answer file read (an export of a large shard fits).
pub const MAX_ANSWER_BYTES: u64 = 64 * 1024 * 1024;

/// What the caller asked the API, as JSON (`--asked`). Each kind names the
/// fields [`Expect`] needs; nothing is read from the answer itself.
// One per run, never stored in bulk (as `Expect`): a `Box` buys nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    /// `/v1/proofs/{round}/{shard}/{index}`.
    Proof {
        /// Round.
        round: u64,
        /// Shard.
        shard: Hash32,
        /// Row index.
        index: u64,
    },
    /// `/v1/export/{round}/{shard}`.
    Export {
        /// Round.
        round: u64,
        /// Shard.
        shard: Hash32,
    },
    /// `/v1/settlement/{market}`, with the market's round, shard and subject
    /// as the market contract states them.
    Settlement {
        /// Market id.
        market: String,
        /// The market's round.
        round: u64,
        /// The market's shard.
        shard: Hash32,
        /// The market's row key.
        subject: String,
    },
    /// `/v1/metrics/{metric}`, with the metric's definition from the registry.
    Metric {
        /// Metric name.
        metric: String,
        /// Row key asked for.
        subject: String,
        /// Latest `as_of` asked for, if any.
        as_of: Option<u64>,
        /// Oldest acceptable snapshot end (required; 0 accepts any age).
        min_as_of: u64,
        /// The definition.
        def: Def,
    },
}

/// An owned [`MetricDef`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Def {
    /// Source id.
    pub source: String,
    /// Chain of the snapshot; `None` for offchain documents.
    pub chain: Option<String>,
    /// The window.
    pub window: Window,
    /// Recipe id.
    pub recipe_id: Hash32,
    /// Runtime profile hash.
    pub runtime_hash: Hash32,
    /// Row schema id.
    pub schema_id: Hash32,
    /// `keccak256(params)`.
    pub params_hash: Hash32,
    /// Label set hash (zero for none).
    pub label_set: Hash32,
    /// Registry root.
    pub registry_root: Hash32,
}

/// The caller's own chain node: where `getFinalizedRoot` is read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// JSON-RPC URL (HTTPS, or http to a loopback node).
    pub rpc: String,
    /// Chain id the node must report.
    pub chain_id: u64,
    /// Settlement contract the caller trusts.
    pub contract: [u8; 20],
}

/// A question or option that cannot be used.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

fn bad(m: impl Into<String>) -> UsageError {
    UsageError(m.into())
}

/// Parse a `0x`-prefixed 32-byte hex value.
pub fn hash32(s: &str) -> Result<Hash32, UsageError> {
    let b = s
        .strip_prefix("0x")
        .and_then(|h| hex::decode(h).ok())
        .ok_or_else(|| bad(format!("not 0x-prefixed hex: {s}")))?;
    let a: [u8; 32] = b
        .try_into()
        .map_err(|_| bad(format!("not 32 bytes: {s}")))?;
    Ok(Hash32(a))
}

/// Parse a `0x`-prefixed 20-byte address.
pub fn address(s: &str) -> Result<[u8; 20], UsageError> {
    let b = s
        .strip_prefix("0x")
        .and_then(|h| hex::decode(h).ok())
        .ok_or_else(|| bad(format!("not 0x-prefixed hex: {s}")))?;
    b.try_into().map_err(|_| bad(format!("not 20 bytes: {s}")))
}

fn only(v: &Value, keys: &[&str], what: &str) -> Result<(), UsageError> {
    let o = v
        .as_object()
        .ok_or_else(|| bad(format!("{what}: not an object")))?;
    match o.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(k) => Err(bad(format!("{what}: unknown field {k}"))),
        None => Ok(()),
    }
}

fn u64_field(v: &Value, k: &str) -> Result<u64, UsageError> {
    v[k].as_u64()
        .ok_or_else(|| bad(format!("{k}: a whole number is required")))
}

fn str_field<'a>(v: &'a Value, k: &str) -> Result<&'a str, UsageError> {
    v[k].as_str()
        .ok_or_else(|| bad(format!("{k}: a string is required")))
}

fn h_field(v: &Value, k: &str) -> Result<Hash32, UsageError> {
    hash32(str_field(v, k)?)
}

/// Parse the `--asked` JSON. Unknown fields are refused, so a typo cannot
/// silently drop a condition.
pub fn parse_asked(v: &Value) -> Result<Asked, UsageError> {
    let kind = str_field(v, "kind")?;
    Ok(match kind {
        "proof" => {
            only(v, &["kind", "round", "shard", "index"], "asked")?;
            Asked::Proof {
                round: u64_field(v, "round")?,
                shard: h_field(v, "shard")?,
                index: u64_field(v, "index")?,
            }
        }
        "export" => {
            only(v, &["kind", "round", "shard"], "asked")?;
            Asked::Export {
                round: u64_field(v, "round")?,
                shard: h_field(v, "shard")?,
            }
        }
        "settlement" => {
            only(v, &["kind", "market", "round", "shard", "subject"], "asked")?;
            Asked::Settlement {
                market: str_field(v, "market")?.to_string(),
                round: u64_field(v, "round")?,
                shard: h_field(v, "shard")?,
                subject: str_field(v, "subject")?.to_string(),
            }
        }
        "metric" => {
            only(
                v,
                &["kind", "metric", "subject", "asOf", "minAsOf", "def"],
                "asked",
            )?;
            let as_of = match &v["asOf"] {
                Value::Null => None,
                _ => Some(u64_field(v, "asOf")?),
            };
            Asked::Metric {
                metric: str_field(v, "metric")?.to_string(),
                subject: str_field(v, "subject")?.to_string(),
                as_of,
                min_as_of: u64_field(v, "minAsOf")?,
                def: parse_def(&v["def"])?,
            }
        }
        k => {
            return Err(bad(format!(
                "kind: {k} is not proof, export, settlement or metric"
            )))
        }
    })
}

fn parse_def(d: &Value) -> Result<Def, UsageError> {
    only(
        d,
        &[
            "source",
            "chain",
            "window",
            "recipeId",
            "runtimeHash",
            "schemaId",
            "paramsHash",
            "labelSet",
            "registryRoot",
        ],
        "def",
    )?;
    let w = &d["window"];
    let window = if w.as_str() == Some("document") {
        Window::Document
    } else if let Some(n) = w.get("blocks").and_then(Value::as_u64) {
        only(w, &["blocks"], "def.window")?;
        Window::Blocks(n)
    } else if let Some(n) = w.get("fromHeight").and_then(Value::as_u64) {
        only(w, &["fromHeight"], "def.window")?;
        Window::FromHeight(n)
    } else {
        return Err(bad(
            "def.window: \"document\", {\"blocks\": n} or {\"fromHeight\": n}",
        ));
    };
    let chain = match &d["chain"] {
        Value::Null => None,
        _ => Some(str_field(d, "chain")?.to_string()),
    };
    Ok(Def {
        source: str_field(d, "source")?.to_string(),
        chain,
        window,
        recipe_id: h_field(d, "recipeId")?,
        runtime_hash: h_field(d, "runtimeHash")?,
        schema_id: h_field(d, "schemaId")?,
        params_hash: h_field(d, "paramsHash")?,
        label_set: h_field(d, "labelSet")?,
        registry_root: h_field(d, "registryRoot")?,
    })
}

impl Asked {
    /// The [`Expect`] the client check takes.
    pub fn expect(&self) -> Expect<'_> {
        match self {
            Asked::Proof {
                round,
                shard,
                index,
            } => Expect::Proof {
                round: *round,
                shard: *shard,
                index: *index,
            },
            Asked::Export { round, shard } => Expect::Export {
                round: *round,
                shard: *shard,
            },
            Asked::Settlement {
                market,
                round,
                shard,
                subject,
            } => Expect::Settlement {
                market,
                round: *round,
                shard: *shard,
                subject,
            },
            Asked::Metric {
                metric,
                subject,
                as_of,
                min_as_of,
                def,
            } => Expect::Metric {
                metric,
                def: MetricDef {
                    source: &def.source,
                    chain: def.chain.as_deref(),
                    window: def.window,
                    recipe_id: def.recipe_id,
                    runtime_hash: def.runtime_hash,
                    schema_id: def.schema_id,
                    params_hash: def.params_hash,
                    label_set: def.label_set,
                    registry_root: def.registry_root,
                },
                subject,
                as_of: *as_of,
                min_as_of: *min_as_of,
            },
        }
    }
}

/// How a check ended, and the process exit code for it.
#[derive(Debug)]
pub enum Outcome {
    /// Every check passed (exit 0); the second field is what the level
    /// binding found (`--registry`), merged into the report.
    Verified(Verified, Option<Value>),
    /// The answer is refused (exit 1).
    Refused(VerifyError),
    /// The caller's node could not be read: nothing was decided (exit 3).
    Undecided(VerifyError),
    /// The caller's own settings are wrong (node on another chain, no
    /// settlement contract at the address): nothing was decided, and a retry
    /// will not help (exit 3).
    NodeConfig(String),
}

impl Outcome {
    fn from(r: Result<Verified, VerifyError>) -> Self {
        match r {
            Ok(v) => Outcome::Verified(v, None),
            Err(e @ VerifyError::ChainUnavailable(_)) => Outcome::Undecided(e),
            // Only the caller's node's `getFinalizedRoot` return is decoded
            // as "return data": a wrong contract address, not the server.
            Err(VerifyError::Malformed("return data")) => Outcome::NodeConfig(
                "your node gave no getFinalizedRoot answer at --contract: is it the settlement contract?".into(),
            ),
            Err(e) => Outcome::Refused(e),
        }
    }

    /// The exit code: 0 verified, 1 refused, 3 undecided (2 is a usage error).
    pub fn code(&self) -> i32 {
        match self {
            Outcome::Verified(..) => 0,
            Outcome::Refused(_) => 1,
            Outcome::Undecided(_) | Outcome::NodeConfig(_) => 3,
        }
    }

    /// The JSON report written to stdout.
    pub fn report(&self) -> Value {
        match self {
            Outcome::Verified(v, bound) => {
                let mut r = json!({
                    "ok": true,
                    "chainChecked": v.chain_checked,
                    "round": v.round,
                    "shard": v.shard.to_string(),
                    "root": v.root.to_string(),
                    "binding": v.binding.to_string(),
                    "servingCheck": v.serving.as_ref().map(|s| json!({
                        "gate": s.gate,
                        "crossChecked": s.cross_checked,
                        "bases": s.bases.iter().map(|(b, src)| json!({"basis": b, "source": src})).collect::<Vec<_>>(),
                        "note": "the operator's claim, not bound to the root",
                    })),
                });
                if !v.chain_checked {
                    r["warning"] = json!(UNCHECKED_WARNING);
                }
                if let Some(b) = bound {
                    r["level"] = b.clone();
                }
                r
            }
            Outcome::NodeConfig(m) => json!({
                "ok": false,
                "decided": false,
                "code": "node_config",
                "error": m,
                "retryLater": false,
            }),
            Outcome::Refused(e) | Outcome::Undecided(e) => json!({
                "ok": false,
                "decided": matches!(self, Outcome::Refused(_)),
                "code": error_code(e),
                "error": e.to_string(),
                "retryLater": matches!(e, VerifyError::NotFinalOnChain | VerifyError::ChainUnavailable(_)),
            }),
        }
    }
}

/// The text an unchecked report carries (as `verify_response_unchecked`).
pub const UNCHECKED_WARNING: &str = "chain_checked=false (NOT VERIFIED ON CHAIN): the root was not read on your own node; a lying operator node is not caught. Treat as unverified for anything that pays or settles.";

/// A stable machine name for each refusal.
pub fn error_code(e: &VerifyError) -> &'static str {
    match e {
        VerifyError::Malformed(_) => "malformed",
        VerifyError::ShardMismatch => "shard_mismatch",
        VerifyError::BindingMismatch => "binding_mismatch",
        VerifyError::ProofInvalid => "proof_invalid",
        VerifyError::ChainMismatch(_) => "chain_mismatch",
        VerifyError::NotRequested(_) => "not_requested",
        VerifyError::UnexpectedField(_) => "unexpected_field",
        VerifyError::Observation(_) => "observation",
        VerifyError::Ladder(_) => "ladder",
        VerifyError::NotFinalOnChain => "not_final_on_chain",
        VerifyError::ChainUnavailable(_) => "chain_unavailable",
    }
}

/// The explicit opt-out: every check except the root read on the caller's
/// node.
pub fn check_unchecked(answer: &Value, asked: &Asked) -> Outcome {
    Outcome::from(verify_response_unchecked(answer, &asked.expect()))
}

/// The default: every check, with the root read on `rpc` (the caller's node
/// on `chain_id`, contract `contract`).
pub async fn check_on<T: JsonRpc>(
    answer: &Value,
    asked: &Asked,
    rpc: T,
    chain_id: u64,
    contract: [u8; 20],
) -> Outcome {
    // The node's chain first, so a wrong --chain-id is the caller's setting,
    // not a mismatch the answer caused (the check below repeats it).
    match node_chain(&rpc).await {
        Err(e) => return Outcome::Undecided(VerifyError::ChainUnavailable(e)),
        Ok(got) if got != chain_id => {
            return Outcome::NodeConfig(format!(
                "your node is on chain {got}, not {chain_id} (--chain-id)"
            ))
        }
        Ok(_) => {}
    }
    let node = ClientNode::new(rpc, chain_id, contract);
    Outcome::from(node.verify(answer, &asked.expect()).await)
}

/// What `--registry` and `--ladder-state` add to [`check_on`] (ADR-0048
/// amendment 2, THREATS NS5-6, NS5-9, NS5-14).
#[derive(Clone, Debug, Default)]
pub struct Bind {
    /// The caller's `OnyxRegistry`: an answer's `proofLevel` may not be
    /// stronger than `levelOf(round, shard)` read there.
    pub registry: Option<[u8; 20]>,
    /// A folder of the row ladders this caller accepted before (one file per
    /// value key): a later answer must extend them (no fall).
    pub ladder_state: Option<std::path::PathBuf>,
}

/// [`check_on`], then the answer's stated level bound to the caller's own
/// registry and its row ladder to what the caller saw before ([`Bind`]).
pub async fn check_on_bound<T: JsonRpc>(
    answer: &Value,
    asked: &Asked,
    rpc: T,
    chain_id: u64,
    contract: [u8; 20],
    bind: &Bind,
) -> Outcome {
    let Some(registry) = bind.registry else {
        if bind.ladder_state.is_some() {
            return Outcome::NodeConfig(
                "--ladder-state needs --registry (the level is read there)".into(),
            );
        }
        return check_on(answer, asked, rpc, chain_id, contract).await;
    };
    // The registry read below goes to the same node as the root read.
    let rpc = std::sync::Arc::new(rpc);
    let outcome = check_on(answer, asked, ArcRpc(rpc.clone()), chain_id, contract).await;
    let Outcome::Verified(v, _) = outcome else {
        return outcome;
    };
    match bind_level(
        answer,
        asked,
        &v,
        &*rpc,
        registry,
        bind.ladder_state.as_deref(),
    )
    .await
    {
        Ok(b) => Outcome::Verified(v, b),
        Err(e @ VerifyError::ChainUnavailable(_)) => Outcome::Undecided(e),
        Err(e) => Outcome::Refused(e),
    }
}

/// A shared node, so the root read and the level read use one connection.
struct ArcRpc<T>(std::sync::Arc<T>);

impl<T: JsonRpc> JsonRpc for ArcRpc<T> {
    fn call<'a>(
        &'a self,
        method: &'a str,
        params: Value,
    ) -> onyx_api::x402::BoxFuture<'a, Result<Value, String>> {
        self.0.call(method, params)
    }
}

/// `levelOf(round, shard)` on the caller's registry; a shard it attests
/// nothing for counts as the weakest level (the node floors it so).
async fn registry_level<T: JsonRpc + ?Sized>(
    rpc: &T,
    registry: [u8; 20],
    round: u64,
    shard: &Hash32,
) -> Result<onyx_core::ProofLevel, VerifyError> {
    let data = format!(
        "0x{}",
        hex::encode(onyx_api::levels::level_of_calldata(round, shard))
    );
    let ret = rpc
        .call(
            "eth_call",
            json!([{"to": format!("0x{}", hex::encode(registry)), "data": data}, "latest"]),
        )
        .await
        .map_err(VerifyError::ChainUnavailable)?;
    let bytes = ret
        .as_str()
        .and_then(|s| s.strip_prefix("0x"))
        .and_then(|s| hex::decode(s).ok())
        .ok_or(VerifyError::Malformed("levelOf return"))?;
    let level = onyx_api::levels::level_of_return(&bytes)
        .map_err(|_| VerifyError::Malformed("levelOf return"))?;
    Ok(level.unwrap_or(onyx_core::ProofLevel::TrackRecord))
}

/// The binding itself: `proofLevel` (when the answer states one) no stronger
/// than the registry's level, and for a row answer (`proof`, `metric`) its
/// ladder the row's one step at that level ([`onyx_api::client::check_row_ladder`]),
/// extending the ladder kept in `state` for the same value.
async fn bind_level<T: JsonRpc + ?Sized>(
    answer: &Value,
    asked: &Asked,
    v: &Verified,
    rpc: &T,
    registry: [u8; 20],
    state: Option<&std::path::Path>,
) -> Result<Option<Value>, VerifyError> {
    let stated = match answer.get("proofLevel") {
        None | Some(Value::Null) => return Ok(None),
        Some(l) => l
            .as_u64()
            .and_then(|n| u8::try_from(n).ok())
            .and_then(onyx_core::ProofLevel::from_number)
            .ok_or(VerifyError::Malformed("proofLevel"))?,
    };
    let chain = registry_level(rpc, registry, v.round, &v.shard).await?;
    if stated.number() < chain.number() {
        return Err(VerifyError::Ladder(format!(
            "the answer says level {}, your registry attests level {} for this shard",
            stated.number(),
            chain.number()
        )));
    }
    let row = matches!(asked, Asked::Proof { .. } | Asked::Metric { .. });
    if row && answer.get("ladder").is_some_and(|l| !l.is_null()) {
        let l = onyx_api::client::check_row_ladder(answer, v, chain)?;
        if let Some(dir) = state {
            keep_ladder(dir, &l)?;
        }
    }
    Ok(Some(json!({
        "stated": stated.number(),
        "registry": chain.number(),
        "note": "the answer's level is no stronger than your own registry's levelOf(round, shard)",
    })))
}

/// THREATS NS5-9: the ladder this caller accepted before for the same value
/// must still be there (no step dropped or changed); the new one is kept.
fn keep_ladder(dir: &std::path::Path, l: &onyx_core::Ladder) -> Result<(), VerifyError> {
    let path = dir.join(format!("{}.json", l.value));
    let store =
        |e: std::io::Error| VerifyError::Ladder(format!("--ladder-state {}: {e}", path.display()));
    match std::fs::read_to_string(&path) {
        Ok(s) => {
            let prev: Value = serde_json::from_str(&s).map_err(|e| {
                VerifyError::Ladder(format!("--ladder-state {}: {e}", path.display()))
            })?;
            let prev =
                onyx_api::ladder::parse(&prev, onyx_api::ladder::ROW_RECORD).map_err(|e| {
                    VerifyError::Ladder(format!("--ladder-state {}: {e}", path.display()))
                })?;
            l.check_extends(&prev).map_err(|e| {
                VerifyError::Ladder(format!("against the ladder you accepted before: {e}"))
            })?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(store(e)),
    }
    std::fs::create_dir_all(dir).map_err(store)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        onyx_api::ladder::to_json(l, onyx_api::ladder::ROW_RECORD).to_string(),
    )
    .map_err(store)?;
    std::fs::rename(&tmp, &path).map_err(store)
}

async fn node_chain<T: JsonRpc + ?Sized>(rpc: &T) -> Result<u64, String> {
    let id = rpc.call("eth_chainId", json!([])).await?;
    id.as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| format!("eth_chainId: not a quantity: {id}"))
}

/// How a payment offer check ended (`--offer`).
#[derive(Debug)]
pub enum OfferOutcome {
    /// One option pays the pinned `OnyxRevenue` in the pinned token on the
    /// pinned testnet, within the caller's limit (exit 0).
    Payable(PaymentRequirements),
    /// Do not pay (exit 1).
    Refused(&'static str, String),
    /// Nothing decided: the node could not be read, or the caller's settings
    /// are wrong (exit 3).
    Undecided(&'static str, String),
}

impl OfferOutcome {
    /// 0 payable, 1 refused, 3 undecided.
    pub fn code(&self) -> i32 {
        match self {
            OfferOutcome::Payable(_) => 0,
            OfferOutcome::Refused(..) => 1,
            OfferOutcome::Undecided(..) => 3,
        }
    }

    /// The JSON report.
    pub fn report(&self) -> Value {
        match self {
            OfferOutcome::Payable(r) => json!({
                "ok": true,
                "pay": r,
                "note": "the only option to sign: it pays the pinned OnyxRevenue on the pinned testnet",
            }),
            OfferOutcome::Refused(code, m) | OfferOutcome::Undecided(code, m) => json!({
                "ok": false,
                "decided": matches!(self, OfferOutcome::Refused(..)),
                "code": code,
                "error": m,
            }),
        }
    }
}

/// Check a 402 offer before paying it (REV-4, AGT-9): the deployment
/// manifest the caller trusts names the `OnyxRevenue` contract, token and
/// code hash ([`PayToPin`]); the caller's node must be on that testnet and
/// hold that code; one offered option must pay exactly that contract in that
/// token, for at most `max_amount` (the token's smallest unit).
pub async fn check_offer<T: JsonRpc + ?Sized>(
    offer: &Value,
    manifest: &Value,
    max_amount: u128,
    rpc: &T,
) -> OfferOutcome {
    let pr: PaymentRequired = match serde_json::from_value(offer.clone()) {
        Ok(p) => p,
        Err(e) => return OfferOutcome::Refused("malformed_offer", e.to_string()),
    };
    let pin = match PayToPin::from_manifest(manifest) {
        Ok(p) => p,
        Err(e) => return OfferOutcome::Undecided("manifest", e.to_string()),
    };
    if let Err(e) = check_network(&pin.network, false) {
        return OfferOutcome::Undecided("manifest", e);
    }
    let verified = match pin.check_code(rpc).await {
        Ok(v) => v,
        Err(PinError::Rpc(e)) => return OfferOutcome::Undecided("chain_unavailable", e),
        Err(e) => return OfferOutcome::Undecided("node_config", e.to_string()),
    };
    let req = match verified.select(&pr) {
        Ok(r) => r,
        Err(e) => return OfferOutcome::Refused("not_pinned", e.to_string()),
    };
    match req.amount.parse::<u128>() {
        Ok(a) if a <= max_amount => OfferOutcome::Payable(req.clone()),
        Ok(a) => OfferOutcome::Refused(
            "over_limit",
            format!("the offer asks {a}, over your limit {max_amount}"),
        ),
        Err(_) => OfferOutcome::Refused("malformed_offer", "amount is not a whole number".into()),
    }
}

/// What the caller asked a bulk node for (`onyx-check --bulk`): the answer
/// must be about exactly this publisher and dataset (review CL5-R2), at least
/// this version, and the rows in this key range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BulkAsked {
    /// Publisher source id.
    pub publisher: String,
    /// Dataset id.
    pub dataset: String,
    /// Lowest version accepted (an older one is a rollback).
    pub min_version: u32,
    /// Smallest key asked.
    pub from: Option<i128>,
    /// Largest key asked.
    pub to: Option<i128>,
}

/// The completeness note every bulk report carries (THREATS CL5-9).
pub const BULK_COMPLETENESS: &str = "every row returned is proven and the page has no gap inside it; rows at the edges of the page (or a page answered empty) are not proven complete yet";

/// How a bulk range check ended (CL5, ADR-0059; `onyx-check --bulk`).
// One per run: a `Box` buys nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum BulkOutcome {
    /// Every row proven to its shard, every shard to the manifest, the
    /// manifest root signed by the publisher, and (when a node was given)
    /// that signed report recorded and accepted in the book at the caller's
    /// finalized block with the key active (exit 0).
    Verified {
        /// The off-chain check.
        checked: onyx_bulk::wire::Checked,
        /// The caller's chain read, if any.
        anchor: Option<onyx_api::bulk_serve::AnchorView>,
    },
    /// Refused (exit 1).
    Refused(String),
    /// The caller's node could not be read (exit 3).
    Undecided(String),
}

impl BulkOutcome {
    /// Exit code: 0 verified, 1 refused, 3 undecided.
    pub fn code(&self) -> i32 {
        match self {
            BulkOutcome::Verified { .. } => 0,
            BulkOutcome::Refused(_) => 1,
            BulkOutcome::Undecided(_) => 3,
        }
    }

    /// The JSON report.
    pub fn report(&self) -> Value {
        match self {
            BulkOutcome::Verified { checked: c, anchor } => {
                let mut r = json!({
                    "ok": true,
                    "chainChecked": anchor.is_some(),
                    "publisher": c.header.publisher.0,
                    "dataset": c.header.dataset,
                    "version": c.header.version,
                    "manifestRoot": c.manifest_root.to_string(),
                    "digest": c.digest.to_string(),
                    "signer": format!("0x{}", hex::encode(c.signer)),
                    "rows": c.rows.len(),
                    "firstKey": c.rows.first().map(|r| r.2.to_string()),
                    "lastKey": c.rows.last().map(|r| r.2.to_string()),
                    "completeness": BULK_COMPLETENESS,
                });
                match anchor {
                    Some(a) => {
                        r["anchor"] = json!({
                            "chainId": a.chain_id,
                            "book": format!("0x{}", hex::encode(a.book)),
                            "fact": a.fact.to_string(),
                            "acceptedAt": a.accepted_at,
                            "checkedAt": {"number": a.block_number, "hash": a.block_hash.to_string()},
                        })
                    }
                    None => r["warning"] = json!(UNCHECKED_WARNING),
                }
                r
            }
            BulkOutcome::Refused(e) | BulkOutcome::Undecided(e) => json!({
                "ok": false,
                "decided": matches!(self, BulkOutcome::Refused(_)),
                "code": if matches!(self, BulkOutcome::Refused(_)) { "bulk_refused" } else { "chain_unavailable" },
                "error": e,
                "retryLater": matches!(self, BulkOutcome::Undecided(_)),
            }),
        }
    }
}

/// Check a `GET /v1/bulk/{publisher}/{dataset}/rows` answer for the asked
/// key range: the proofs and signature off chain
/// ([`onyx_bulk::wire::check_rows_answer`]), then, with `node` =
/// `(rpc, chain id, book)`, the book read at the caller's own finalized block
/// ([`onyx_api::bulk_serve::read_anchor`]).
pub async fn check_bulk(
    answer: &Value,
    asked: &BulkAsked,
    node: Option<(&dyn JsonRpc, u64, [u8; 20])>,
) -> BulkOutcome {
    let checked = match onyx_bulk::wire::check_rows_answer(answer, asked.from, asked.to) {
        Ok(c) => c,
        Err(e) => return BulkOutcome::Refused(e.to_string()),
    };
    if checked.header.publisher.0 != asked.publisher || checked.header.dataset != asked.dataset {
        return BulkOutcome::Refused(format!(
            "the answer is about {}/{}, not the {}/{} asked",
            checked.header.publisher.0, checked.header.dataset, asked.publisher, asked.dataset
        ));
    }
    if checked.header.version < asked.min_version {
        return BulkOutcome::Refused(format!(
            "the answer is version {}, older than the {} asked",
            checked.header.version, asked.min_version
        ));
    }
    let Some((rpc, chain_id, book)) = node else {
        return BulkOutcome::Verified {
            checked,
            anchor: None,
        };
    };
    match onyx_api::bulk_serve::read_anchor(rpc, chain_id, &book, &checked.report, &checked.signer).await {
        Err(e) => BulkOutcome::Undecided(e),
        Ok(onyx_api::bulk_serve::AnchorState::Not(why)) => BulkOutcome::Refused(format!(
            "the manifest's signed report is not anchored in your book at your finalized block: {why}"
        )),
        Ok(onyx_api::bulk_serve::AnchorState::Anchored(a)) => BulkOutcome::Verified {
            checked,
            anchor: Some(a),
        },
    }
}
