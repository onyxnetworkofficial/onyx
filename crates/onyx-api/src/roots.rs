//! Where finalized roots come from (ADR-0023). The API never decides a root:
//! it reads one from the canonical ledger, either the off-chain [`Ledger`]
//! (M4/M5 model) or the `OnyxSettlement` contract (M6), and serves rows only
//! when they hash to it.

use crate::x402::BoxFuture;
use onyx_coordinator::round::Round;
use onyx_coordinator::validate::Ledger;
use onyx_core::{keccak256, Hash32, RuleSet};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::RwLock;

/// A finalized root as the ledger records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalRoot {
    /// Merkle root of the shard's rows.
    pub root: Hash32,
    /// Height (block number on chain) at which it finalized.
    pub finalized_at: u64,
    /// Unix seconds at which it finalized: the decay clock (pricing.md).
    pub finalized_unix: u64,
    /// The binding hash the ledger recorded with the root (always set by
    /// [`ChainRoots`]); the server checks it against the shard's binding inputs.
    pub binding: Option<Hash32>,
    /// Settlement transaction, when the root came from a chain.
    pub tx: Option<String>,
}

/// Root lookup error: the source could not answer (never "not finalized").
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RootError {
    /// Transport or decoding failure.
    #[error("root source unavailable: {0}")]
    Unavailable(String),
    /// The node is not the chain or the contract the server was configured for.
    #[error("root source refused: {0}")]
    WrongSource(String),
}

/// Which ledger a source reads, for the verification bundle (M7-F2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedgerKind {
    /// The off-chain M4/M5 ledger: no chain id or contract to check against.
    OffChain,
    /// `OnyxSettlement` at `contract` on `chain_id`.
    Chain {
        /// EIP-155 chain id.
        chain_id: u64,
        /// Settlement contract address.
        contract: [u8; 20],
    },
}

/// A source of finalized roots, keyed by (round, shard).
pub trait RootSource: Send + Sync {
    /// The finalized root of `shard` in `round`, or `None` if it is not finalized.
    fn finalized<'a>(
        &'a self,
        round: u64,
        shard: &'a Hash32,
    ) -> BoxFuture<'a, Result<Option<FinalRoot>, RootError>>;
    /// Short name for `/v1/health`.
    fn kind(&self) -> &'static str;
    /// The ledger this source reads.
    fn ledger(&self) -> LedgerKind;
}

/// Roots from the off-chain ledger. Filled from a [`Ledger`] and the round
/// that opened its work: a shard is finalized iff the ledger finalized its
/// work id in that round.
#[derive(Debug, Default)]
pub struct LedgerRoots {
    map: RwLock<BTreeMap<(u64, Hash32), FinalRoot>>,
}

impl LedgerRoots {
    /// Empty source.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a root an indexer has already read from the canonical ledger
    /// (used by the demo server and by tests; [`LedgerRoots::sync_round`] is
    /// the path that reads a [`Ledger`] directly).
    pub fn insert(&self, round: u64, shard: Hash32, root: FinalRoot) {
        if let Ok(mut map) = self.map.write() {
            map.insert((round, shard), root);
        }
    }

    /// Record every shard of `round` whose root is final at height `now`
    /// ([`Ledger::final_root`]): a root a dispute or rerun may still change
    /// is never served (ADR-0021, M5-L49-F2). `finalized_at` and
    /// `finalized_unix` are the settlement's height and unix time (the decay
    /// clock). Returns how many shards are recorded; call again as `now`
    /// advances.
    pub fn sync_round<R: RuleSet>(
        &self,
        ledger: &Ledger<R>,
        round: &Round,
        now: u64,
        finalized_at: u64,
        finalized_unix: u64,
    ) -> usize {
        let mut n = 0;
        if let Ok(mut map) = self.map.write() {
            for (shard, work) in round.work_of() {
                let ours = ledger
                    .finalized()
                    .get(&work)
                    .is_some_and(|(r, _)| *r == round.id());
                let Some(root) = ledger.final_root(now, &work).filter(|_| ours) else {
                    continue;
                };
                map.insert(
                    (round.id(), shard),
                    FinalRoot {
                        root,
                        finalized_at,
                        finalized_unix,
                        binding: None,
                        tx: None,
                    },
                );
                n += 1;
            }
        }
        n
    }
}

impl RootSource for LedgerRoots {
    fn finalized<'a>(
        &'a self,
        round: u64,
        shard: &'a Hash32,
    ) -> BoxFuture<'a, Result<Option<FinalRoot>, RootError>> {
        let r = self
            .map
            .read()
            .map(|m| m.get(&(round, *shard)).cloned())
            .map_err(|_| RootError::Unavailable("lock poisoned".into()));
        Box::pin(async move { r })
    }

    fn kind(&self) -> &'static str {
        "ledger"
    }

    fn ledger(&self) -> LedgerKind {
        LedgerKind::OffChain
    }
}

/// JSON-RPC transport (one call, returns the `result` field).
pub trait JsonRpc: Send + Sync {
    /// Call `method` with `params`.
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>>;
}

/// Largest JSON-RPC answer [`HttpJsonRpc`] accepts as an error message
/// excerpt (the body itself is capped by the transport).
const RPC_ERROR_EXCERPT: usize = 200;

/// Most JSON values (every array, object, string, number, bool and null
/// counts one) a JSON-RPC answer may hold (IDX-R2-3). A full `eth_getLogs`
/// answer of [`MAX_LOGS_PER_RANGE`](crate::indexer::MAX_LOGS_PER_RANGE) logs
/// is about 150,000; a hostile 16 MiB body like `[0,0,...]` would otherwise
/// become about 8 million `Value`s (0.3 to 0.5 GB).
pub const MAX_RPC_VALUES: usize = 1 << 18;

/// Parse `bytes` as one JSON document of at most `max` values, refusing
/// (while reading, before building more) an answer that holds more.
pub fn parse_bounded(bytes: &[u8], max: usize) -> Result<Value, String> {
    use serde::de::DeserializeSeed;
    let budget = std::cell::Cell::new(max);
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let v = Bounded(&budget)
        .deserialize(&mut de)
        .map_err(|e| e.to_string())?;
    de.end().map_err(|e| e.to_string())?;
    Ok(v)
}

/// Builds a `Value` while counting down a shared budget of values.
struct Bounded<'a>(&'a std::cell::Cell<usize>);

impl Bounded<'_> {
    fn take<E: serde::de::Error>(&self) -> Result<(), E> {
        match self.0.get() {
            0 => Err(E::custom("answer holds too many JSON values")),
            n => {
                self.0.set(n - 1);
                Ok(())
            }
        }
    }
}

impl<'de> serde::de::DeserializeSeed<'de> for Bounded<'_> {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Bounded<'_> {
    type Value = Value;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Value, E> {
        self.take()?;
        Ok(Value::Bool(v))
    }
    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Value, E> {
        self.take()?;
        Ok(Value::from(v))
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Value, E> {
        self.take()?;
        Ok(Value::from(v))
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Value, E> {
        self.take()?;
        Ok(serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number))
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Value, E> {
        self.take()?;
        Ok(Value::String(v.to_owned()))
    }
    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Value, E> {
        self.take()?;
        Ok(Value::String(v))
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        self.take()?;
        Ok(Value::Null)
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Value, E> {
        self.visit_unit()
    }
    fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        serde::de::DeserializeSeed::deserialize(self, d)
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        self.take()?;
        let mut out = Vec::new();
        while let Some(v) = seq.next_element_seed(Bounded(self.0))? {
            out.push(v);
        }
        Ok(Value::Array(out))
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        self.take()?;
        let mut out = serde_json::Map::new();
        while let Some(k) = map.next_key::<String>()? {
            let v = map.next_value_seed(Bounded(self.0))?;
            out.insert(k, v);
        }
        Ok(Value::Object(out))
    }
}

/// JSON-RPC 2.0 over any [`HttpPost`](crate::x402::HttpPost) transport (M8,
/// docs/ops/demo-local.md): one POST per call. An answer with `error`, a non-200
/// status, or an `id` other than the request's is an error, never a value.
/// With [`HyperPost::for_rpc`](crate::x402::HyperPost) it reads a public HTTPS
/// node (Base Sepolia) with a 1 MiB answer cap, not the facilitator's 64 KiB
/// (M8-L55-F5); the local rehearsal plugs in a loopback transport.
pub struct HttpJsonRpc<P: crate::x402::HttpPost> {
    url: String,
    post: P,
    next_id: std::sync::atomic::AtomicU64,
}

impl<P: crate::x402::HttpPost> HttpJsonRpc<P> {
    /// Calls go to `url` through `post`.
    pub fn new(url: impl Into<String>, post: P) -> Self {
        HttpJsonRpc {
            url: url.into(),
            post,
            next_id: std::sync::atomic::AtomicU64::new(1),
        }
    }
}

impl<P: crate::x402::HttpPost> JsonRpc for HttpJsonRpc<P> {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            let id = self
                .next_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let body = serde_json::to_vec(
                &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
            )
            .map_err(|e| e.to_string())?;
            let (status, bytes) = self.post.post(&self.url, body).await?;
            if status != 200 {
                return Err(format!("{method}: node answered HTTP {status}"));
            }
            let v = parse_bounded(&bytes, MAX_RPC_VALUES)
                .map_err(|e| format!("{method}: bad answer: {e}"))?;
            if v["id"] != json!(id) {
                return Err(format!("{method}: answer is for another request"));
            }
            if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
                let text = e.to_string();
                let cut = text
                    .char_indices()
                    .nth(RPC_ERROR_EXCERPT)
                    .map_or(text.len(), |(i, _)| i);
                return Err(format!("{method}: node error: {}", &text[..cut]));
            }
            v.get("result")
                .cloned()
                .ok_or_else(|| format!("{method}: answer has no result"))
        })
    }
}

/// The view a client calls to check a response (docs/spec/delivery.md).
pub const GET_FINALIZED_ROOT: &str = "getFinalizedRoot(uint64,bytes32)";

/// `getFinalizedRoot(uint64,bytes32)` selector, pinned (`cast sig`).
pub const GET_FINALIZED_ROOT_SELECTOR: [u8; 4] = [0x80, 0x52, 0xf2, 0x52];

/// `Finalized(uint64,bytes32,bytes32,bytes32)` event topic, pinned (`cast keccak`).
pub const FINALIZED_TOPIC: &str =
    "0x642365fa0ec457443cdadf7b4792f76d3de186ce4a33f99c4f6b69e5828fba73";

/// ABI calldata for `getFinalizedRoot(round, shard)`.
pub fn get_finalized_root_calldata(round: u64, shard: &Hash32) -> Vec<u8> {
    let mut d = GET_FINALIZED_ROOT_SELECTOR.to_vec();
    let mut word = [0u8; 32];
    word[24..].copy_from_slice(&round.to_be_bytes());
    d.extend_from_slice(&word);
    d.extend_from_slice(&shard.0);
    d
}

/// Decode the `(bytes32 root, bytes32 binding, uint64 finalizedAt)` return.
/// A zero root means "not finalized" (the contract's convention).
pub fn decode_finalized_root(ret: &[u8]) -> Result<Option<(Hash32, Hash32, u64)>, RootError> {
    if ret.len() != 96 {
        return Err(RootError::Unavailable(format!(
            "return data is {} bytes, want 96",
            ret.len()
        )));
    }
    let mut root = [0u8; 32];
    root.copy_from_slice(&ret[..32]);
    let mut binding = [0u8; 32];
    binding.copy_from_slice(&ret[32..64]);
    if ret[64..88].iter().any(|b| *b != 0) {
        return Err(RootError::Unavailable(
            "finalizedAt does not fit u64".into(),
        ));
    }
    let mut at = [0u8; 8];
    at.copy_from_slice(&ret[88..96]);
    if root == [0u8; 32] {
        return Ok(None);
    }
    Ok(Some((
        Hash32(root),
        Hash32(binding),
        u64::from_be_bytes(at),
    )))
}

fn hex_u64(v: &Value) -> Result<u64, RootError> {
    let s = v
        .as_str()
        .ok_or_else(|| RootError::Unavailable("expected a hex quantity".into()))?;
    u64::from_str_radix(s.trim_start_matches("0x"), 16)
        .map_err(|e| RootError::Unavailable(format!("bad quantity {s}: {e}")))
}

fn hex_bytes(v: &Value) -> Result<Vec<u8>, RootError> {
    let s = v
        .as_str()
        .ok_or_else(|| RootError::Unavailable("result is not a string".into()))?;
    hex::decode(s.trim_start_matches("0x")).map_err(|e| RootError::Unavailable(e.to_string()))
}

/// Roots read from `OnyxSettlement.getFinalizedRoot(uint64,bytes32)` with
/// `eth_call` at the `finalized` block (M7-F6), so a root that a reorg could
/// still remove is never served. Built only by [`ChainRoots::connect`], which
/// checks the node's chain id and the contract's code first (fail closed).
/// The chain id is checked again on every lookup, before the root is read and
/// again before the block time is trusted (prices depend on it): a node that
/// is repointed to another chain is refused (M7-L39-F7).
pub struct ChainRoots<T: JsonRpc> {
    contract: [u8; 20],
    chain_id: u64,
    rpc: T,
}

/// Fail closed unless the node reports `chain_id`.
async fn check_chain<T: JsonRpc>(rpc: &T, chain_id: u64) -> Result<(), RootError> {
    let got = hex_u64(
        &rpc.call("eth_chainId", json!([]))
            .await
            .map_err(RootError::Unavailable)?,
    )?;
    if got != chain_id {
        return Err(RootError::WrongSource(format!(
            "node is chain {got}, want {chain_id}"
        )));
    }
    Ok(())
}

impl<T: JsonRpc> ChainRoots<T> {
    /// Connect to the settlement contract at `contract` through `rpc`. Refuses
    /// unless the node reports `chain_id` and the contract has code, whose
    /// keccak256 must equal `code_hash` when given (the deployed build).
    pub async fn connect(
        contract: [u8; 20],
        chain_id: u64,
        code_hash: Option<Hash32>,
        rpc: T,
    ) -> Result<Self, RootError> {
        check_chain(&rpc, chain_id).await?;
        let code = hex_bytes(
            &rpc.call(
                "eth_getCode",
                json!([format!("0x{}", hex::encode(contract)), "finalized"]),
            )
            .await
            .map_err(RootError::Unavailable)?,
        )?;
        if code.is_empty() {
            return Err(RootError::WrongSource("no contract code at address".into()));
        }
        if let Some(want) = code_hash {
            if keccak256(&code) != want {
                return Err(RootError::WrongSource(
                    "contract code is not the configured build".into(),
                ));
            }
        }
        Ok(ChainRoots {
            contract,
            chain_id,
            rpc,
        })
    }

    /// Unix time of block `number`, read only after the node is still on
    /// the configured chain.
    async fn block_time(&self, number: u64) -> Result<u64, RootError> {
        check_chain(&self.rpc, self.chain_id).await?;
        let b = self
            .rpc
            .call(
                "eth_getBlockByNumber",
                json!([format!("0x{number:x}"), false]),
            )
            .await
            .map_err(RootError::Unavailable)?;
        hex_u64(&b["timestamp"])
    }

    /// The transaction that emitted `Finalized(round, shard, ..)` in block
    /// `number`, if the node returns it (best effort: `None` on any failure).
    async fn finalize_tx(&self, round: u64, shard: &Hash32, number: u64) -> Option<String> {
        let mut word = [0u8; 32];
        word[24..].copy_from_slice(&round.to_be_bytes());
        let block = format!("0x{number:x}");
        let logs = self
            .rpc
            .call(
                "eth_getLogs",
                json!([{
                    "address": format!("0x{}", hex::encode(self.contract)),
                    "fromBlock": block,
                    "toBlock": block,
                    "topics": [FINALIZED_TOPIC, format!("0x{}", hex::encode(word)), shard.to_string()],
                }]),
            )
            .await
            .ok()?;
        logs.as_array()?
            .first()?
            .get("transactionHash")?
            .as_str()
            .map(str::to_string)
    }
}

impl<T: JsonRpc> RootSource for ChainRoots<T> {
    fn finalized<'a>(
        &'a self,
        round: u64,
        shard: &'a Hash32,
    ) -> BoxFuture<'a, Result<Option<FinalRoot>, RootError>> {
        Box::pin(async move {
            check_chain(&self.rpc, self.chain_id).await?;
            let params = json!([
                {
                    "to": format!("0x{}", hex::encode(self.contract)),
                    "data": format!("0x{}", hex::encode(get_finalized_root_calldata(round, shard))),
                },
                "finalized"
            ]);
            let v = self
                .rpc
                .call("eth_call", params)
                .await
                .map_err(RootError::Unavailable)?;
            let Some((root, binding, at)) = decode_finalized_root(&hex_bytes(&v)?)? else {
                return Ok(None);
            };
            let finalized_unix = self.block_time(at).await?;
            let tx = self.finalize_tx(round, shard, at).await;
            Ok(Some(FinalRoot {
                root,
                finalized_at: at,
                finalized_unix,
                binding: Some(binding),
                tx,
            }))
        })
    }

    fn kind(&self) -> &'static str {
        "chain"
    }

    fn ledger(&self) -> LedgerKind {
        LedgerKind::Chain {
            chain_id: self.chain_id,
            contract: self.contract,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    /// A node: answers by method, records every call.
    struct FakeNode {
        chain_id: AtomicU64,
        /// After this many `eth_chainId` answers, report `switch_to`.
        switch_after: AtomicU64,
        switch_to: u64,
        code: Vec<u8>,
        call_answer: String,
        seen: Mutex<Vec<(String, Value)>>,
    }
    impl JsonRpc for FakeNode {
        fn call<'a>(
            &'a self,
            method: &'a str,
            params: Value,
        ) -> BoxFuture<'a, Result<Value, String>> {
            if let Ok(mut s) = self.seen.lock() {
                s.push((method.to_string(), params));
            }
            let r = match method {
                "eth_chainId" => {
                    if self.switch_after.fetch_sub(1, Ordering::SeqCst) == 1 {
                        self.chain_id.store(self.switch_to, Ordering::SeqCst);
                    }
                    Ok(json!(format!(
                        "0x{:x}",
                        self.chain_id.load(Ordering::SeqCst)
                    )))
                }
                "eth_getCode" => Ok(json!(format!("0x{}", hex::encode(&self.code)))),
                "eth_call" => Ok(json!(self.call_answer.clone())),
                "eth_getBlockByNumber" => Ok(json!({"timestamp": "0x6553f100"})),
                "eth_getLogs" => Ok(json!([{"transactionHash": "0xfeed"}])),
                m => Err(format!("unexpected {m}")),
            };
            Box::pin(async move { r })
        }
    }

    fn node(chain_id: u64, code: &[u8], answer: String) -> FakeNode {
        FakeNode {
            chain_id: AtomicU64::new(chain_id),
            switch_after: AtomicU64::new(u64::MAX),
            switch_to: chain_id,
            code: code.to_vec(),
            call_answer: answer,
            seen: Mutex::new(Vec::new()),
        }
    }

    #[test]
    fn pinned_selector_and_topic_match_the_signatures() {
        // Literals from `cast sig` / `cast keccak`; recomputed here as a check.
        assert_eq!(
            GET_FINALIZED_ROOT_SELECTOR,
            keccak256(b"getFinalizedRoot(uint64,bytes32)").0[..4]
        );
        assert_eq!(
            FINALIZED_TOPIC,
            keccak256(b"Finalized(uint64,bytes32,bytes32,bytes32)").to_string()
        );
        let d = get_finalized_root_calldata(7, &Hash32([0xab; 32]));
        assert_eq!(d.len(), 4 + 64);
        assert_eq!(&d[..4], &[0x80, 0x52, 0xf2, 0x52]);
        assert_eq!(d[4 + 31], 7);
        assert_eq!(&d[36..], &[0xab; 32]);
    }

    #[tokio::test]
    async fn chain_roots_read_finalized_block_with_binding_time_and_tx() {
        let mut ret = vec![0u8; 96];
        ret[..32].copy_from_slice(&[0x11; 32]);
        ret[32..64].copy_from_slice(&[0x44; 32]);
        ret[95] = 42;
        let code = b"\x60\x80code";
        let src = ChainRoots::connect(
            [0x22; 20],
            84532,
            Some(keccak256(code)),
            node(84532, code, format!("0x{}", hex::encode(&ret))),
        )
        .await
        .expect("connects");
        let got = src
            .finalized(3, &Hash32([0x33; 32]))
            .await
            .expect("answer")
            .expect("finalized");
        assert_eq!(got.root, Hash32([0x11; 32]));
        assert_eq!(got.binding, Some(Hash32([0x44; 32])));
        assert_eq!(got.finalized_at, 42);
        assert_eq!(got.finalized_unix, 0x6553_f100);
        assert_eq!(got.tx.as_deref(), Some("0xfeed"));
        assert_eq!(
            src.ledger(),
            LedgerKind::Chain {
                chain_id: 84532,
                contract: [0x22; 20]
            }
        );
        let seen = src.rpc.seen.lock().expect("lock");
        let call = seen
            .iter()
            .find(|(m, _)| m == "eth_call")
            .expect("eth_call");
        assert_eq!(call.1[0]["to"], format!("0x{}", "22".repeat(20)));
        assert_eq!(call.1[1], "finalized");
    }

    #[tokio::test]
    async fn zero_root_means_not_finalized_and_bad_data_is_an_outage() {
        let zero = ChainRoots::connect(
            [1; 20],
            1,
            None,
            node(1, b"c", "0x".to_string() + &"00".repeat(96)),
        )
        .await
        .expect("connects");
        assert_eq!(zero.finalized(1, &Hash32([0; 32])).await, Ok(None));
        let short = ChainRoots::connect([1; 20], 1, None, node(1, b"c", "0x00".into()))
            .await
            .expect("connects");
        assert!(matches!(
            short.finalized(1, &Hash32([0; 32])).await,
            Err(RootError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn connect_fails_closed_on_wrong_chain_missing_or_wrong_code() {
        let r = ChainRoots::connect([1; 20], 84532, None, node(1, b"c", String::new())).await;
        assert!(matches!(r, Err(RootError::WrongSource(_))));
        let r = ChainRoots::connect([1; 20], 84532, None, node(84532, b"", String::new())).await;
        assert!(matches!(r, Err(RootError::WrongSource(_))));
        let r = ChainRoots::connect(
            [1; 20],
            84532,
            Some(keccak256(b"other build")),
            node(84532, b"c", String::new()),
        )
        .await;
        assert!(matches!(r, Err(RootError::WrongSource(_))));
    }

    fn finalized_answer() -> String {
        let mut ret = vec![0u8; 96];
        ret[..32].copy_from_slice(&[0x11; 32]);
        ret[32..64].copy_from_slice(&[0x44; 32]);
        ret[95] = 42;
        format!("0x{}", hex::encode(&ret))
    }

    /// M7-L39-F7: the node changes chain after `connect`. Every lookup then
    /// fails closed, whether the switch comes before the root is read or
    /// between the root and the block time used for pricing.
    #[tokio::test]
    async fn chain_id_is_rechecked_before_every_root_and_block_time() {
        for (switch_after, label) in [(2u64, "before the root"), (3, "before the block time")] {
            let mut n = node(84532, b"c", finalized_answer());
            n.switch_after = AtomicU64::new(switch_after);
            n.switch_to = 8453;
            let src = ChainRoots::connect([1; 20], 84532, None, n)
                .await
                .expect("connects");
            assert!(
                matches!(
                    src.finalized(3, &Hash32([0x33; 32])).await,
                    Err(RootError::WrongSource(_))
                ),
                "{label}"
            );
            // And it stays refused.
            assert!(matches!(
                src.finalized(3, &Hash32([0x33; 32])).await,
                Err(RootError::WrongSource(_))
            ));
        }
        // A node that stays on the chain keeps answering.
        let src = ChainRoots::connect([1; 20], 84532, None, node(84532, b"c", finalized_answer()))
            .await
            .expect("connects");
        for _ in 0..3 {
            assert!(src
                .finalized(3, &Hash32([0x33; 32]))
                .await
                .expect("ok")
                .is_some());
        }
    }

    /// A transport that answers every POST with a fixed status and body, and
    /// records what was sent.
    struct Canned {
        status: u16,
        body: String,
        sent: Mutex<Vec<Value>>,
    }
    impl crate::x402::HttpPost for Canned {
        fn post<'a>(
            &'a self,
            _url: &'a str,
            body: Vec<u8>,
        ) -> BoxFuture<'a, Result<(u16, Vec<u8>), String>> {
            if let Ok(mut s) = self.sent.lock() {
                s.push(serde_json::from_slice(&body).unwrap_or(Value::Null));
            }
            let out = (self.status, self.body.clone().into_bytes());
            Box::pin(async move { Ok(out) })
        }
    }

    fn canned(status: u16, body: &str) -> HttpJsonRpc<Canned> {
        HttpJsonRpc::new(
            "http://127.0.0.1:8545",
            Canned {
                status,
                body: body.into(),
                sent: Mutex::new(Vec::new()),
            },
        )
    }

    #[tokio::test]
    async fn http_json_rpc_returns_only_a_matching_result() {
        let rpc = canned(200, r#"{"jsonrpc":"2.0","id":1,"result":"0x7a69"}"#);
        assert_eq!(
            rpc.call("eth_chainId", json!([])).await,
            Ok(json!("0x7a69"))
        );
        let sent = rpc.post.sent.lock().expect("lock")[0].clone();
        assert_eq!(
            sent,
            json!({"jsonrpc": "2.0", "id": 1, "method": "eth_chainId", "params": []})
        );
        // The next request has id 2, so an answer for id 1 is refused.
        assert!(rpc.call("eth_chainId", json!([])).await.is_err());
        // A node error, an HTTP failure, a missing result and junk are errors.
        for (status, body) in [
            (
                200,
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":3,"message":"reverted"}}"#,
            ),
            (500, r#"{"jsonrpc":"2.0","id":1,"result":"0x1"}"#),
            (200, r#"{"jsonrpc":"2.0","id":1}"#),
            (200, "not json"),
        ] {
            assert!(
                canned(status, body)
                    .call("eth_call", json!([]))
                    .await
                    .is_err(),
                "{status} {body}"
            );
        }
    }

    /// IDX-R2-3: a hostile answer of millions of tiny values is refused while
    /// it is read (at most MAX_RPC_VALUES are built), and a full answer of
    /// MAX_LOGS_PER_RANGE real-shaped logs still parses.
    #[tokio::test]
    async fn an_answer_with_too_many_values_is_refused_while_read() {
        let mut body = String::from(r#"{"jsonrpc":"2.0","id":1,"result":["#);
        let n = 8 * 1024 * 1024 - 64;
        body.reserve(2 * n);
        for i in 0..n {
            body.push_str(if i == 0 { "0" } else { ",0" });
        }
        body.push_str("]}");
        assert!(body.len() <= 16 * 1024 * 1024);
        let e = canned(200, &body)
            .call("eth_getLogs", json!([]))
            .await
            .expect_err("refused");
        assert!(e.contains("too many JSON values"), "{e}");
        let one = json!({
            "address": format!("0x{}", "5e".repeat(20)),
            "topics": [FINALIZED_TOPIC, format!("0x{:064x}", 1), format!("0x{}", "ab".repeat(32))],
            "data": format!("0x{}", "cd".repeat(64)),
            "blockNumber": "0x3e8",
            "transactionHash": format!("0x{}", "ef".repeat(32)),
            "transactionIndex": "0x0",
            "blockHash": format!("0x{}", "12".repeat(32)),
            "logIndex": "0x0",
            "removed": false,
        });
        let logs = vec![one; crate::indexer::MAX_LOGS_PER_RANGE];
        let body = json!({"jsonrpc": "2.0", "id": 1, "result": logs}).to_string();
        let got = canned(200, &body)
            .call("eth_getLogs", json!([]))
            .await
            .expect("a full answer parses");
        assert_eq!(
            got.as_array().map(Vec::len),
            Some(crate::indexer::MAX_LOGS_PER_RANGE)
        );
        assert_eq!(parse_bounded(b"[1,2] x", 10).ok(), None, "trailing junk");
        assert_eq!(
            parse_bounded(b"[1,[2,{\"a\":null}]]", 6).ok(),
            Some(json!([1, [2, {"a": null}]]))
        );
        assert!(parse_bounded(b"[1,[2,{\"a\":null}]]", 5).is_err());
    }
}
