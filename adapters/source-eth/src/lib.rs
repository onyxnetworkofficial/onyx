//! Ethereum source adapter (ADR-0044, ROADMAP P4a): JSON-RPC answers
//! (`eth_getBlockByNumber` with full transactions, `eth_getBlockReceipts`)
//! to the canonical `EthChainSource` of `eth-chain-stats`.
//! This crate may depend on `onyx-core`; no core crate may depend on it.
//!
//! The RPC's JSON is never taken at its word. Each header, transaction and
//! receipt is re-encoded in the chain's own format (RLP, EIP-2718
//! envelopes) and checked against the hashes the chain commits to: the
//! header's Keccak-256 must be the block hash, each transaction's the
//! transaction hash, the transactions and receipts must rebuild the header's
//! trie roots, and each block's parent hash must be the block before. Then
//! the JSON's own derived fields (per-transaction `gasUsed`,
//! `effectiveGasPrice`, `blobGasPrice`, `contractAddress`) must equal what
//! the recipe computes from the hashed bytes; any difference is refused.
//!
//! Trusted, not checked here (R-ETH-1): that the newest block is on
//! Ethereum's finalized chain (post-merge blocks carry no proof of work; the
//! fetch compares hashes with a second operator and stays at or below the
//! finalized block, `scripts/refresh-eth-onchain.py`), and the execution
//! itself (that each receipt is what running the transaction gives; only a
//! full node re-executes).

use borsh::BorshDeserialize;
use onyx_core::{
    keccak256 as core_keccak, ChainId, DataRights, Hash32, MetricInfo, Registry, RuntimeProfile,
    ShardSpec, Snapshot, SourceId, SourceRef, StaticRegistry, ValueKind,
};
use onyx_eth_schema::decode::{self, Fees, GAS_PER_BLOB};
use onyx_eth_schema::rlp::{encode_bytes, encode_list};
use onyx_eth_schema::{keccak256, trie, EthBlock, EthChainSource, ETH_MAINNET, ETH_SCHEMA_VERSION};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

/// Source id for Ethereum mainnet data.
pub const SOURCE_ID: &str = "chain.eth.mainnet";
/// Chain id.
pub const CHAIN_ID: &str = ETH_MAINNET;
/// Network domain of the fixture registry.
pub const DEVNET_DOMAIN: &str = "onyx.devnet";
/// Metric of the per-block chain statistics (`eth-chain-stats`, ADR-0044).
pub const METRIC_ETH_CHAIN_STATS: &str = "crypto.eth.chain_stats";
/// Schema tag of `EthChainStatsRow`: `ShardSpec.schema` is its Keccak.
pub const SCHEMA_ETH_CHAIN_STATS: &[u8] = onyx_eth_schema::SCHEMA_ETH_CHAIN_STATS;
/// Largest JSON-RPC answer read from a fixture file, bytes.
pub const MAX_ANSWER: u64 = 64 << 20;

/// Adapter error.
#[derive(Debug, thiserror::Error)]
pub enum EthAdapterError {
    /// A file is missing, unreadable or not JSON.
    #[error("fixture: {0}")]
    Fixture(String),
    /// A file's SHA-256 differs from the folder's manifest, or it is not listed.
    #[error("checksum: {0}")]
    Checksum(String),
    /// An answer that does not match the chain's hashes or the recipe's rules.
    #[error("block {number}: {what}")]
    Answer {
        /// Block number.
        number: u64,
        /// What failed.
        what: String,
    },
}

fn bad(number: u64, what: impl Into<String>) -> EthAdapterError {
    EthAdapterError::Answer {
        number,
        what: what.into(),
    }
}

/// The registry the chain statistics run under: Ethereum mainnet as a
/// public-chain source and the metric [`METRIC_ETH_CHAIN_STATS`] (no label set).
pub fn fixture_registry_eth() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    let built = r
        .set_domain(DEVNET_DOMAIN)
        .and_then(|r| r.add_source(DataRights::public_chain(SourceId(SOURCE_ID.into()))))
        .and_then(|r| {
            r.add_metric(
                METRIC_ETH_CHAIN_STATS,
                MetricInfo {
                    value_kind: ValueKind::Bytes,
                    needs_label_set: false,
                },
            )
        })
        .map(|_| ());
    debug_assert!(built.is_ok(), "the Ethereum registry is static and valid");
    r
}

/// The recipe params a snapshot pins (P4A-R1-1): its range and tip as Borsh
/// `EthStatsParams`. Empty for a snapshot that is not a block range.
pub fn params_for(snapshot: &Snapshot) -> Vec<u8> {
    match snapshot {
        Snapshot::BlockRange {
            from_height,
            to_height,
            tip_hash,
            ..
        } => borsh::to_vec(&onyx_eth_schema::EthStatsParams {
            from: *from_height,
            to: *to_height,
            tip: tip_hash.0,
        })
        .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// True when a spec's params are exactly the ones its snapshot pins, so the
/// recipe's own range and tip check binds the rows to the snapshot (P4A-R1-1).
pub fn spec_binds_snapshot(spec: &ShardSpec) -> bool {
    !spec.params.is_empty() && spec.params == params_for(&spec.source.snapshot)
}

/// The shard spec of one `eth-chain-stats` run under [`fixture_registry_eth`]:
/// no label set, params from the snapshot ([`params_for`]), the Ethereum source
/// pinned by `snapshot`.
pub fn stats_spec(recipe: Hash32, runtime: RuntimeProfile, snapshot: Snapshot) -> ShardSpec {
    let reg = fixture_registry_eth();
    let source = SourceId(SOURCE_ID.into());
    let params = params_for(&snapshot);
    ShardSpec {
        recipe: Some(recipe),
        runtime: Some(runtime),
        label_set: None,
        rights_hash: reg
            .source_rights(&source)
            .map(|r| r.hash())
            .unwrap_or(Hash32::ZERO),
        source: SourceRef { source, snapshot },
        schema: core_keccak(SCHEMA_ETH_CHAIN_STATS),
        params,
        registry_root: reg.root(),
    }
}

// ---- JSON quantities and data ----

fn field<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing or non-string `{k}`"))
}

/// A `DATA` field: `0x` and an even number of lowercase or uppercase hex digits.
fn data(v: &Value, k: &str) -> Result<Vec<u8>, String> {
    let s = field(v, k)?;
    let h = s
        .strip_prefix("0x")
        .ok_or_else(|| format!("`{k}` lacks 0x"))?;
    hex::decode(h).map_err(|_| format!("`{k}` is not hex"))
}

fn data_n<const N: usize>(v: &Value, k: &str) -> Result<[u8; N], String> {
    let b = data(v, k)?;
    b.try_into().map_err(|_| format!("`{k}` is not {N} bytes"))
}

/// A `QUANTITY` field as minimal big-endian bytes (no leading zeros; zero is
/// empty), at most 32 bytes. Refuses a non-minimal form such as `0x01`.
fn qty_bytes(v: &Value, k: &str) -> Result<Vec<u8>, String> {
    let s = field(v, k)?;
    let h = s
        .strip_prefix("0x")
        .ok_or_else(|| format!("`{k}` lacks 0x"))?;
    if h.is_empty() || h.len() > 64 || (h.len() > 1 && h.starts_with('0')) {
        return Err(format!("`{k}` is not a minimal quantity"));
    }
    let padded = if h.len() % 2 == 1 {
        format!("0{h}")
    } else {
        h.to_string()
    };
    let b = hex::decode(padded).map_err(|_| format!("`{k}` is not hex"))?;
    let skip = b.iter().take_while(|&&x| x == 0).count();
    Ok(b[skip..].to_vec())
}

fn qty(v: &Value, k: &str) -> Result<u128, String> {
    let b = qty_bytes(v, k)?;
    if b.len() > 16 {
        return Err(format!("`{k}` is wider than 128 bits"));
    }
    Ok(b.iter().fold(0u128, |a, &x| (a << 8) | u128::from(x)))
}

fn qty64(v: &Value, k: &str) -> Result<u64, String> {
    u64::try_from(qty(v, k)?).map_err(|_| format!("`{k}` is wider than 64 bits"))
}

fn put_qty(p: &mut Vec<u8>, v: &Value, k: &str) -> Result<(), String> {
    encode_bytes(p, &qty_bytes(v, k)?);
    Ok(())
}

fn put_data(p: &mut Vec<u8>, v: &Value, k: &str) -> Result<(), String> {
    encode_bytes(p, &data(v, k)?);
    Ok(())
}

fn list_of(p: &mut Vec<u8>, payload: &[u8]) {
    encode_list(p, payload);
}

// ---- Headers ----

/// Header fields in their RLP order, and whether each is a quantity. Fields
/// 16 and later exist only from Shanghai, Cancun and Prague on; a header has
/// a prefix of this list (at least the 16 fields of London).
const HEADER_FIELDS: [(&str, bool); 21] = [
    ("parentHash", false),
    ("sha3Uncles", false),
    ("miner", false),
    ("stateRoot", false),
    ("transactionsRoot", false),
    ("receiptsRoot", false),
    ("logsBloom", false),
    ("difficulty", true),
    ("number", true),
    ("gasLimit", true),
    ("gasUsed", true),
    ("timestamp", true),
    ("extraData", false),
    ("mixHash", false),
    ("nonce", false),
    ("baseFeePerGas", true),
    ("withdrawalsRoot", false),
    ("blobGasUsed", true),
    ("excessBlobGas", true),
    ("parentBeaconBlockRoot", false),
    ("requestsHash", false),
];

/// Keys of a block answer that are not header fields.
const BLOCK_OTHER_KEYS: [&str; 6] = [
    "hash",
    "size",
    "totalDifficulty",
    "uncles",
    "transactions",
    "withdrawals",
];

/// The header's RLP from a block answer. A key that is neither a known
/// header field nor a known block key is refused: a new fork's header field
/// must be added here (and the recipe reviewed), never dropped silently.
pub fn header_rlp(block: &Value) -> Result<Vec<u8>, String> {
    let obj = block.as_object().ok_or("block is not an object")?;
    for k in obj.keys() {
        if !HEADER_FIELDS.iter().any(|(f, _)| f == k) && !BLOCK_OTHER_KEYS.contains(&k.as_str()) {
            return Err(format!("unknown block field `{k}`"));
        }
    }
    let mut p = Vec::new();
    let mut n = 0;
    for (k, is_qty) in HEADER_FIELDS {
        if !obj.contains_key(k) {
            break;
        }
        if is_qty {
            put_qty(&mut p, block, k)?;
        } else {
            put_data(&mut p, block, k)?;
        }
        n += 1;
    }
    if n < 16 || HEADER_FIELDS[n..].iter().any(|(k, _)| obj.contains_key(*k)) {
        return Err("header fields are not a London-or-later prefix".into());
    }
    let mut out = Vec::new();
    list_of(&mut out, &p);
    Ok(out)
}

// ---- Transactions ----

fn put_to(p: &mut Vec<u8>, tx: &Value) -> Result<(), String> {
    match tx.get("to") {
        None | Some(Value::Null) => {
            encode_bytes(p, &[]);
            Ok(())
        }
        Some(_) => {
            encode_bytes(p, &data_n::<20>(tx, "to")?);
            Ok(())
        }
    }
}

fn put_access_list(p: &mut Vec<u8>, tx: &Value) -> Result<(), String> {
    let list = tx
        .get("accessList")
        .and_then(Value::as_array)
        .ok_or("missing `accessList`")?;
    let mut entries = Vec::new();
    for e in list {
        let mut one = Vec::new();
        encode_bytes(&mut one, &data_n::<20>(e, "address")?);
        let mut keys = Vec::new();
        for k in e
            .get("storageKeys")
            .and_then(Value::as_array)
            .ok_or("missing `storageKeys`")?
        {
            let s = k.as_str().ok_or("storage key is not a string")?;
            let b = hex::decode(s.strip_prefix("0x").ok_or("storage key lacks 0x")?)
                .map_err(|_| "storage key is not hex")?;
            if b.len() != 32 {
                return Err("storage key is not 32 bytes".into());
            }
            encode_bytes(&mut keys, &b);
        }
        list_of(&mut one, &keys);
        list_of(&mut entries, &one);
    }
    list_of(p, &entries);
    Ok(())
}

fn put_parity(p: &mut Vec<u8>, tx: &Value) -> Result<(), String> {
    // Typed transactions sign with a y parity (0 or 1); some nodes also print it as `v`.
    let k = if tx.get("yParity").is_some() {
        "yParity"
    } else {
        "v"
    };
    if qty(tx, k)? > 1 {
        return Err("y parity above 1".into());
    }
    put_qty(p, tx, k)
}

/// A transaction's envelope (as the transactions trie stores it) from a
/// transaction object, for types 0-4.
pub fn tx_envelope(tx: &Value) -> Result<Vec<u8>, String> {
    let ty = qty(tx, "type")?;
    let mut p = Vec::new();
    if ty == 0 {
        for k in ["nonce", "gasPrice", "gas"] {
            put_qty(&mut p, tx, k)?;
        }
        put_to(&mut p, tx)?;
        put_qty(&mut p, tx, "value")?;
        put_data(&mut p, tx, "input")?;
        for k in ["v", "r", "s"] {
            put_qty(&mut p, tx, k)?;
        }
        let mut out = Vec::new();
        list_of(&mut out, &p);
        return Ok(out);
    }
    if !(1..=4).contains(&ty) {
        return Err(format!(
            "transaction type {ty} is not one this adapter knows"
        ));
    }
    put_qty(&mut p, tx, "chainId")?;
    put_qty(&mut p, tx, "nonce")?;
    if ty == 1 {
        put_qty(&mut p, tx, "gasPrice")?;
    } else {
        put_qty(&mut p, tx, "maxPriorityFeePerGas")?;
        put_qty(&mut p, tx, "maxFeePerGas")?;
    }
    put_qty(&mut p, tx, "gas")?;
    put_to(&mut p, tx)?;
    put_qty(&mut p, tx, "value")?;
    put_data(&mut p, tx, "input")?;
    put_access_list(&mut p, tx)?;
    if ty == 3 {
        put_qty(&mut p, tx, "maxFeePerBlobGas")?;
        let mut hashes = Vec::new();
        for h in tx
            .get("blobVersionedHashes")
            .and_then(Value::as_array)
            .ok_or("missing `blobVersionedHashes`")?
        {
            let s = h.as_str().ok_or("blob hash is not a string")?;
            let b = hex::decode(s.strip_prefix("0x").ok_or("blob hash lacks 0x")?)
                .map_err(|_| "blob hash is not hex")?;
            if b.len() != 32 {
                return Err("blob hash is not 32 bytes".into());
            }
            encode_bytes(&mut hashes, &b);
        }
        list_of(&mut p, &hashes);
    }
    if ty == 4 {
        let mut auths = Vec::new();
        for a in tx
            .get("authorizationList")
            .and_then(Value::as_array)
            .ok_or("missing `authorizationList`")?
        {
            let mut one = Vec::new();
            put_qty(&mut one, a, "chainId")?;
            encode_bytes(&mut one, &data_n::<20>(a, "address")?);
            put_qty(&mut one, a, "nonce")?;
            put_qty(&mut one, a, "yParity")?;
            put_qty(&mut one, a, "r")?;
            put_qty(&mut one, a, "s")?;
            list_of(&mut auths, &one);
        }
        list_of(&mut p, &auths);
    }
    put_parity(&mut p, tx)?;
    put_qty(&mut p, tx, "r")?;
    put_qty(&mut p, tx, "s")?;
    // ty is 1..=4 here.
    let mut out = vec![ty as u8];
    list_of(&mut out, &p);
    Ok(out)
}

// ---- Receipts ----

/// A receipt's envelope (as the receipts trie stores it) from a receipt object.
pub fn receipt_envelope(r: &Value) -> Result<Vec<u8>, String> {
    let ty = qty(r, "type")?;
    if ty > 4 {
        return Err(format!("receipt type {ty} is not one this adapter knows"));
    }
    let mut p = Vec::new();
    put_qty(&mut p, r, "status")?;
    put_qty(&mut p, r, "cumulativeGasUsed")?;
    let bloom = data(r, "logsBloom")?;
    if bloom.len() != 256 {
        return Err("logs bloom is not 256 bytes".into());
    }
    encode_bytes(&mut p, &bloom);
    let mut logs = Vec::new();
    for l in r
        .get("logs")
        .and_then(Value::as_array)
        .ok_or("missing `logs`")?
    {
        let mut one = Vec::new();
        encode_bytes(&mut one, &data_n::<20>(l, "address")?);
        let mut topics = Vec::new();
        for t in l
            .get("topics")
            .and_then(Value::as_array)
            .ok_or("missing `topics`")?
        {
            let s = t.as_str().ok_or("topic is not a string")?;
            let b = hex::decode(s.strip_prefix("0x").ok_or("topic lacks 0x")?)
                .map_err(|_| "topic is not hex")?;
            if b.len() != 32 {
                return Err("topic is not 32 bytes".into());
            }
            encode_bytes(&mut topics, &b);
        }
        list_of(&mut one, &topics);
        put_data(&mut one, l, "data")?;
        list_of(&mut logs, &one);
    }
    list_of(&mut p, &logs);
    let mut out = if ty == 0 { Vec::new() } else { vec![ty as u8] };
    list_of(&mut out, &p);
    Ok(out)
}

// ---- One block ----

fn hash_of(v: &Value, k: &str) -> Result<[u8; 32], String> {
    data_n::<32>(v, k)
}

/// One block from its two answers' `result` values, checked against the
/// chain's hashes (header, every transaction, both trie roots) and with the
/// answer's derived receipt fields compared with the hashed bytes, and the
/// header's EIP-1559 base fee, EIP-7918 excess blob gas and gas used checked
/// against the parent and the receipts. `parent` is the parent's header RLP.
pub fn eth_block(
    number: u64,
    block: &Value,
    receipts: &Value,
    parent_header: &[u8],
) -> Result<EthBlock, EthAdapterError> {
    let e = |w: String| bad(number, w);
    let header = header_rlp(block).map_err(e)?;
    let hash = hash_of(block, "hash").map_err(e)?;
    if keccak256(&header) != hash {
        return Err(bad(number, "the header does not hash to the block hash"));
    }
    let h = decode::header(&header).map_err(|x| bad(number, format!("header: {x:?}")))?;
    let p =
        decode::header(parent_header).map_err(|x| bad(number, format!("parent header: {x:?}")))?;
    if h.number != number || h.parent_hash != keccak256(parent_header) {
        return Err(bad(
            number,
            "the header is not the child of the given parent",
        ));
    }
    let txs_json = block
        .get("transactions")
        .and_then(Value::as_array)
        .ok_or_else(|| bad(number, "no full transactions"))?;
    let rec_json = receipts
        .as_array()
        .ok_or_else(|| bad(number, "receipts are not a list"))?;
    if rec_json.len() != txs_json.len() {
        return Err(bad(number, "not one receipt per transaction"));
    }
    let mut txs = Vec::with_capacity(txs_json.len());
    let mut recs = Vec::with_capacity(txs_json.len());
    let mut cum = 0u64;
    for (i, (t, r)) in txs_json.iter().zip(rec_json).enumerate() {
        let env = tx_envelope(t).map_err(|w| bad(number, format!("tx {i}: {w}")))?;
        let want = hash_of(t, "hash").map_err(|w| bad(number, format!("tx {i}: {w}")))?;
        if keccak256(&env) != want {
            return Err(bad(
                number,
                format!("tx {i}: envelope does not hash to its hash"),
            ));
        }
        let renv = receipt_envelope(r).map_err(|w| bad(number, format!("receipt {i}: {w}")))?;
        // Derived fields the JSON prints, against the hashed bytes.
        let dt = decode::tx(&env).map_err(|x| bad(number, format!("tx {i}: {x:?}")))?;
        let dr = decode::receipt(&renv).map_err(|x| bad(number, format!("receipt {i}: {x:?}")))?;
        let gas = dr.cumulative_gas.checked_sub(cum).unwrap_or(u64::MAX);
        cum = dr.cumulative_gas;
        let egp = match dt.fees {
            Fees::GasPrice(g) => g,
            Fees::Dynamic {
                max_fee,
                max_priority,
            } => max_fee.min(h.base_fee.saturating_add(max_priority)),
        };
        let mismatch = |k: &str| {
            bad(
                number,
                format!("receipt {i}: `{k}` is not what the hashed bytes give"),
            )
        };
        if r.get("transactionHash").and_then(Value::as_str) != t.get("hash").and_then(Value::as_str)
        {
            return Err(mismatch("transactionHash"));
        }
        if qty64(r, "gasUsed").ok() != Some(gas) {
            return Err(mismatch("gasUsed"));
        }
        if qty(r, "effectiveGasPrice").ok() != Some(egp) {
            return Err(mismatch("effectiveGasPrice"));
        }
        let created = !matches!(r.get("contractAddress"), None | Some(Value::Null));
        if created != dt.creates {
            return Err(mismatch("contractAddress"));
        }
        if dt.blobs > 0 {
            let fee = h.excess_blob_gas.and_then(|x| {
                decode::fake_exponential(
                    decode::MIN_BLOB_BASE_FEE,
                    u128::from(x),
                    decode::BPO2_BLOB_UPDATE_FRACTION,
                )
            });
            if fee.is_none() || qty(r, "blobGasPrice").ok() != fee {
                return Err(mismatch("blobGasPrice"));
            }
            if qty64(r, "blobGasUsed").ok() != Some(dt.blobs * GAS_PER_BLOB) {
                return Err(mismatch("blobGasUsed"));
            }
        }
        txs.push(env);
        recs.push(renv);
    }
    if trie::ordered_root(&txs) != h.transactions_root {
        return Err(bad(
            number,
            "the transactions do not rebuild the transactions root",
        ));
    }
    if trie::ordered_root(&recs) != h.receipts_root {
        return Err(bad(number, "the receipts do not rebuild the receipts root"));
    }
    if decode::next_base_fee(&p) != Some(h.base_fee) {
        return Err(bad(
            number,
            "the base fee is not EIP-1559's from the parent",
        ));
    }
    // P4A-R2-3: the adapter applies the recipe's other header rules too, so a self-consistent
    // answer that breaks them is refused at fetch, not only when the shard runs.
    if h.excess_blob_gas.is_some() && decode::next_excess_blob_gas(&p) != h.excess_blob_gas {
        return Err(bad(
            number,
            "the excess blob gas is not EIP-7918's from the parent",
        ));
    }
    if cum != h.gas_used {
        return Err(bad(
            number,
            "the last cumulative gas is not the header's gas used",
        ));
    }
    Ok(EthBlock {
        number,
        hash,
        header,
        txs,
        receipts: recs,
    })
}

// ---- Window folders ----

fn read_gz(path: &Path) -> Result<Vec<u8>, EthAdapterError> {
    let f = std::fs::File::open(path)
        .map_err(|e| EthAdapterError::Fixture(format!("{}: {e}", path.display())))?;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(f)
        .take(MAX_ANSWER + 1)
        .read_to_end(&mut out)
        .map_err(|e| EthAdapterError::Fixture(format!("{}: {e}", path.display())))?;
    if out.len() as u64 > MAX_ANSWER {
        return Err(EthAdapterError::Fixture(format!(
            "{}: larger than {MAX_ANSWER} bytes",
            path.display()
        )));
    }
    Ok(out)
}

/// A window folder as `scripts/refresh-eth-onchain.py` writes it: per block
/// `<n>.block.json.gz` and `<n>.receipts.json.gz` (raw JSON-RPC answers) and
/// `MANIFEST.json` with the SHA-256 of every answer.
pub struct Window {
    dir: std::path::PathBuf,
    sha256: BTreeMap<String, String>,
    /// The manifest as written (endpoints, fetch time, cross-checked hashes).
    pub manifest: Value,
}

impl Window {
    /// Open a folder and read its manifest.
    pub fn open(dir: &Path) -> Result<Self, EthAdapterError> {
        let path = dir.join("MANIFEST.json");
        let text = std::fs::read(&path)
            .map_err(|e| EthAdapterError::Fixture(format!("{}: {e}", path.display())))?;
        let manifest: Value = serde_json::from_slice(&text)
            .map_err(|e| EthAdapterError::Fixture(format!("{}: {e}", path.display())))?;
        let mut sha256 = BTreeMap::new();
        for (k, v) in manifest["sha256"].as_object().into_iter().flatten() {
            let s = v
                .as_str()
                .ok_or_else(|| EthAdapterError::Fixture(format!("manifest: {k}")))?;
            sha256.insert(k.clone(), s.to_string());
        }
        if manifest["chain"].as_str() != Some(CHAIN_ID) {
            return Err(EthAdapterError::Fixture("manifest: not eth.mainnet".into()));
        }
        Ok(Window {
            dir: dir.to_path_buf(),
            sha256,
            manifest,
        })
    }

    /// The manifest's `from` and `to`.
    pub fn range(&self) -> Option<(u64, u64)> {
        Some((
            self.manifest["from"].as_u64()?,
            self.manifest["to"].as_u64()?,
        ))
    }

    /// The `result` of one answer file, after its SHA-256 matched the manifest.
    pub fn answer(&self, name: &str) -> Result<Value, EthAdapterError> {
        let want = self
            .sha256
            .get(name)
            .ok_or_else(|| EthAdapterError::Checksum(format!("{name}: not in the manifest")))?;
        let raw = read_gz(&self.dir.join(name))?;
        let got = hex::encode(Sha256::digest(&raw));
        if &got != want {
            return Err(EthAdapterError::Checksum(format!(
                "{name}: {got}, manifest {want}"
            )));
        }
        let mut doc: Value = serde_json::from_slice(&raw)
            .map_err(|e| EthAdapterError::Fixture(format!("{name}: {e}")))?;
        Ok(doc["result"].take())
    }

    /// True when the folder has both answers of block `n`.
    pub fn has(&self, n: u64) -> bool {
        self.sha256.contains_key(&format!("{n}.block.json.gz"))
            && self.sha256.contains_key(&format!("{n}.receipts.json.gz"))
    }
}

/// P4A-R1-3: the window was cross-checked by a second operator: `check_rpc` set on another host
/// than `rpc`, and its confirmed hashes include the window's parent, first and last block with
/// the hashes of the answers. Returns why not.
pub fn cross_checked(w: &Window) -> Result<(), String> {
    let m = &w.manifest;
    let host = |k: &str| {
        m[k].as_str().and_then(|u| u.split("://").nth(1)).map(|r| {
            r.split(['/', ':'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase()
        })
    };
    match (host("rpc"), host("check_rpc")) {
        (Some(a), Some(b)) if !b.is_empty() && a != b => {}
        _ => return Err("no second operator on another host checked this window".into()),
    }
    let (from, to) = w.range().ok_or("the manifest names no range")?;
    let confirmed: BTreeMap<u64, String> = m["cross_checked"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| Some((c["number"].as_u64()?, c["hash"].as_str()?.to_string())))
        .collect();
    for n in [from - 1, from, to] {
        let b = w
            .answer(&format!("{n}.block.json.gz"))
            .map_err(|e| e.to_string())?;
        if confirmed.get(&n).map(String::as_str) != b["hash"].as_str() {
            return Err(format!("block {n}: no matching second-operator hash"));
        }
    }
    Ok(())
}

/// Header RLP of block `n` from its block answer, checked against its hash.
pub fn read_header(w: &Window, n: u64) -> Result<Vec<u8>, EthAdapterError> {
    let b = w.answer(&format!("{n}.block.json.gz"))?;
    let header = header_rlp(&b).map_err(|x| bad(n, x))?;
    if keccak256(&header) != hash_of(&b, "hash").map_err(|x| bad(n, x))? {
        return Err(bad(n, "the header does not hash to the block hash"));
    }
    Ok(header)
}

/// Load and check blocks `from..=to` (and block `from - 1`'s header) from a
/// window folder. Returns the canonical source bytes (Borsh
/// `EthChainSource`) and the snapshot that pins them.
pub fn load_eth_range(
    w: &Window,
    from: u64,
    to: u64,
) -> Result<(Vec<u8>, Snapshot), EthAdapterError> {
    if from == 0 || from > to {
        return Err(EthAdapterError::Fixture(format!("bad range {from}-{to}")));
    }
    let parent_header = read_header(w, from - 1)?;
    let mut prev = parent_header.clone();
    let mut blocks = Vec::new();
    for n in from..=to {
        let b = w.answer(&format!("{n}.block.json.gz"))?;
        let r = w.answer(&format!("{n}.receipts.json.gz"))?;
        let block = eth_block(n, &b, &r, &prev)?;
        prev = block.header.clone();
        blocks.push(block);
    }
    let tip = blocks.last().map(|b| b.hash).unwrap_or_default();
    let source = EthChainSource {
        schema_version: ETH_SCHEMA_VERSION,
        chain: CHAIN_ID.into(),
        parent_header,
        blocks,
    };
    let bytes = borsh::to_vec(&source).map_err(|e| EthAdapterError::Fixture(e.to_string()))?;
    let snapshot = Snapshot::BlockRange {
        chain: ChainId(CHAIN_ID.into()),
        from_height: from,
        to_height: to,
        tip_hash: Hash32(tip),
        content_hash: core_keccak(&bytes),
    };
    Ok((bytes, snapshot))
}

/// Decode a source back (for tests and the browser's rerun check).
pub fn decode_source(bytes: &[u8]) -> Option<EthChainSource> {
    EthChainSource::try_from_slice(bytes).ok()
}

// ---- Website window ----

/// One chain-statistics shard of a published window: its range and root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EthShard {
    /// First block.
    pub from: u64,
    /// Last block.
    pub to: u64,
    /// Merkle root of the shard's leaves.
    pub root: Hash32,
}

/// The hash a website pins for a window of Ethereum chain statistics: the
/// domain, the recipe id, the tip block hash, then each shard as
/// `from (u64 BE) || to (u64 BE) || root`. Any edited row changes a root;
/// any moved, dropped or added shard changes the list.
pub fn eth_chain_stats_commitment(recipe: &Hash32, tip: &[u8; 32], shards: &[EthShard]) -> Hash32 {
    let mut b = Vec::with_capacity(23 + 64 + shards.len() * 48);
    b.extend_from_slice(b"onyx-eth-chain-stats-v1");
    b.extend_from_slice(&recipe.0);
    b.extend_from_slice(tip);
    for s in shards {
        b.extend_from_slice(&s.from.to_be_bytes());
        b.extend_from_slice(&s.to.to_be_bytes());
        b.extend_from_slice(&s.root.0);
    }
    core_keccak(&b)
}

/// `YYYY-MM-DD` of a unix time, UTC (proleptic Gregorian, days-from-civil inverse).
pub fn utc_date(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `HH:MM` of a unix time, UTC.
pub fn utc_time(secs: u64) -> String {
    let s = secs % 86_400;
    format!("{:02}:{:02}", s / 3_600, (s % 3_600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_commitment_moves_with_every_field() {
        let s = |from, to, a: u8| EthShard {
            from,
            to,
            root: Hash32([a; 32]),
        };
        let base = [s(1, 1, 1), s(2, 2, 3)];
        let c = |shards: &[EthShard], tip: [u8; 32], r: u8| {
            eth_chain_stats_commitment(&Hash32([r; 32]), &tip, shards)
        };
        let reference = c(&base, [0; 32], 7);
        assert_ne!(reference, c(&base, [1; 32], 7));
        assert_ne!(reference, c(&base, [0; 32], 8));
        assert_ne!(reference, c(&[s(1, 1, 1), s(2, 2, 4)], [0; 32], 7));
        assert_ne!(reference, c(&[s(1, 1, 1), s(3, 3, 3)], [0; 32], 7));
        assert_ne!(reference, c(&[s(1, 1, 1)], [0; 32], 7));
        assert_ne!(reference, c(&[s(2, 2, 3), s(1, 1, 1)], [0; 32], 7));
        assert_eq!(reference, c(&base, [0; 32], 7));
    }

    #[test]
    fn utc_dates_and_times() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(1_769_904_000), "2026-02-01");
        assert_eq!(utc_date(1_798_761_599), "2026-12-31");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
        assert_eq!(utc_time(1_769_904_000 + 3_600 * 13 + 60 * 7 + 59), "13:07");
    }

    #[test]
    fn quantities_must_be_minimal() {
        let v = json!({"a": "0x0", "b": "0x01", "c": "0x", "d": "0x1f", "e": "12"});
        assert_eq!(qty(&v, "a"), Ok(0));
        assert!(qty(&v, "b").is_err());
        assert!(qty(&v, "c").is_err());
        assert_eq!(qty(&v, "d"), Ok(31));
        assert!(qty(&v, "e").is_err());
        assert_eq!(qty_bytes(&v, "a"), Ok(vec![]));
    }

    #[test]
    fn a_header_with_an_unknown_field_is_refused() {
        let mut b = serde_json::Map::new();
        for (k, q) in HEADER_FIELDS.iter().take(16) {
            b.insert(
                (*k).into(),
                json!(if *q {
                    "0x1".to_string()
                } else {
                    format!("0x{}", "00".repeat(32))
                }),
            );
        }
        b.insert("logsBloom".into(), json!(format!("0x{}", "00".repeat(256))));
        assert!(header_rlp(&Value::Object(b.clone())).is_ok());
        let mut extra = b.clone();
        extra.insert("blockAccessListHash".into(), json!("0x00"));
        assert!(header_rlp(&Value::Object(extra))
            .err()
            .is_some_and(|e| e.contains("unknown block field")));
        // A gap in the field list (a Cancun field without Shanghai's) is refused.
        let mut gap = b;
        gap.insert("blobGasUsed".into(), json!("0x0"));
        assert!(header_rlp(&Value::Object(gap)).is_err());
    }
}
