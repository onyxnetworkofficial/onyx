//! The signed book's registered keys, read from a chain node (ADR-0066
//! slice 3; THREATS PUBON-19).
//!
//! A reviewer never takes the list of registered keys from the applicant.
//! This reader gives it from the book itself, at one finalized block (the
//! rule of every other bulk read, [`decision_block`]):
//!
//! - every `KeyRegistered` log of the book from `from_block` to that block,
//!   in ranges of at most `range` blocks (`indexer::Spans`: a range the
//!   node refuses is halved; a rate limit is waited out);
//! - each log checked against the book's state at the same block: the key
//!   id is `keyId(source, signer)`, `keyState` holds the signer and the
//!   source, and `originOfSource` holds the origin. A log that does not
//!   decode or does not match stops the read: nothing is skipped;
//! - [`BookKeys::confirm_free`]: what the list shows as free is read again
//!   from the book's mappings, so a scan that started after the book's
//!   first key cannot make a taken id, origin or signer look free.

use crate::bulk_serve::{call_at, decision_block, word, At};
use crate::early_chain::rate_limited;
use crate::indexer::Spans;
use crate::roots::JsonRpc;
use onyx_bulk::wire::hex0x;
use onyx_core::{keccak256, Hash32};
use serde_json::{json, Value};

/// Blocks per `eth_getLogs` query when the caller names none. A node with
/// a lower cap refuses the range and the walk halves it (the public Base
/// Sepolia node answers 200 blocks, and that chain makes about five blocks
/// a second: a reviewer reads the book from a node that answers wide
/// ranges, or waits).
pub const DEFAULT_LOGS_RANGE: u64 = 10_000;
/// Most `eth_getLogs` queries of one read: a start block far below the
/// book's own stops the tool instead of asking a node without end.
pub const MAX_LOG_QUERIES: u64 = 200_000;
/// Tries of a log query that failed otherwise before its range is halved.
const REFUSED_TRIES: u64 = 2;
/// Waits between tries of a rate-limited log query (about a minute in all).
const RATE_LIMIT_WAITS_SECS: [u64; 6] = [1, 2, 4, 8, 16, 30];

/// The blocks the known books were deployed in (review S3-R1 M4): a scan of
/// such a book starts there, whatever the caller names. The test network's
/// book: docs/ops/deployment.md, under the table with the OnyxSignedBook row.
pub const KNOWN_BOOKS: &[(u64, [u8; 20], u64)] = &[(
    84_532,
    [
        0x04, 0xbb, 0x31, 0xd8, 0x0c, 0x8c, 0xe9, 0xdf, 0x02, 0xc8, 0xa0, 0x34, 0x86, 0xb5, 0x81,
        0x2e, 0xc6, 0xe5, 0xcc, 0xc5,
    ],
    52_118_336,
)];

/// The deploy block of a known book.
pub fn known_deploy_block(chain_id: u64, book: &[u8; 20]) -> Option<u64> {
    KNOWN_BOOKS
        .iter()
        .find(|(c, b, _)| *c == chain_id && b == book)
        .map(|(_, _, n)| *n)
}

/// The first block of a scan is the book's first: a known book's pinned
/// deploy block, block 0, or a block before which the address holds no code
/// (`eth_getCode` at `from_block - 1`). A later start would hide earlier
/// keys from the look-alike and overlap rules, which no mapping read can
/// make up for (review S3-R1 M4). A node that does not keep old state
/// cannot show it: nothing is decided.
pub async fn check_first_block(
    rpc: &dyn JsonRpc,
    chain_id: u64,
    book: &[u8; 20],
    from_block: u64,
) -> Result<(), String> {
    if let Some(deploy) = known_deploy_block(chain_id, book) {
        return if from_block == deploy {
            Ok(())
        } else {
            Err(format!(
                "this book was deployed in block {deploy}: the scan starts there, not at {from_block}"
            ))
        };
    }
    if from_block == 0 {
        return Ok(());
    }
    let before = from_block - 1;
    let code = rpc
        .call("eth_getCode", json!([hex0x(book), format!("0x{before:x}")]))
        .await
        .map_err(|e| format!("whether the book existed before block {from_block} (a node that keeps old state is needed): {e}"))?;
    match code.as_str() {
        Some("0x") => Ok(()),
        Some(_) => Err(format!(
            "the book already has code at block {before}: the scan must start at the block it was deployed in, or keys registered before block {from_block} are missed"
        )),
        None => Err("the node's eth_getCode answer does not decode".into()),
    }
}

/// What a scan keeps between runs (`--scan-cache`): the keys read so far
/// and the finalized block they are valid to, with its hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanCache {
    /// The chain.
    pub chain_id: u64,
    /// The book.
    pub book: [u8; 20],
    /// The first block scanned.
    pub from_block: u64,
    /// The last block scanned, and its hash: a node on another fork, or a
    /// reorg below it, shows another hash.
    pub to: At,
    /// The keys registered in `from_block..=to`.
    pub keys: Vec<BookKey>,
}

fn h32(v: &Value) -> Option<Hash32> {
    Some(Hash32(hex_field(v, 32)?.try_into().ok()?))
}

impl ScanCache {
    /// The file's JSON.
    pub fn to_json(&self) -> Value {
        json!({
            "version": 1, "chain_id": self.chain_id, "book": hex0x(&self.book),
            "from_block": self.from_block, "to_block": self.to.0, "to_block_hash": self.to.1.to_string(),
            "keys": self.keys.iter().map(|k| json!({
                "key": k.key.to_string(), "publisher": k.publisher, "origin": k.origin,
                "signer": hex0x(&k.signer), "provenance": k.provenance.to_string(),
                "block": k.block, "tx": k.tx.to_string(),
            })).collect::<Vec<_>>(),
        })
    }

    /// Read a cache file's JSON. A key's life is not kept: it is read from
    /// the book again at every use.
    pub fn from_json(v: &Value) -> Option<ScanCache> {
        if v["version"] != 1 {
            return None;
        }
        let keys = v["keys"]
            .as_array()?
            .iter()
            .map(|k| {
                Some(BookKey {
                    key: h32(&k["key"])?,
                    publisher: k["publisher"].as_str()?.to_string(),
                    origin: k["origin"].as_str()?.to_string(),
                    signer: hex_field(&k["signer"], 20)?.try_into().ok()?,
                    provenance: h32(&k["provenance"])?,
                    life: 0,
                    suspended: false,
                    block: k["block"].as_u64()?,
                    tx: h32(&k["tx"])?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(ScanCache {
            chain_id: v["chain_id"].as_u64()?,
            book: hex_field(&v["book"], 20)?.try_into().ok()?,
            from_block: v["from_block"].as_u64()?,
            to: (v["to_block"].as_u64()?, h32(&v["to_block_hash"])?),
            keys,
        })
    }
}

/// The cache file for a book under `dir`: `<chain>-<book>.json`.
pub fn scan_cache_path(
    dir: &std::path::Path,
    chain_id: u64,
    book: &[u8; 20],
) -> std::path::PathBuf {
    dir.join(format!("{chain_id}-{}.json", hex0x(book)))
}

fn load_cache(path: &std::path::Path) -> Result<Option<ScanCache>, String> {
    let b = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("the scan cache {}: {e}", path.display())),
    };
    serde_json::from_slice::<Value>(&b)
        .ok()
        .and_then(|v| ScanCache::from_json(&v))
        .map(Some)
        .ok_or_else(|| {
            format!(
                "the scan cache {} does not parse: remove it to scan again",
                path.display()
            )
        })
}

/// Write the cache for its owner only, through a temporary file beside it.
fn save_cache(path: &std::path::Path, c: &ScanCache) -> Result<(), String> {
    use std::io::Write;
    let err = |e: std::io::Error| format!("the scan cache {}: {e}", path.display());
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(err)?;
    }
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&tmp).map_err(err)?;
    f.write_all(format!("{:#}\n", c.to_json()).as_bytes())
        .map_err(err)?;
    std::fs::rename(&tmp, path).map_err(err)
}

async fn block_hash(rpc: &dyn JsonRpc, number: u64) -> Result<Hash32, String> {
    let b = rpc
        .call(
            "eth_getBlockByNumber",
            json!([format!("0x{number:x}"), false]),
        )
        .await?;
    h32(&b["hash"]).ok_or_else(|| format!("the node has no block {number}"))
}

/// Log ranges between two saves of the cache during a long scan.
const SAVE_EVERY_QUERIES: u64 = 250;

/// One key the book holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookKey {
    /// The key id (`keyId(source, signer)`).
    pub key: Hash32,
    /// The publisher (source) id.
    pub publisher: String,
    /// The origin the id is bound to.
    pub origin: String,
    /// The key's address.
    pub signer: [u8; 20],
    /// The keys document's Keccak the reviewer registered.
    pub provenance: Hash32,
    /// `Life`: 1 active, 2 retired, 3 compromised.
    pub life: u8,
    /// Suspended after an equivocation.
    pub suspended: bool,
    /// The block of its `KeyRegistered` log.
    pub block: u64,
    /// The transaction of that log.
    pub tx: Hash32,
}

/// The book's keys at one finalized block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookKeys {
    /// The finalized block every read was made at.
    pub at: At,
    /// The first block scanned.
    pub from_block: u64,
    /// The book's reviewer.
    pub reviewer: [u8; 20],
    /// Every key registered from `from_block` on, in log order.
    pub keys: Vec<BookKey>,
}

fn selector(sig: &str) -> Vec<u8> {
    keccak256(sig.as_bytes()).0[..4].to_vec()
}

fn u64_of(w: &[u8; 32]) -> Option<u64> {
    if w[..24].iter().any(|x| *x != 0) {
        return None;
    }
    Some(u64::from_be_bytes(w[24..].try_into().ok()?))
}

fn hex_field(v: &Value, len: usize) -> Option<Vec<u8>> {
    let b = hex::decode(v.as_str()?.strip_prefix("0x")?).ok()?;
    (b.len() == len).then_some(b)
}

fn quantity(v: &Value) -> Option<u64> {
    u64::from_str_radix(v.as_str()?.strip_prefix("0x")?, 16).ok()
}

/// The ABI string whose offset is word `i` of `data`.
pub fn abi_string(data: &[u8], i: usize) -> Option<String> {
    let at = usize::try_from(u64_of(&word(data, i)?)?).ok()?;
    let len = usize::try_from(u64_of(data.get(at..at.checked_add(32)?)?.try_into().ok()?)?).ok()?;
    let start = at.checked_add(32)?;
    String::from_utf8(data.get(start..start.checked_add(len)?)?.to_vec()).ok()
}

/// `SourceKey::id`: keccak256("onyx-source-key-v1" ‖ u32le(len) ‖ source ‖ signer).
pub fn key_id(source: &str, signer: &[u8; 20]) -> Hash32 {
    let mut b = b"onyx-source-key-v1".to_vec();
    b.extend_from_slice(&(source.len() as u32).to_le_bytes());
    b.extend_from_slice(source.as_bytes());
    b.extend_from_slice(signer);
    keccak256(&b)
}

/// The topic of `KeyRegistered`.
pub fn key_registered_topic() -> Hash32 {
    keccak256(b"KeyRegistered(bytes32,address,string,string,bytes32,address,string)")
}

async fn mapping(
    rpc: &dyn JsonRpc,
    book: &[u8; 20],
    sig: &str,
    arg: &Hash32,
    at: &At,
) -> Result<[u8; 32], String> {
    let r = call_at(rpc, book, sig, arg, &at.1).await?;
    word(&r, 0).ok_or_else(|| format!("the book's {sig} does not decode"))
}

/// The book's state for one logged key at the block `at`: the key id is
/// `keyId(source, signer)`, `keyState` holds the signer and the source, and
/// `originOfSource` the origin. Fills the key's life and suspension.
async fn confirm_key(
    rpc: &dyn JsonRpc,
    book: &[u8; 20],
    at: &At,
    k: &mut BookKey,
) -> Result<(), String> {
    let what = format!("the key {} of {:?}", hex0x(&k.signer), k.publisher);
    if key_id(&k.publisher, &k.signer) != k.key {
        return Err(format!(
            "{what}: its log's key id is not keyId(source, signer)"
        ));
    }
    let st = call_at(rpc, book, "keyState(bytes32)", &k.key, &at.1).await?;
    let undecoded = || format!("{what}: keyState does not decode");
    let life = word(&st, 0)
        .and_then(|w| u64_of(&w))
        .ok_or_else(undecoded)?;
    let suspended = word(&st, 1)
        .and_then(|w| u64_of(&w))
        .ok_or_else(undecoded)?;
    let on_signer = word(&st, 2).ok_or_else(undecoded)?;
    let on_source = word(&st, 3).ok_or_else(undecoded)?;
    let source_hash = keccak256(k.publisher.as_bytes());
    if !(1..=3).contains(&life) || on_signer[12..] != k.signer || on_source != source_hash.0 {
        return Err(format!("{what}: the book's state does not hold its log"));
    }
    let bound = mapping(rpc, book, "originOfSource(bytes32)", &source_hash, at).await?;
    if bound != keccak256(k.origin.as_bytes()).0 {
        return Err(format!(
            "{what}: the book binds the id to another origin than its log's"
        ));
    }
    k.life = life as u8;
    k.suspended = suspended != 0;
    Ok(())
}

/// Read the book's keys (see the module text). `Err`: the chain could not
/// be read or a log does not match the book: nothing is decided.
pub async fn read_book_keys(
    rpc: &dyn JsonRpc,
    chain_id: u64,
    book: &[u8; 20],
    from_block: u64,
    range: u64,
) -> Result<BookKeys, String> {
    read_book_keys_cached(rpc, chain_id, book, from_block, range, None, 0).await
}

/// [`read_book_keys`] with a cache file (`--scan-cache`): the keys of the
/// blocks already scanned come from the file, and only the blocks after it
/// are asked for. The file is used only when it is for this chain, book and
/// first block (`pause_ms`: a wait after each log query), ends at or below the node's finalized block, and the node
/// shows the same hash for its last block (another fork or node is
/// refused: remove the file to scan again). Every cached key is checked
/// against the book's state again, as a logged one is. The file is written
/// for its owner only, during a long scan and at its end.
pub async fn read_book_keys_cached(
    rpc: &dyn JsonRpc,
    chain_id: u64,
    book: &[u8; 20],
    from_block: u64,
    range: u64,
    cache: Option<&std::path::Path>,
    pause_ms: u64,
) -> Result<BookKeys, String> {
    let at = decision_block(rpc, chain_id).await?;
    if from_block > at.0 {
        return Err(format!(
            "the first block to scan ({from_block}) is after the finalized block {}",
            at.0
        ));
    }
    let mut start = from_block;
    let mut cached: Vec<BookKey> = Vec::new();
    if let Some(c) = cache.map(load_cache).transpose()?.flatten() {
        let name = cache.map(|p| p.display().to_string()).unwrap_or_default();
        if c.chain_id != chain_id || c.book != *book || c.from_block != from_block {
            return Err(format!(
                "the scan cache {name} is for another chain, book or first block: name another file or remove it"
            ));
        }
        if c.to.0 > at.0 {
            return Err(format!(
                "the scan cache {name} ends at block {}, after this node's finalized block {}: another node or chain; nothing is read from it",
                c.to.0, at.0
            ));
        }
        if block_hash(rpc, c.to.0).await? != c.to.1 {
            return Err(format!(
                "the scan cache {name} ends at a block this node shows with another hash (a reorg, or another chain): remove it to scan again"
            ));
        }
        start = c.to.0.saturating_add(1);
        cached = c.keys;
    }
    let code = rpc
        .call(
            "eth_getCode",
            json!([hex0x(book), {"blockHash": at.1.to_string()}]),
        )
        .await?;
    if code.as_str().is_none_or(|c| c.len() <= 2) {
        return Err(format!(
            "no contract at {} at the finalized block {}",
            hex0x(book),
            at.0
        ));
    }
    let r = rpc
        .call(
            "eth_call",
            json!([{"to": hex0x(book), "data": hex0x(&selector("reviewer()"))}, {"blockHash": at.1.to_string()}]),
        )
        .await?;
    let r = hex_field(&r, 32).ok_or("the book's reviewer() does not decode")?;
    let mut reviewer = [0u8; 20];
    reviewer.copy_from_slice(&r[12..]);

    check_first_block(rpc, chain_id, book, from_block).await?;
    let topic = key_registered_topic().to_string();
    let mut keys: Vec<BookKey> = Vec::new();
    for mut k in cached {
        confirm_key(rpc, book, &at, &mut k).await?;
        keys.push(k);
    }
    // The repository's range walk: a range the node refuses (its span cap)
    // is halved and the smaller span stays; a rate limit is waited out.
    let mut spans = Spans::new(start, at.0, range);
    let mut queries = 0u64;
    while let Some((lo, hi)) = spans.range() {
        let span = format!("the book's logs, blocks {lo} to {hi}");
        queries += 1;
        if queries > MAX_LOG_QUERIES {
            return Err(format!(
                "blocks {from_block} to {} take over {MAX_LOG_QUERIES} log queries on this node: name the book's own first block, or use a node that answers wider ranges",
                at.0
            ));
        }
        let params = json!([{
            "address": hex0x(book),
            "fromBlock": format!("0x{lo:x}"),
            "toBlock": format!("0x{hi:x}"),
            "topics": [topic],
        }]);
        let mut waits = RATE_LIMIT_WAITS_SECS.iter();
        let mut refused = 0u64;
        let logs = loop {
            match rpc.call("eth_getLogs", params.clone()).await {
                Ok(Value::Array(a)) => break Some(a),
                Ok(_) => return Err(format!("{span}: not a list")),
                Err(e) if rate_limited(&e) => match waits.next() {
                    Some(s) => tokio::time::sleep(std::time::Duration::from_secs(*s)).await,
                    None => return Err(format!("{span}: rate-limited past the retries ({e})")),
                },
                Err(e) => {
                    // A passing failure (a timeout, a 5xx) is tried again
                    // before the range is taken for too wide: a halved
                    // span stays for the rest of the walk.
                    if refused < REFUSED_TRIES {
                        refused += 1;
                        tokio::time::sleep(std::time::Duration::from_secs(refused)).await;
                        continue;
                    }
                    if !spans.split() {
                        return Err(format!("{span}: {e}"));
                    }
                    break None;
                }
            }
        };
        let Some(logs) = logs else {
            continue;
        };
        for l in &logs {
            let bad = |what: &str| format!("{span}: a log {what}");
            if l["removed"].as_bool() == Some(true) {
                return Err(bad("is marked removed at a finalized block"));
            }
            let from = hex_field(&l["address"], 20).ok_or_else(|| bad("without its address"))?;
            if from != book {
                return Err(bad("of another contract"));
            }
            let topics = l["topics"]
                .as_array()
                .filter(|t| t.len() == 3 && t[0].as_str() == Some(topic.as_str()))
                .ok_or_else(|| bad("without the three topics of KeyRegistered"))?;
            let key: [u8; 32] = hex_field(&topics[1], 32)
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| bad("without its key"))?;
            let key = Hash32(key);
            let s = hex_field(&topics[2], 32).ok_or_else(|| bad("without its signer"))?;
            let mut signer = [0u8; 20];
            signer.copy_from_slice(&s[12..]);
            let data = l["data"]
                .as_str()
                .and_then(|d| hex::decode(d.strip_prefix("0x")?).ok())
                .ok_or_else(|| bad("without its data"))?;
            let (Some(publisher), Some(origin), Some(provenance)) =
                (abi_string(&data, 0), abi_string(&data, 1), word(&data, 2))
            else {
                return Err(bad("whose data does not decode"));
            };
            let block = quantity(&l["blockNumber"]).ok_or_else(|| bad("without its block"))?;
            let tx: [u8; 32] = hex_field(&l["transactionHash"], 32)
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| bad("without its transaction"))?;
            if block < lo || block > hi {
                return Err(bad("outside the range asked for"));
            }
            let mut k = BookKey {
                key,
                publisher,
                origin,
                signer,
                provenance: Hash32(provenance),
                life: 0,
                suspended: false,
                block,
                tx: Hash32(tx),
            };
            let what = format!("the key {} of {:?}", hex0x(&k.signer), k.publisher);
            // The log against the book's state at the same block.
            confirm_key(rpc, book, &at, &mut k).await?;
            if keys.iter().any(|k| k.key == key) {
                return Err(format!("{what}: registered twice in the logs"));
            }
            keys.push(k);
        }
        spans.done();
        if pause_ms > 0 {
            // A public node is asked gently.
            tokio::time::sleep(std::time::Duration::from_millis(pause_ms)).await;
        }
        // A long scan keeps what it has: the next run goes on from here.
        if let Some(path) =
            cache.filter(|_| queries.is_multiple_of(SAVE_EVERY_QUERIES) && hi < at.0)
        {
            let to = (hi, block_hash(rpc, hi).await?);
            save_cache(
                path,
                &ScanCache {
                    chain_id,
                    book: *book,
                    from_block,
                    to,
                    keys: keys.clone(),
                },
            )?;
        }
    }
    if let Some(path) = cache {
        save_cache(
            path,
            &ScanCache {
                chain_id,
                book: *book,
                from_block,
                to: at,
                keys: keys.clone(),
            },
        )?;
    }
    Ok(BookKeys {
        at,
        from_block,
        reviewer,
        keys,
    })
}

impl BookKeys {
    /// What the list shows as free for an application (`publisher`,
    /// `origin`, `signer`) is free in the book's own mappings at the same
    /// block. `Err` names what the scan missed: it started after the book's
    /// first key, so the list is not the book.
    pub async fn confirm_free(
        &self,
        rpc: &dyn JsonRpc,
        book: &[u8; 20],
        publisher: &str,
        origin: &str,
        signer: &[u8; 20],
    ) -> Result<(), String> {
        let late = |what: &str| {
            format!(
                "the book holds {what} but no KeyRegistered log from block {} on names it: the scan started after the book's first key (name an earlier first block)",
                self.from_block
            )
        };
        let zero = [0u8; 32];
        if !self.keys.iter().any(|k| k.signer == *signer) {
            let mut padded = [0u8; 32];
            padded[12..].copy_from_slice(signer);
            let k = mapping(rpc, book, "keyOfSigner(address)", &Hash32(padded), &self.at).await?;
            if k != zero {
                return Err(late("a key for this signer"));
            }
        }
        if !self.keys.iter().any(|k| k.publisher == publisher) {
            let s = keccak256(publisher.as_bytes());
            if mapping(rpc, book, "originOfSource(bytes32)", &s, &self.at).await? != zero {
                return Err(late("this publisher id"));
            }
        }
        if !self.keys.iter().any(|k| k.origin == origin) {
            let o = keccak256(origin.as_bytes());
            if mapping(rpc, book, "sourceOfOrigin(bytes32)", &o, &self.at).await? != zero {
                return Err(late("this origin"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x402::BoxFuture;
    use std::collections::BTreeMap;

    /// A book with scripted logs and state.
    #[derive(Default)]
    struct Book {
        chain: u64,
        finalized: Option<u64>,
        logs: Vec<Value>,
        /// Calldata (hex) to the returned words.
        calls: BTreeMap<String, Vec<u8>>,
        /// The block the book was deployed in (no code before it).
        deployed: u64,
        /// Another fork: every block below the finalized one has another hash.
        fork: u8,
        /// The widest log range the node answers (0: any).
        max_range: u64,
        /// Log queries answered "rate limit" before the node answers.
        limited: std::sync::Mutex<u32>,
        queries: std::sync::Mutex<Vec<(u64, u64)>>,
    }

    const BOOK: [u8; 20] = [0xb0; 20];

    fn w(n: u64) -> [u8; 32] {
        let mut x = [0u8; 32];
        x[24..].copy_from_slice(&n.to_be_bytes());
        x
    }

    fn addr_word(a: &[u8; 20]) -> [u8; 32] {
        let mut x = [0u8; 32];
        x[12..].copy_from_slice(a);
        x
    }

    fn log_data(source: &str, origin: &str) -> Vec<u8> {
        let mut d = Vec::new();
        let padded = |s: &str| 32 + s.len().div_ceil(32) * 32;
        let (a, b) = (160, 160 + padded(source));
        for x in [
            w(a as u64),
            w(b as u64),
            [7u8; 32],
            w(0),
            w((b + padded(origin)) as u64),
        ] {
            d.extend_from_slice(&x);
        }
        for s in [source, origin, "r"] {
            d.extend_from_slice(&w(s.len() as u64));
            let mut t = s.as_bytes().to_vec();
            t.resize(s.len().div_ceil(32) * 32, 0);
            d.extend_from_slice(&t);
        }
        d
    }

    impl Book {
        fn new() -> Book {
            let mut b = Book {
                chain: 31_337,
                finalized: Some(100),
                ..Book::default()
            };
            b.calls.insert(
                hex::encode(selector("reviewer()")),
                addr_word(&[9; 20]).to_vec(),
            );
            b
        }
        fn hash_of(&self, block: u64) -> [u8; 32] {
            [(block % 200) as u8 ^ self.fork ^ 0xf1; 32]
        }
        fn set(&mut self, sig: &str, arg: [u8; 32], ret: Vec<u8>) {
            let mut d = selector(sig);
            d.extend_from_slice(&arg);
            self.calls.insert(hex::encode(d), ret);
        }
        /// Register `signer` for `source` under `origin` at `block`, with
        /// the state the book would hold (`life`, `suspended`).
        fn register(
            &mut self,
            source: &str,
            origin: &str,
            signer: [u8; 20],
            block: u64,
            life: u64,
        ) {
            let key = key_id(source, &signer);
            let s = keccak256(source.as_bytes());
            let o = keccak256(origin.as_bytes());
            self.logs.push(json!({
                "address": hex0x(&BOOK),
                "topics": [key_registered_topic().to_string(), key.to_string(), hex0x(&addr_word(&signer))],
                "data": hex0x(&log_data(source, origin)),
                "blockNumber": format!("0x{block:x}"),
                "transactionHash": hex0x(&[block as u8; 32]),
            }));
            let mut st = Vec::new();
            for x in [w(life), w(0), addr_word(&signer), s.0, w(0), w(0)] {
                st.extend_from_slice(&x);
            }
            self.set("keyState(bytes32)", key.0, st);
            self.set("originOfSource(bytes32)", s.0, o.0.to_vec());
            self.set("sourceOfOrigin(bytes32)", o.0, s.0.to_vec());
            self.set("keyOfSigner(address)", addr_word(&signer), key.0.to_vec());
        }
    }

    impl JsonRpc for Book {
        fn call<'a>(&'a self, method: &'a str, p: Value) -> BoxFuture<'a, Result<Value, String>> {
            Box::pin(async move {
                match method {
                    "eth_chainId" => Ok(json!(format!("0x{:x}", self.chain))),
                    "eth_getBlockByNumber" => Ok(match self.finalized {
                        None => Value::Null,
                        Some(n) if p[0] == "finalized" => {
                            json!({"number": format!("0x{n:x}"), "hash": hex0x(&self.hash_of(n))})
                        }
                        Some(n) => match quantity(&p[0]) {
                            Some(b) if b <= n => json!({"hash": hex0x(&self.hash_of(b))}),
                            _ => Value::Null,
                        },
                    }),
                    "eth_getCode" => Ok(match quantity(&p[1]) {
                        Some(b) if b < self.deployed => json!("0x"),
                        _ => json!("0x60"),
                    }),
                    "eth_call" => {
                        assert_eq!(
                            p[1]["blockHash"],
                            hex0x(&self.hash_of(self.finalized.unwrap_or(0))),
                            "one finalized block"
                        );
                        let d = p[0]["data"].as_str().unwrap_or_default();
                        Ok(json!(hex0x(
                            self.calls
                                .get(d.trim_start_matches("0x"))
                                .map_or(&[0u8; 192][..], Vec::as_slice)
                        )))
                    }
                    "eth_getLogs" => {
                        let n = |k: &str| quantity(&p[0][k]).unwrap_or(0);
                        let (lo, hi) = (n("fromBlock"), n("toBlock"));
                        if let Ok(mut l) = self.limited.lock() {
                            if *l > 0 {
                                *l -= 1;
                                return Err("node answered HTTP 429".into());
                            }
                        }
                        if self.max_range > 0 && hi - lo >= self.max_range {
                            return Err("node answered HTTP 413".into());
                        }
                        if let Ok(mut q) = self.queries.lock() {
                            q.push((lo, hi));
                        }
                        Ok(Value::Array(
                            self.logs
                                .iter()
                                .filter(|l| {
                                    quantity(&l["blockNumber"]).is_some_and(|b| b >= lo && b <= hi)
                                })
                                .cloned()
                                .collect(),
                        ))
                    }
                    other => Err(format!("unexpected {other}")),
                }
            })
        }
    }

    #[tokio::test]
    async fn the_books_keys_are_read_at_one_finalized_block() {
        let mut b = Book::new();
        b.register(
            "river-levels.open",
            "data.example.org/river-levels",
            [1; 20],
            12,
            1,
        );
        b.register(
            "river-levels.open",
            "data.example.org/river-levels",
            [2; 20],
            40,
            2,
        );
        b.register("tide-tables.open", "tides.example.net", [3; 20], 99, 3);
        // A key registered after the finalized block is not read.
        b.register("late.open", "late.example.org", [4; 20], 101, 1);
        let got = read_book_keys(&b, 31_337, &BOOK, 0, 30).await.expect("ok");
        assert_eq!(got.at.0, 100);
        assert_eq!(got.reviewer, [9; 20]);
        assert_eq!(
            got.keys
                .iter()
                .map(|k| (
                    k.publisher.as_str(),
                    k.origin.as_str(),
                    k.signer[0],
                    k.life,
                    k.block
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "river-levels.open",
                    "data.example.org/river-levels",
                    1,
                    1,
                    12
                ),
                (
                    "river-levels.open",
                    "data.example.org/river-levels",
                    2,
                    2,
                    40
                ),
                ("tide-tables.open", "tides.example.net", 3, 3, 99),
            ]
        );
        assert_eq!(
            *b.queries.lock().expect("ok"),
            vec![(0, 29), (30, 59), (60, 89), (90, 100)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_node_with_a_range_cap_or_a_rate_limit_is_still_read_whole() {
        let mut b = Book::new();
        b.max_range = 7;
        b.limited = std::sync::Mutex::new(2);
        b.register("river-levels.open", "data.example.org", [1; 20], 3, 1);
        b.register("tide-tables.open", "tides.example.net", [3; 20], 100, 1);
        let got = read_book_keys(&b, 31_337, &BOOK, 0, 50).await.expect("ok");
        assert_eq!(got.keys.len(), 2);
        let q = b.queries.lock().expect("ok").clone();
        // Every block from 0 to 100 is in exactly one answered range.
        let answered: Vec<_> = q.iter().filter(|(lo, hi)| hi - lo < 7).collect();
        let mut next = 0;
        for (lo, hi) in answered {
            assert_eq!(*lo, next);
            next = hi + 1;
        }
        assert_eq!(next, 101);
        // A node that only ever says "rate limit" ends the read, undecided.
        b.limited = std::sync::Mutex::new(1_000);
        let e = read_book_keys(&b, 31_337, &BOOK, 0, 50)
            .await
            .expect_err("refused");
        assert!(e.contains("rate-limited"), "{e}");
    }

    #[tokio::test]
    async fn a_node_without_a_finalized_block_or_on_another_chain_is_not_read() {
        let mut b = Book::new();
        b.finalized = None;
        let e = read_book_keys(&b, 31_337, &BOOK, 0, 30)
            .await
            .expect_err("refused");
        assert!(e.contains("no finalized block"), "{e}");
        let b = Book::new();
        let e = read_book_keys(&b, 84_532, &BOOK, 0, 30)
            .await
            .expect_err("refused");
        assert!(e.contains("not 84532"), "{e}");
        let e = read_book_keys(&b, 31_337, &BOOK, 0, 0).await;
        assert!(e.is_ok(), "a range of 0 reads block by block");
        let e = read_book_keys(&b, 31_337, &BOOK, 101, 10)
            .await
            .expect_err("refused");
        assert!(e.contains("after the finalized block"), "{e}");
    }

    #[tokio::test]
    async fn a_log_the_books_state_does_not_hold_stops_the_read() {
        // Another contract's log, handed in by the node.
        let mut b = Book::new();
        b.register("river-levels.open", "data.example.org", [1; 20], 12, 1);
        b.logs[0]["address"] = json!(hex0x(&[0xee; 20]));
        let e = read_book_keys(&b, 31_337, &BOOK, 0, 50)
            .await
            .expect_err("refused");
        assert!(e.contains("another contract"), "{e}");
        // A log whose key the book does not hold.
        let mut b = Book::new();
        b.register("river-levels.open", "data.example.org", [1; 20], 12, 1);
        b.calls
            .retain(|k, _| !k.starts_with(&hex::encode(selector("keyState(bytes32)"))));
        let e = read_book_keys(&b, 31_337, &BOOK, 0, 50)
            .await
            .expect_err("refused");
        assert!(e.contains("does not hold its log"), "{e}");
        // A log that names another origin than the book binds.
        let mut b = Book::new();
        b.register("river-levels.open", "data.example.org", [1; 20], 12, 1);
        b.logs[0]["data"] = json!(hex0x(&log_data("river-levels.open", "other.example.org")));
        let e = read_book_keys(&b, 31_337, &BOOK, 0, 50)
            .await
            .expect_err("refused");
        assert!(e.contains("another origin"), "{e}");
        // A log whose data is cut.
        let mut b = Book::new();
        b.register("river-levels.open", "data.example.org", [1; 20], 12, 1);
        b.logs[0]["data"] = json!("0x00");
        let e = read_book_keys(&b, 31_337, &BOOK, 0, 50)
            .await
            .expect_err("refused");
        assert!(e.contains("does not decode"), "{e}");
    }

    #[tokio::test]
    async fn a_scan_that_starts_late_cannot_make_a_taken_name_look_free() {
        let mut b = Book::new();
        b.register("river-levels.open", "data.example.org", [1; 20], 12, 1);
        // A node that shows no code before block 50 although a key is older.
        b.deployed = 50;
        let late = read_book_keys(&b, 31_337, &BOOK, 50, 50).await.expect("ok");
        assert!(late.keys.is_empty());
        for (id, origin, signer, what) in [
            (
                "river-levels.open",
                "new.example.org",
                [5u8; 20],
                "publisher id",
            ),
            ("new.open", "data.example.org", [5; 20], "origin"),
            ("new.open", "new.example.org", [1; 20], "signer"),
        ] {
            let e = late
                .confirm_free(&b, &BOOK, id, origin, &signer)
                .await
                .expect_err("refused");
            assert!(e.contains(what) && e.contains("started after"), "{e}");
        }
        late.confirm_free(&b, &BOOK, "new.open", "new.example.org", &[5; 20])
            .await
            .expect("ok");
        // The full scan knows the key: a second key of the id is the
        // application's business, not a late scan.
        let full = read_book_keys(&b, 31_337, &BOOK, 0, 50).await.expect("ok");
        full.confirm_free(&b, &BOOK, "river-levels.open", "data.example.org", &[6; 20])
            .await
            .expect("ok");
    }

    /// Review S3-R1 M4: a scan that starts after the book's deployment
    /// would miss an earlier look-alike id or overlapping origin.
    #[tokio::test]
    async fn the_scan_starts_at_the_books_first_block() {
        let mut b = Book::new();
        b.deployed = 40;
        b.register("river-levels.open", "data.example.org", [1; 20], 45, 1);
        let e = read_book_keys(&b, 31_337, &BOOK, 60, 50)
            .await
            .expect_err("late");
        assert!(e.contains("already has code at block 59"), "{e}");
        assert!(b.queries.lock().expect("ok").is_empty(), "no log is read");
        assert_eq!(
            read_book_keys(&b, 31_337, &BOOK, 40, 50)
                .await
                .expect("at")
                .keys
                .len(),
            1
        );
        assert_eq!(
            read_book_keys(&b, 31_337, &BOOK, 7, 50)
                .await
                .expect("before")
                .keys
                .len(),
            1
        );
        // The test network's book is pinned to its deploy block.
        let (chain, book, deploy) = KNOWN_BOOKS[0];
        assert_eq!(hex0x(&book), "0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5");
        assert_eq!(known_deploy_block(chain, &book), Some(deploy));
        let mut live = Book::new();
        live.chain = chain;
        live.finalized = Some(deploy + 10);
        let e = read_book_keys(&live, chain, &book, deploy + 1, 50)
            .await
            .expect_err("pinned");
        assert!(e.contains(&format!("deployed in block {deploy}")), "{e}");
        assert!(check_first_block(&live, chain, &book, deploy).await.is_ok());
    }

    fn cache_file(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("onyx-scan-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        scan_cache_path(&d, 31_337, &BOOK)
    }

    #[tokio::test]
    async fn a_scan_goes_on_from_its_cache() {
        let path = cache_file("resume");
        let mut b = Book::new();
        b.register("river-levels.open", "data.example.org", [1; 20], 12, 1);
        let first = read_book_keys_cached(&b, 31_337, &BOOK, 0, 30, Some(&path), 0)
            .await
            .expect("first");
        assert_eq!(first.keys.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("file").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "the cache is its owner's only");
        }
        // The chain goes on: a new key, and the first one is retired.
        b.finalized = Some(160);
        b.register("tide-tables.open", "tides.example.net", [3; 20], 150, 1);
        let key = key_id("river-levels.open", &[1; 20]);
        let mut st = Vec::new();
        for x in [
            w(2),
            w(0),
            addr_word(&[1; 20]),
            keccak256(b"river-levels.open").0,
            w(0),
            w(0),
        ] {
            st.extend_from_slice(&x);
        }
        b.set("keyState(bytes32)", key.0, st);
        b.queries.lock().expect("ok").clear();
        let second = read_book_keys_cached(&b, 31_337, &BOOK, 0, 30, Some(&path), 0)
            .await
            .expect("second");
        assert_eq!(
            *b.queries.lock().expect("ok"),
            vec![(101, 130), (131, 160)],
            "only the new blocks"
        );
        assert_eq!(
            second
                .keys
                .iter()
                .map(|k| (k.publisher.as_str(), k.life, k.block))
                .collect::<Vec<_>>(),
            vec![("river-levels.open", 2, 12), ("tide-tables.open", 1, 150)],
            "a cached key's life is read from the book again"
        );
        // The same answer as a scan without the cache.
        let whole = read_book_keys(&b, 31_337, &BOOK, 0, 30)
            .await
            .expect("whole");
        assert_eq!(second, whole);
        // A cached key the book no longer holds as logged stops the read.
        b.calls
            .retain(|k, _| !k.starts_with(&hex::encode(selector("keyState(bytes32)"))));
        let e = read_book_keys_cached(&b, 31_337, &BOOK, 0, 30, Some(&path), 0)
            .await
            .expect_err("state");
        assert!(e.contains("does not hold its log"), "{e}");
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    #[tokio::test]
    async fn a_cache_for_another_book_chain_or_fork_is_refused() {
        let path = cache_file("refused");
        let mut b = Book::new();
        b.register("river-levels.open", "data.example.org", [1; 20], 12, 1);
        read_book_keys_cached(&b, 31_337, &BOOK, 0, 30, Some(&path), 0)
            .await
            .expect("first");
        // Another book, another first block.
        let other = [0xb1; 20];
        let e = read_book_keys_cached(&b, 31_337, &other, 0, 30, Some(&path), 0)
            .await
            .expect_err("book");
        assert!(e.contains("another chain, book or first block"), "{e}");
        let e = read_book_keys_cached(&b, 31_337, &BOOK, 1, 30, Some(&path), 0)
            .await
            .expect_err("first");
        assert!(e.contains("another chain, book or first block"), "{e}");
        // A node whose finalized block is below the cache's last block.
        b.finalized = Some(90);
        let e = read_book_keys_cached(&b, 31_337, &BOOK, 0, 30, Some(&path), 0)
            .await
            .expect_err("ahead");
        assert!(e.contains("after this node's finalized block 90"), "{e}");
        // A node on another fork: block 100 has another hash there.
        b.finalized = Some(140);
        b.fork = 0x55;
        let e = read_book_keys_cached(&b, 31_337, &BOOK, 0, 30, Some(&path), 0)
            .await
            .expect_err("fork");
        assert!(e.contains("another hash"), "{e}");
        // A file that is not a cache.
        std::fs::write(&path, "{}").expect("write");
        let e = read_book_keys_cached(&b, 31_337, &BOOK, 0, 30, Some(&path), 0)
            .await
            .expect_err("junk");
        assert!(e.contains("does not parse"), "{e}");
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }
}
