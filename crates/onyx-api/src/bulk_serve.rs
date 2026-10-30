//! Large publisher datasets on a node (contributor loop CL5, ADR-0059,
//! docs/spec/bulk-datasets.md). Test network only; reading is free.
//!
//! - `POST /v1/bulk/manifests` takes a manifest and the publisher's signed
//!   report, and keeps it only when the node's FINALIZED block shows the
//!   report recorded in the pinned `OnyxSignedBook`, the fact accepted on
//!   exactly that report, and the key active and not suspended. Version 1
//!   opens a dataset; each later version must be the append-only successor of
//!   the one held. A second, different anchored manifest for a version marks
//!   the dataset forked and stops serving it (THREATS CL5-2).
//! - `PUT /v1/bulk/{publisher}/{dataset}/shards/{root}` takes a shard file
//!   only if `root` is in a held manifest, the body is exactly the entry's
//!   size (no content encoding: the node never inflates a body, CL5-4), and
//!   the file checks against the schema and hashes to `root` with the
//!   entry's rows and key range (CL5-1). Shards go to the node's CAS (kind
//!   `Shard`) under the ADR-0052 tiers: hot, warm after a quiet period, back
//!   to hot when read, never cold.
//! - Per publisher at most [`BulkLimits::quota_bytes`] across its datasets'
//!   latest versions and [`BulkLimits::max_datasets`] datasets; the node at
//!   most [`BulkLimits::max_total_bytes`] (CL5-5).
//! - `GET /v1/bulk`, `GET /v1/bulk/{publisher}/{dataset}` and
//!   `GET /v1/bulk/{publisher}/{dataset}/rows?from=&to=&page=&limit=`: every
//!   row with its Merkle proof, its shard's entry with a proof under the
//!   manifest, the manifest header, the signed report and the anchor as read
//!   at a finalized block (rechecked at most [`ANCHOR_RECHECK_SECS`] apart:
//!   a key marked compromised stops serving, CL5-6).
//!
//! The upload routes are mounted outside the server guard's 12 s deadline
//! (a 64 MiB body may take longer): they have their own concurrency limit
//! and deadline ([`MAX_UPLOADS`], [`UPLOAD_TIMEOUT`]).

use crate::cache::Caches;
use crate::roots::JsonRpc;
use crate::{bad_request, internal, rate_scopes, ApiError, ClientKey};
use axum::body::Body;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use borsh::BorshDeserialize;
use onyx_bulk::manifest::{shards_tree, Manifest, ShardEntry};
use onyx_bulk::shard::check_shard;
use onyx_bulk::wire::{hex0x, row_json, shard_json};
use onyx_coordinator::cas::{Cas, FsCas, Kind};
use onyx_coordinator::tiers::{Tier, TieredStore};
use onyx_core::{keccak256, Hash32, SignedReport};
use onyx_merkle::MerkleTree;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// Largest `POST /v1/bulk/manifests` body (hex doubles the 8 MiB manifest).
pub const MAX_MANIFEST_BODY: usize = 2 * onyx_bulk::manifest::MAX_MANIFEST_BYTES + 64 * 1024;
/// Uploads (manifests and shards) handled at once; more wait.
pub const MAX_UPLOADS: usize = 2;
/// Deadline of one upload, body included.
pub const UPLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Shards loaded (read, checked, tree built) at once for queries.
pub const MAX_LOADS: usize = 2;
/// Shards kept loaded in memory (each up to 64 MiB plus its tree).
pub const LOADED_SHARDS: usize = 4;
/// Most rows in one page.
pub const MAX_PAGE_ROWS: u64 = 1_000;
/// Rows in a page when `limit` is not given.
pub const DEFAULT_PAGE_ROWS: u64 = 100;
/// How long (host clock) an anchor read stays good before a query reads it again.
pub const ANCHOR_RECHECK_SECS: u64 = 300;
/// Free answers per client per minute.
pub const FREE_PER_MINUTE: u32 = 60;
/// How long a key the book did not know (or had suspended) is refused
/// without a chain read (review CL5-R5).
pub const KEY_REFUSAL_SECS: u64 = 60;

/// The signed book manifests are anchored in: the node's pinned
/// `OnyxSignedBook` (its code hash and reviewer are checked at start by the
/// node, ROADMAP C10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BulkBook {
    /// Chain id.
    pub chain_id: u64,
    /// `OnyxSignedBook` address.
    pub book: [u8; 20],
}

/// Storage limits (THREATS CL5-5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BulkLimits {
    /// Bytes per publisher, summed over its datasets' latest versions.
    pub quota_bytes: u64,
    /// Datasets per publisher.
    pub max_datasets: usize,
    /// Bytes for every publisher together.
    pub max_total_bytes: u64,
    /// A hot shard not read for this long moves to warm.
    pub warm_after_secs: u64,
}

impl Default for BulkLimits {
    fn default() -> Self {
        BulkLimits {
            quota_bytes: 16 << 30,
            max_datasets: 64,
            max_total_bytes: 256 << 30,
            warm_after_secs: 7 * 86_400,
        }
    }
}

/// What the book says of a manifest's signed report at a finalized block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorView {
    /// Chain id.
    pub chain_id: u64,
    /// The book.
    pub book: [u8; 20],
    /// The payload digest (the book's record key).
    pub digest: Hash32,
    /// The fact id the book filed it under.
    pub fact: Hash32,
    /// `SignedReport::hash`.
    pub report_hash: Hash32,
    /// The key id.
    pub key: Hash32,
    /// The publisher's signer.
    pub signer: [u8; 20],
    /// Block the read was made at.
    pub block_number: u64,
    /// Its hash.
    pub block_hash: Hash32,
    /// When the book recorded the report (block time).
    pub recorded_at: u64,
    /// When the fact was accepted (block time).
    pub accepted_at: u64,
}

impl AnchorView {
    /// JSON form, with the signed report.
    pub fn to_json(&self, report: &SignedReport) -> Value {
        json!({
            "report": hex0x(&borsh::to_vec(report).unwrap_or_default()),
            "digest": self.digest.to_string(),
            "report_hash": self.report_hash.to_string(),
            "fact": self.fact.to_string(),
            "key": self.key.to_string(),
            "signer": hex0x(&self.signer),
            "sequence": report.payload.sequence,
            "chain_id": self.chain_id,
            "book": hex0x(&self.book),
            "recorded_at": self.recorded_at,
            "accepted_at": self.accepted_at,
            "checked_at": {"number": self.block_number, "hash": self.block_hash.to_string()},
        })
    }
}

/// Why a report does not anchor its manifest (yet).
// One per read, never stored in bulk: a `Box` buys nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnchorState {
    /// Recorded and accepted at the finalized block.
    Anchored(AnchorView),
    /// Refused, with a stable reason.
    Not(&'static str),
}

/// Reasons of [`AnchorState::Not`].
pub mod reason {
    /// The book has no record of the report at the finalized block (not
    /// posted, or not final yet).
    pub const NOT_FINAL: &str = "not_recorded_at_finalized_block";
    /// The book holds another signature over this payload.
    pub const OTHER_SIGNATURE: &str = "other_signature";
    /// The fact is disputed, withdrawn, or accepted on another report.
    pub const NOT_ACCEPTED: &str = "fact_not_accepted_on_this_report";
    /// The key is unknown or suspended, or another signer.
    pub const KEY: &str = "key_not_active";
}

fn word(b: &[u8], i: usize) -> Option<[u8; 32]> {
    b.get(i * 32..i * 32 + 32)?.try_into().ok()
}

fn word_u64(w: &[u8; 32]) -> Option<u64> {
    if w[..24].iter().any(|x| *x != 0) {
        return None;
    }
    Some(u64::from_be_bytes(w[24..].try_into().ok()?))
}

fn selector(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes());
    [h.0[0], h.0[1], h.0[2], h.0[3]]
}

fn qty(v: &Value) -> Result<u64, String> {
    let s = v.as_str().ok_or("not a quantity")?;
    u64::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| e.to_string())
}

fn hex_of(v: &Value) -> Result<Vec<u8>, String> {
    let s = v.as_str().ok_or("not a string")?;
    hex::decode(s.strip_prefix("0x").ok_or("no 0x")?).map_err(|e| e.to_string())
}

/// The finalized block: (number, hash).
pub async fn finalized(rpc: &dyn JsonRpc) -> Result<(u64, Hash32), String> {
    let b = rpc
        .call("eth_getBlockByNumber", json!(["finalized", false]))
        .await?;
    if b.is_null() {
        return Err("the node has no finalized block".into());
    }
    let n = qty(&b["number"])?;
    let h: [u8; 32] = hex_of(&b["hash"])?
        .try_into()
        .map_err(|_| "bad block hash".to_string())?;
    Ok((n, Hash32(h)))
}

async fn call_at(
    rpc: &dyn JsonRpc,
    book: &[u8; 20],
    sig: &str,
    arg: &Hash32,
    at: &Hash32,
) -> Result<Vec<u8>, String> {
    let mut d = selector(sig).to_vec();
    d.extend_from_slice(&arg.0);
    let r = rpc
        .call(
            "eth_call",
            json!([{"to": hex0x(book), "data": hex0x(&d)}, {"blockHash": at.to_string()}]),
        )
        .await?;
    hex_of(&r)
}

/// Read `report`'s anchor in `book` at the finalized block of `rpc`'s chain
/// (`signer` is the publisher key recovered from it). `Err` is a chain that
/// could not be read (nothing decided).
pub async fn read_anchor(
    rpc: &dyn JsonRpc,
    chain_id: u64,
    book: &[u8; 20],
    report: &SignedReport,
    signer: &[u8; 20],
) -> Result<AnchorState, String> {
    let id = qty(&rpc.call("eth_chainId", json!([])).await?)?;
    if id != chain_id {
        return Err(format!("the node's chain is {id}, not {chain_id}"));
    }
    let (block_number, block_hash) = finalized(rpc).await?;
    let digest = report.payload.digest();
    let bad = || "the signed book's answer does not decode".to_string();
    let r = call_at(rpc, book, "report(bytes32)", &digest, &block_hash).await?;
    if r.is_empty() || word(&r, 0).ok_or_else(bad)? == [0u8; 32] {
        return Ok(AnchorState::Not(reason::NOT_FINAL));
    }
    let fact = Hash32(word(&r, 1).ok_or_else(bad)?);
    let report_hash = Hash32(word(&r, 3).ok_or_else(bad)?);
    let recorded_at = word_u64(&word(&r, 6).ok_or_else(bad)?).ok_or_else(bad)?;
    if report_hash != report.hash() {
        return Ok(AnchorState::Not(reason::OTHER_SIGNATURE));
    }
    let f = call_at(rpc, book, "factView(bytes32)", &fact, &block_hash).await?;
    let state = word_u64(&word(&f, 0).ok_or_else(bad)?).ok_or_else(bad)?;
    let accepted = Hash32(word(&f, 1).ok_or_else(bad)?);
    let accepted_at = word_u64(&word(&f, 2).ok_or_else(bad)?).ok_or_else(bad)?;
    if state != 1 || accepted != report_hash {
        return Ok(AnchorState::Not(reason::NOT_ACCEPTED));
    }
    let key = report.payload.key;
    let k = call_at(rpc, book, "keyState(bytes32)", &key, &block_hash).await?;
    let life = word_u64(&word(&k, 0).ok_or_else(bad)?).ok_or_else(bad)?;
    let suspended = word_u64(&word(&k, 1).ok_or_else(bad)?).ok_or_else(bad)?;
    let on_chain_signer = word(&k, 2).ok_or_else(bad)?;
    // Registered (active, retired or compromised: the book's fact view above
    // already withdraws what a retired or compromised key may no longer
    // state, from its retirement or compromise point on), not suspended, and
    // the signer recovered from the report.
    if life == 0 || suspended != 0 || on_chain_signer[12..] != signer[..] {
        return Ok(AnchorState::Not(reason::KEY));
    }
    Ok(AnchorState::Anchored(AnchorView {
        chain_id,
        book: *book,
        digest,
        fact,
        report_hash,
        key,
        signer: *signer,
        block_number,
        block_hash,
        recorded_at,
        accepted_at,
    }))
}

/// One held version.
struct Held {
    manifest: Manifest,
    report: SignedReport,
    signer: [u8; 20],
    anchor: AnchorView,
}

#[derive(Default)]
struct Dataset {
    /// Index `v - 1` is version `v`.
    versions: Vec<Held>,
    /// Why the dataset is not served (two anchored manifests for a version).
    forked: Option<String>,
}

impl Dataset {
    fn latest(&self) -> Option<&Held> {
        self.versions.last()
    }
}

/// A shard read and checked, with its tree.
struct Loaded {
    bytes: Vec<u8>,
    offsets: Vec<(usize, usize)>,
    keys: Vec<i128>,
    tree: MerkleTree,
}

impl Loaded {
    fn row(&self, i: usize) -> &[u8] {
        let (a, b) = self.offsets[i];
        &self.bytes[a..b]
    }
}

type Key = (String, String);
/// A loaded shard's cache key: (root, keccak of the Borsh schema).
type LoadKey = (Hash32, Hash32);

/// The bulk datasets one node holds and serves.
pub struct BulkStore {
    pin: BulkBook,
    rpc: Arc<dyn JsonRpc>,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
    dir: PathBuf,
    limits: BulkLimits,
    tiers: Arc<TieredStore<FsCas, FsCas>>,
    datasets: RwLock<BTreeMap<Key, Dataset>>,
    /// Loaded shards by (root, schema hash): the same bytes under another
    /// dataset's schema are another load (review CL5-R3).
    loaded: Mutex<Vec<(LoadKey, Arc<Loaded>)>>,
    loads: tokio::sync::Semaphore,
    uploads: tokio::sync::Semaphore,
    touched: Mutex<BTreeMap<Hash32, u64>>,
    /// Manifest root -> (host time of the last chain read, whether it held).
    rechecked: Mutex<BTreeMap<Hash32, (u64, bool)>>,
    /// Keys the book did not know or had suspended, by host time: a POST
    /// under one is refused without a chain read for [`KEY_REFUSAL_SECS`].
    refused_keys: Mutex<BTreeMap<Hash32, u64>>,
    free_per_minute: u32,
    rate: Caches,
}

fn write_atomic(path: &FsPath, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

impl BulkStore {
    /// Open the store in `dir` (manifests, tiers file, hot shards) with warm
    /// shards in `warm`. Every held manifest is decoded and checked again
    /// (signature, successor rule); a damaged one fails the start.
    pub fn open(
        pin: BulkBook,
        rpc: Arc<dyn JsonRpc>,
        now: Arc<dyn Fn() -> u64 + Send + Sync>,
        dir: &FsPath,
        warm: &FsPath,
        limits: BulkLimits,
    ) -> Result<Self, String> {
        let s = BulkStore {
            pin,
            rpc,
            now,
            dir: dir.to_path_buf(),
            limits,
            tiers: Arc::new(TieredStore::new(
                FsCas::new(dir.join("hot")),
                FsCas::new(warm),
            )),
            datasets: RwLock::new(BTreeMap::new()),
            loaded: Mutex::new(Vec::new()),
            loads: tokio::sync::Semaphore::new(MAX_LOADS),
            uploads: tokio::sync::Semaphore::new(MAX_UPLOADS),
            touched: Mutex::new(BTreeMap::new()),
            rechecked: Mutex::new(BTreeMap::new()),
            refused_keys: Mutex::new(BTreeMap::new()),
            free_per_minute: FREE_PER_MINUTE,
            rate: Caches::new(1, 1),
        };
        std::fs::create_dir_all(dir).map_err(|e| format!("bulk dir: {e}"))?;
        let tiers = dir.join("tiers.json");
        if tiers.exists() {
            s.tiers
                .load(&tiers)
                .map_err(|e| format!("bulk tiers: {e}"))?;
        }
        s.load_manifests()?;
        let now = (s.now)();
        if let Ok(mut t) = s.touched.lock() {
            for (r, _) in s.tiers.bulk_tiers() {
                t.insert(r, now);
            }
        }
        Ok(s)
    }

    /// Answers per client per minute (tests).
    pub fn with_free_per_minute(mut self, n: u32) -> Self {
        self.free_per_minute = n.max(1);
        self
    }

    fn manifests_dir(&self) -> PathBuf {
        self.dir.join("manifests")
    }

    fn load_manifests(&self) -> Result<(), String> {
        let root = self.manifests_dir();
        let Ok(pubs) = std::fs::read_dir(&root) else {
            return Ok(());
        };
        let mut all = BTreeMap::new();
        for p in pubs.flatten() {
            for d in std::fs::read_dir(p.path())
                .map_err(|e| e.to_string())?
                .flatten()
            {
                let mut ds = Dataset::default();
                let mut v = 1u32;
                loop {
                    let f = d.path().join(format!("v{v}.bin"));
                    let Ok(bytes) = std::fs::read(&f) else { break };
                    let (m, r, a): (Vec<u8>, Vec<u8>, Vec<u8>) =
                        BorshDeserialize::try_from_slice(&bytes)
                            .map_err(|_| format!("{}: does not decode", f.display()))?;
                    let manifest =
                        Manifest::from_bytes(&m).map_err(|e| format!("{}: {e}", f.display()))?;
                    let report = SignedReport::try_from_slice(&r)
                        .map_err(|_| format!("{}: report does not decode", f.display()))?;
                    let signer = onyx_bulk::anchor::verify(&manifest.header, &report)
                        .map_err(|e| format!("{}: {e}", f.display()))?;
                    let anchor =
                        anchor_from_bytes(&a).ok_or_else(|| format!("{}: anchor", f.display()))?;
                    if manifest.header.version != v {
                        return Err(format!(
                            "{}: holds version {}",
                            f.display(),
                            manifest.header.version
                        ));
                    }
                    if let Some(prev) = ds.latest() {
                        manifest
                            .check_successor(&prev.manifest)
                            .map_err(|e| format!("{}: {e}", f.display()))?;
                    }
                    ds.versions.push(Held {
                        manifest,
                        report,
                        signer,
                        anchor,
                    });
                    v += 1;
                }
                if let Ok(why) = std::fs::read_to_string(d.path().join("forked.txt")) {
                    ds.forked = Some(why);
                }
                if let Some(h) = ds.latest() {
                    let key = (
                        h.manifest.header.publisher.0.clone(),
                        h.manifest.header.dataset.clone(),
                    );
                    all.insert(key, ds);
                }
            }
        }
        *self.datasets.write().map_err(|_| "lock")? = all;
        Ok(())
    }

    fn dataset_dir(&self, publisher: &str, dataset: &str) -> PathBuf {
        // Both are dotted ids (no `/`, never `..`), checked by the manifest.
        self.manifests_dir().join(publisher).join(dataset)
    }

    fn save_tiers(&self) -> Result<(), ApiError> {
        self.tiers
            .save(&self.dir.join("tiers.json"))
            .map_err(|e| internal(format!("tiers file: {e}")))
    }

    async fn anchor_of(
        &self,
        report: &SignedReport,
        signer: &[u8; 20],
    ) -> Result<AnchorState, ApiError> {
        read_anchor(
            self.rpc.as_ref(),
            self.pin.chain_id,
            &self.pin.book,
            report,
            signer,
        )
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "bulk_chain_unavailable",
                format!("the chain could not be read ({e}); try again"),
            )
            .with_header("retry-after", "30".into())
        })
    }

    /// Take a manifest and its signed report (see the module docs).
    pub async fn accept_manifest(
        &self,
        manifest: &[u8],
        report: &[u8],
    ) -> Result<(StatusCode, Value), ApiError> {
        let m = Manifest::from_bytes(manifest).map_err(|e| {
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "bulk_bad_manifest",
                e.to_string(),
            )
        })?;
        let r = SignedReport::try_from_slice(report)
            .map_err(|_| bad_request("report: not a Borsh SignedReport"))?;
        let signer = onyx_bulk::anchor::verify(&m.header, &r).map_err(|e| {
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "bulk_bad_manifest",
                format!("signature: {e}"),
            )
        })?;
        let h = &m.header;
        let key = (h.publisher.0.clone(), h.dataset.clone());
        let root = h.root();
        let now = (self.now)();
        if self
            .refused_keys
            .lock()
            .ok()
            .and_then(|k| k.get(&r.payload.key).copied())
            .is_some_and(|t| now.saturating_sub(t) < KEY_REFUSAL_SECS)
        {
            return Err(not_anchored(reason::KEY));
        }
        // Rules that need no chain read first.
        {
            let ds = self.datasets.read().map_err(|_| internal("lock"))?;
            let held = ds.get(&key);
            if let Some(d) = held {
                if let Some(same) = d.versions.get(h.version as usize - 1) {
                    // The same manifest and report again: nothing to do. The same
                    // manifest under another report (signed again by a new key
                    // after the old one was retired or compromised, THREATS
                    // CL5-6) is re-anchored below.
                    if same.manifest.header.root() == root && same.report == r {
                        return Ok((StatusCode::OK, self.missing_json(&same.manifest)));
                    }
                }
                if d.forked.is_some() {
                    return Err(forked(d));
                }
            }
            let latest = held.and_then(Dataset::latest).map(|x| &x.manifest);
            match (h.version, latest) {
                (1, None) => {}
                (v, Some(prev)) if v == prev.header.version + 1 => {
                    m.check_successor(prev).map_err(|e| {
                        ApiError::new(StatusCode::CONFLICT, "bulk_not_successor", e.to_string())
                    })?;
                }
                (v, Some(prev)) if v <= prev.header.version => {} // a second manifest for a held version: fork check below
                _ => {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "bulk_not_successor",
                        "versions are taken in order: upload the previous version first",
                    ))
                }
            }
            self.check_quota(&ds, &key, h.byte_count)?;
        }
        let anchor = match self.anchor_of(&r, &signer).await? {
            AnchorState::Anchored(a) => a,
            AnchorState::Not(why) => {
                if why == reason::KEY {
                    if let Ok(mut k) = self.refused_keys.lock() {
                        k.insert(r.payload.key, now);
                    }
                }
                return Err(not_anchored(why));
            }
        };
        // A held manifest with another root for this version: if the book no
        // longer holds it (its key was compromised from before it, review
        // CL5-R4), it and every later version are withdrawn and this one
        // takes its place; if it still holds, two anchored manifests exist
        // for one version (a fork).
        let held_other = {
            let ds = self.datasets.read().map_err(|_| internal("lock"))?;
            ds.get(&key)
                .and_then(|d| d.versions.get(h.version as usize - 1))
                .filter(|o| o.manifest.header.root() != root)
                .map(|o| (o.manifest.header.root(), o.report.clone(), o.signer))
        };
        let withdraw = match held_other {
            Some((other_root, other_report, other_signer)) => {
                match self.anchor_of(&other_report, &other_signer).await? {
                    AnchorState::Not(_) => Some(other_root),
                    AnchorState::Anchored(_) => None,
                }
            }
            None => None,
        };
        let mut ds = self.datasets.write().map_err(|_| internal("lock"))?;
        self.check_quota(&ds, &key, h.byte_count)?;
        let d = ds.entry(key.clone()).or_default();
        let dir = self.dataset_dir(&key.0, &key.1);
        if let Some(other_root) = withdraw {
            let i = h.version as usize - 1;
            if d.versions.get(i).map(|o| o.manifest.header.root()) == Some(other_root) {
                for w in d.versions.drain(i..) {
                    let v = w.manifest.header.version;
                    let to = dir
                        .join("withdrawn")
                        .join(format!("v{v}-{}.bin", w.manifest.header.root().to_hex()));
                    if let Some(parent) = to.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| internal(format!("bulk store: {e}")))?;
                    }
                    std::fs::rename(dir.join(format!("v{v}.bin")), &to)
                        .map_err(|e| internal(format!("bulk store: {e}")))?;
                }
                if let Some(prev) = d.versions.last() {
                    m.check_successor(&prev.manifest).map_err(|e| {
                        ApiError::new(StatusCode::CONFLICT, "bulk_not_successor", e.to_string())
                    })?;
                }
            }
        }
        if let Some(other) = d.versions.get_mut(h.version as usize - 1) {
            if other.manifest.header.root() == root && other.report != r {
                let rec = borsh::to_vec(&(
                    m.to_bytes(),
                    borsh::to_vec(&r).unwrap_or_default(),
                    anchor_to_bytes(&anchor),
                ))
                .map_err(|_| internal("encode"))?;
                write_atomic(&dir.join(format!("v{}.bin", h.version)), &rec)
                    .map_err(|e| internal(format!("bulk store: {e}")))?;
                other.report = r;
                other.signer = signer;
                other.anchor = anchor;
                self.mark(root, true);
                let mut body = self.missing_json(&other.manifest);
                body["reanchored"] = json!(true);
                return Ok((StatusCode::OK, body));
            }
            if other.manifest.header.root() != root {
                // Two manifests for one version, both signed and anchored: the
                // publisher's key equivocated (or was stolen). Stop serving.
                let why = format!(
                    "version {} has two anchored manifests: {} and {}",
                    h.version,
                    other.manifest.header.root(),
                    root
                );
                write_atomic(&dir.join("forked.txt"), why.as_bytes())
                    .map_err(|e| internal(format!("bulk store: {e}")))?;
                d.forked = Some(why);
                return Err(forked(d));
            }
            return Ok((StatusCode::OK, self.missing_json(&other.manifest)));
        }
        if d.versions.len() as u32 + 1 != h.version {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "bulk_not_successor",
                "another version was taken meanwhile; upload again",
            ));
        }
        let rec = borsh::to_vec(&(
            m.to_bytes(),
            borsh::to_vec(&r).unwrap_or_default(),
            anchor_to_bytes(&anchor),
        ))
        .map_err(|_| internal("encode"))?;
        write_atomic(&dir.join(format!("v{}.bin", h.version)), &rec)
            .map_err(|e| internal(format!("bulk store: {e}")))?;
        let body = self.missing_json(&m);
        self.mark(root, true);
        d.versions.push(Held {
            manifest: m,
            report: r,
            signer,
            anchor,
        });
        Ok((StatusCode::CREATED, body))
    }

    fn check_quota(
        &self,
        ds: &BTreeMap<Key, Dataset>,
        key: &Key,
        bytes: u64,
    ) -> Result<(), ApiError> {
        let mut publisher_bytes = bytes;
        let mut total = bytes;
        let mut count = 1usize;
        for (k, d) in ds {
            let b = d.latest().map_or(0, |h| h.manifest.header.byte_count);
            if k == key {
                continue;
            }
            total = total.saturating_add(b);
            if k.0 == key.0 {
                publisher_bytes = publisher_bytes.saturating_add(b);
                count += 1;
            }
        }
        if publisher_bytes > self.limits.quota_bytes || count > self.limits.max_datasets {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "bulk_quota_exceeded",
                format!(
                    "a publisher may hold at most {} bytes in {} datasets on this node",
                    self.limits.quota_bytes, self.limits.max_datasets
                ),
            ));
        }
        if total > self.limits.max_total_bytes {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "bulk_node_full",
                "this node holds no more bulk data",
            ));
        }
        Ok(())
    }

    fn missing_json(&self, m: &Manifest) -> Value {
        let missing: Vec<Value> = m
            .shards
            .iter()
            .filter(|e| self.tiers.tier(&e.root).is_none())
            .map(|e| json!({"index": e.index, "root": e.root.to_string(), "bytes": e.bytes}))
            .collect();
        json!({
            "publisher": m.header.publisher.0,
            "dataset": m.header.dataset,
            "version": m.header.version,
            "root": m.header.root().to_string(),
            "shards": m.header.shard_count,
            "missing": missing,
        })
    }

    fn entry_of(
        &self,
        publisher: &str,
        dataset: &str,
        root: &Hash32,
    ) -> Result<(ShardEntry, onyx_bulk::Schema), ApiError> {
        let ds = self.datasets.read().map_err(|_| internal("lock"))?;
        let d = ds
            .get(&(publisher.to_string(), dataset.to_string()))
            .ok_or_else(unknown_dataset)?;
        if d.forked.is_some() {
            return Err(forked(d));
        }
        let h = d.latest().ok_or_else(unknown_dataset)?;
        let e = h
            .manifest
            .shards
            .iter()
            .find(|e| e.root == *root)
            .copied()
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "bulk_unknown_shard",
                    "this root is in no held manifest of the dataset",
                )
            })?;
        Ok((e, h.manifest.header.schema.clone()))
    }

    /// Take a shard file (see the module docs).
    pub async fn accept_shard(
        &self,
        publisher: &str,
        dataset: &str,
        root: &Hash32,
        headers: &HeaderMap,
        body: Body,
    ) -> Result<(StatusCode, Value), ApiError> {
        if headers
            .get(header::CONTENT_ENCODING)
            .is_some_and(|v| v.as_bytes() != b"identity")
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "bulk_encoded_body",
                "send the shard file as is: the node never decompresses a body",
            ));
        }
        let (entry, schema) = self.entry_of(publisher, dataset, root)?;
        let done = json!({"root": root.to_string(), "index": entry.index, "stored": true});
        let already = self.tiers.tier(root).is_some();
        let declared = headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        if declared.is_some_and(|n| n != entry.bytes) {
            return Err(too_large(&entry));
        }
        let bytes = axum::body::to_bytes(body, entry.bytes as usize)
            .await
            .map_err(|_| too_large(&entry))?;
        if bytes.len() as u64 != entry.bytes {
            return Err(too_large(&entry));
        }
        let bytes = bytes.to_vec();
        let e = entry;
        let checked = tokio::task::spawn_blocking(move || {
            check_shard(&schema, &bytes)
                .map(|c| c.summary)
                .map_err(|err| err.to_string())
                .map(|s| (s, bytes))
        })
        .await
        .map_err(|_| internal("check task"))?;
        let (s, bytes) =
            checked.map_err(|why| mismatch(format!("the file does not check: {why}")))?;
        if s.root != e.root || s.rows != e.rows || s.key_min != e.key_min || s.key_max != e.key_max
        {
            return Err(mismatch(
                "the file's root, rows or key range differ from the manifest entry".into(),
            ));
        }
        // Stored even when the root is held already: every upload is checked
        // against this dataset's entry and schema (review CL5-R3), and the
        // put of identical bytes changes nothing.
        let tiers = self.tiers.clone();
        let tiers_file = self.dir.join("tiers.json");
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            tiers.put(Kind::Shard, &bytes).map_err(|e| e.to_string())?;
            tiers.save(&tiers_file).map_err(|e| e.to_string())
        })
        .await
        .map_err(|_| internal("store task"))?
        .map_err(|err| internal(format!("store: {err}")))?;
        if let Ok(mut t) = self.touched.lock() {
            t.insert(*root, (self.now)());
        }
        Ok((
            if already {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            },
            done,
        ))
    }

    /// Move hot shards not read for `warm_after_secs` to warm (ADR-0052).
    /// Returns how many moved.
    pub fn sweep(&self) -> usize {
        let now = (self.now)();
        let touched = self.touched.lock().map(|t| t.clone()).unwrap_or_default();
        let mut moved = 0;
        for (root, tier) in self.tiers.bulk_tiers() {
            let last = touched.get(&root).copied().unwrap_or(0);
            if tier == Tier::Hot
                && now.saturating_sub(last) >= self.limits.warm_after_secs
                && self.tiers.to_warm(&root).is_ok()
            {
                moved += 1;
            }
        }
        if moved > 0 {
            let _ = self.save_tiers();
        }
        moved
    }

    /// Tier of a shard (tests and listings).
    pub fn tier(&self, root: &Hash32) -> Option<Tier> {
        self.tiers.tier(root)
    }

    async fn load(
        &self,
        entry: &ShardEntry,
        schema: &onyx_bulk::Schema,
    ) -> Result<Arc<Loaded>, ApiError> {
        let root = &entry.root;
        let ck = (*root, keccak256(&borsh::to_vec(schema).unwrap_or_default()));
        if let Some(l) = self.cached(&ck) {
            return Ok(l);
        }
        let _permit = self.loads.acquire().await.map_err(|_| internal("loads"))?;
        if let Some(l) = self.cached(&ck) {
            return Ok(l);
        }
        let Some(tier) = self.tiers.tier(root) else {
            return Err(shard_missing(
                "the publisher has not uploaded this shard to this node yet",
            ));
        };
        let schema = schema.clone();
        let (want, tiers, tiers_file) = (*entry, self.tiers.clone(), self.dir.join("tiers.json"));
        let loaded = tokio::task::spawn_blocking(move || -> Result<Loaded, String> {
            if tier == Tier::Warm && tiers.warm_to_hot(&want.root).is_ok() {
                // Read on demand: back to hot (ADR-0052).
                tiers.save(&tiers_file).map_err(|e| e.to_string())?;
            }
            let bytes = tiers
                .get(Kind::Shard, &want.root)
                .ok_or("a held shard did not read back (lost or damaged on disk)")?;
            let c = check_shard(&schema, &bytes).map_err(|e| e.to_string())?;
            let s = c.summary;
            if s.root != want.root
                || s.rows != want.rows
                || s.key_min != want.key_min
                || s.key_max != want.key_max
            {
                return Err("the stored shard does not match this dataset's entry".into());
            }
            let base = bytes.as_ptr() as usize;
            let offsets = c
                .rows
                .iter()
                .map(|r| {
                    let a = r.as_ptr() as usize - base;
                    (a, a + r.len())
                })
                .collect();
            let (keys, tree) = (c.keys, c.tree);
            Ok(Loaded {
                bytes,
                offsets,
                keys,
                tree,
            })
        })
        .await
        .map_err(|_| internal("load task"))?
        .map_err(|e| shard_missing(format!("the shard cannot be served for this dataset: {e}")))?;
        let l = Arc::new(loaded);
        if let Ok(mut c) = self.loaded.lock() {
            c.retain(|(k, _)| *k != ck);
            c.push((ck, l.clone()));
            while c.len() > LOADED_SHARDS {
                c.remove(0);
            }
        }
        if let Ok(mut t) = self.touched.lock() {
            t.insert(*root, (self.now)());
        }
        Ok(l)
    }

    fn cached(&self, ck: &LoadKey) -> Option<Arc<Loaded>> {
        let c = self.loaded.lock().ok()?;
        c.iter().find(|(k, _)| k == ck).map(|(_, l)| l.clone())
    }

    fn mark(&self, root: Hash32, held: bool) {
        if let Ok(mut c) = self.rechecked.lock() {
            c.insert(root, ((self.now)(), held));
        }
    }

    /// The version to serve: the newest whose anchor still holds, each read
    /// again at most [`ANCHOR_RECHECK_SECS`] (host clock) after the last read.
    /// A version the book no longer holds (its key compromised from before it,
    /// or its fact disputed) is skipped, so the honest earlier versions keep
    /// serving (review CL5-R4); none holding is a refusal.
    async fn serving(&self, key: &Key) -> Result<Held, ApiError> {
        let versions: Vec<(Hash32, SignedReport, [u8; 20])> = {
            let ds = self.datasets.read().map_err(|_| internal("lock"))?;
            let d = ds.get(key).ok_or_else(unknown_dataset)?;
            if d.forked.is_some() {
                return Err(forked(d));
            }
            d.versions
                .iter()
                .rev()
                .map(|h| (h.manifest.header.root(), h.report.clone(), h.signer))
                .collect()
        };
        let mut first_refusal = None;
        for (root, report, signer) in versions {
            let now = (self.now)();
            let last = self
                .rechecked
                .lock()
                .ok()
                .and_then(|c| c.get(&root).copied());
            let held = match last {
                Some((t, held)) if now.saturating_sub(t) < ANCHOR_RECHECK_SECS => held,
                _ => match self.anchor_of(&report, &signer).await? {
                    AnchorState::Anchored(a) => {
                        let mut ds = self.datasets.write().map_err(|_| internal("lock"))?;
                        if let Some(h) = ds.get_mut(key).and_then(|d| {
                            d.versions
                                .iter_mut()
                                .find(|h| h.manifest.header.root() == root)
                        }) {
                            h.anchor = a;
                        }
                        self.mark(root, true);
                        true
                    }
                    AnchorState::Not(why) => {
                        first_refusal.get_or_insert(why);
                        self.mark(root, false);
                        false
                    }
                },
            };
            if held {
                let ds = self.datasets.read().map_err(|_| internal("lock"))?;
                if let Some(h) = ds
                    .get(key)
                    .and_then(|d| d.versions.iter().find(|h| h.manifest.header.root() == root))
                {
                    return Ok(Held {
                        manifest: h.manifest.clone(),
                        report: h.report.clone(),
                        signer: h.signer,
                        anchor: h.anchor.clone(),
                    });
                }
            }
        }
        Err(ApiError::new(
            StatusCode::CONFLICT,
            "bulk_not_servable",
            "no version's signed report holds in the signed book any more (key compromised or suspended, or the fact disputed)",
        )
        .with_field("reason", json!(first_refusal.unwrap_or(reason::NOT_ACCEPTED))))
    }

    /// `GET /v1/bulk`.
    pub fn list(&self) -> Result<Value, ApiError> {
        let ds = self.datasets.read().map_err(|_| internal("lock"))?;
        let items: Vec<Value> = ds
            .iter()
            .filter_map(|((p, d), x)| {
                let h = x.latest()?;
                let hd = &h.manifest.header;
                let stored = h
                    .manifest
                    .shards
                    .iter()
                    .filter(|e| self.tiers.tier(&e.root).is_some())
                    .count();
                Some(json!({
                    "publisher": p, "dataset": d, "version": hd.version,
                    "root": hd.root().to_string(), "rows": hd.row_count, "bytes": hd.byte_count,
                    "shards": hd.shard_count, "stored_shards": stored,
                    "key_min": hd.key_min.to_string(), "key_max": hd.key_max.to_string(),
                    "license": hd.rights.license, "forked": x.forked.is_some(),
                }))
            })
            .collect();
        Ok(json!({"datasets": items}))
    }

    /// `GET /v1/bulk/{publisher}/{dataset}`.
    pub fn info(&self, publisher: &str, dataset: &str) -> Result<Value, ApiError> {
        let ds = self.datasets.read().map_err(|_| internal("lock"))?;
        let d = ds
            .get(&(publisher.to_string(), dataset.to_string()))
            .ok_or_else(unknown_dataset)?;
        if d.forked.is_some() {
            return Err(forked(d));
        }
        let h = d.latest().ok_or_else(unknown_dataset)?;
        let tree = shards_tree(&h.manifest.shards);
        let shards: Vec<Value> = h
            .manifest
            .shards
            .iter()
            .map(|e| {
                let mut v = shard_json(
                    e,
                    &tree
                        .proof(u64::from(e.index))
                        .unwrap_or(onyx_merkle::Proof {
                            index: 0,
                            leaf_count: 0,
                            siblings: vec![],
                        }),
                );
                v["tier"] = json!(self
                    .tiers
                    .tier(&e.root)
                    .map(|t| format!("{t:?}").to_lowercase()));
                v
            })
            .collect();
        let versions: Vec<Value> = d
            .versions
            .iter()
            .map(|v| json!({"version": v.manifest.header.version, "root": v.manifest.header.root().to_string(), "digest": v.anchor.digest.to_string()}))
            .collect();
        Ok(json!({
            "publisher": publisher,
            "dataset": dataset,
            "manifest": header_json(&h.manifest),
            "anchor": h.anchor.to_json(&h.report),
            "versions": versions,
            "shards": shards,
        }))
    }

    /// `GET /v1/bulk/{publisher}/{dataset}/rows`.
    pub async fn rows(
        &self,
        publisher: &str,
        dataset: &str,
        q: &RowsQuery,
    ) -> Result<Value, ApiError> {
        let key = (publisher.to_string(), dataset.to_string());
        let Held {
            manifest,
            report,
            anchor,
            ..
        } = self.serving(&key).await?;
        let from = q.from.unwrap_or(i128::MIN);
        let to = q.to.unwrap_or(i128::MAX);
        let mut skip = q.page.saturating_mul(q.limit);
        let mut rows = Vec::new();
        let mut used: Vec<u32> = Vec::new();
        let mut more = false;
        let candidates: Vec<&ShardEntry> = manifest
            .shards
            .iter()
            .filter(|e| e.key_max >= from && e.key_min <= to)
            .collect();
        for e in candidates {
            if rows.len() as u64 >= q.limit {
                more = true;
                break;
            }
            let inside = e.key_min >= from && e.key_max <= to;
            if inside && skip >= e.rows {
                skip -= e.rows;
                continue;
            }
            let l = self.load(e, &manifest.header.schema).await?;
            let lo = l.keys.partition_point(|k| *k < from) as u64;
            let hi = l.keys.partition_point(|k| *k <= to) as u64;
            let count = hi.saturating_sub(lo);
            if skip >= count {
                skip -= count;
                continue;
            }
            let start = lo + skip;
            skip = 0;
            let take = (q.limit - rows.len() as u64).min(hi - start);
            for i in start..start + take {
                let p = l.tree.proof(i).ok_or_else(|| internal("proof"))?;
                rows.push(row_json(&manifest.header, e.index, l.row(i as usize), &p));
            }
            used.push(e.index);
            if start + take < hi {
                more = true;
                break;
            }
        }
        let tree = shards_tree(&manifest.shards);
        let shards: Vec<Value> = used
            .iter()
            .filter_map(|i| {
                let e = manifest.shards.get(*i as usize)?;
                Some(shard_json(e, &tree.proof(u64::from(*i))?))
            })
            .collect();
        Ok(json!({
            "publisher": publisher,
            "dataset": dataset,
            "manifest": header_json(&manifest),
            "anchor": anchor.to_json(&report),
            "query": {
                "from": q.from.map(|f| f.to_string()),
                "to": q.to.map(|t| t.to_string()),
                "page": q.page,
                "limit": q.limit,
            },
            "shards": shards,
            "rows": rows,
            "next_page": more.then_some(q.page + 1),
            "proof": "each row: Merkle proof to its shard root; each shard: proof to the manifest's shards_root; the manifest root: signed by the publisher (EIP-191, ADR-0028) and recorded in OnyxSignedBook at the finalized block named in anchor.checked_at",
        }))
    }
}

fn header_json(m: &Manifest) -> Value {
    let h = &m.header;
    json!({
        "root": h.root().to_string(),
        "header": hex0x(&borsh::to_vec(h).unwrap_or_default()),
        "version": h.version,
        "prev": h.prev.to_string(),
        "created_unix": h.created_unix,
        "schema": h.schema.to_json(),
        "rights": {
            "license": h.rights.license,
            "attribution": h.rights.attribution,
            "statement": h.rights_statement,
        },
        "shard_count": h.shard_count,
        "row_count": h.row_count,
        "byte_count": h.byte_count,
        "key_min": h.key_min.to_string(),
        "key_max": h.key_max.to_string(),
        "shards_root": h.shards_root.to_string(),
    })
}

fn anchor_to_bytes(a: &AnchorView) -> Vec<u8> {
    borsh::to_vec(&(
        a.chain_id,
        a.book,
        a.digest,
        a.fact,
        a.report_hash,
        a.key,
        a.signer,
        a.block_number,
        a.block_hash,
        a.recorded_at,
        a.accepted_at,
    ))
    .unwrap_or_default()
}

type AnchorTuple = (
    u64,
    [u8; 20],
    Hash32,
    Hash32,
    Hash32,
    Hash32,
    [u8; 20],
    u64,
    Hash32,
    u64,
    u64,
);

fn anchor_from_bytes(b: &[u8]) -> Option<AnchorView> {
    let t = AnchorTuple::try_from_slice(b).ok()?;
    Some(AnchorView {
        chain_id: t.0,
        book: t.1,
        digest: t.2,
        fact: t.3,
        report_hash: t.4,
        key: t.5,
        signer: t.6,
        block_number: t.7,
        block_hash: t.8,
        recorded_at: t.9,
        accepted_at: t.10,
    })
}

fn unknown_dataset() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "bulk_unknown_dataset",
        "no such dataset on this node",
    )
}

fn forked(d: &Dataset) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "bulk_dataset_forked",
        format!(
            "not served: {}",
            d.forked
                .as_deref()
                .unwrap_or("two anchored manifests for one version")
        ),
    )
}

fn too_large(e: &ShardEntry) -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "bulk_shard_size",
        format!("the manifest says this shard is exactly {} bytes", e.bytes),
    )
}

fn not_anchored(why: &'static str) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "bulk_not_anchored",
        "the signed report is not recorded and accepted in the signed book at the finalized block, or its key is unknown or suspended",
    )
    .with_field("reason", json!(why))
    .with_header("retry-after", "60".into())
}

fn shard_missing(why: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "bulk_shard_missing", why)
}

fn mismatch(why: String) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "bulk_shard_mismatch", why)
}

/// A parsed `rows` query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowsQuery {
    /// Smallest key (inclusive).
    pub from: Option<i128>,
    /// Largest key (inclusive).
    pub to: Option<i128>,
    /// 0-based page.
    pub page: u64,
    /// Rows per page, 1..=[`MAX_PAGE_ROWS`].
    pub limit: u64,
}

/// Parse `from`, `to` (decimal integers), `page` and `limit`.
pub fn parse_rows_query(q: Option<&str>) -> Result<RowsQuery, ApiError> {
    let mut out = RowsQuery {
        from: None,
        to: None,
        page: 0,
        limit: DEFAULT_PAGE_ROWS,
    };
    let mut seen: Vec<&str> = Vec::new();
    for pair in q.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if seen.contains(&k) {
            return Err(bad_request(format!("{k} given twice")));
        }
        seen.push(k);
        let int = |what: &str| {
            onyx_bulk::row::parse_int(v)
                .map_err(|_| bad_request(format!("{what} must be a whole number")))
        };
        match k {
            "from" => out.from = Some(int("from")?),
            "to" => out.to = Some(int("to")?),
            "page" => {
                out.page = v
                    .parse::<u64>()
                    .ok()
                    .filter(|p| *p <= 1 << 32)
                    .ok_or_else(|| bad_request("page must be 0..=2^32"))?
            }
            "limit" => {
                out.limit = v
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (1..=MAX_PAGE_ROWS).contains(n))
                    .ok_or_else(|| bad_request(format!("limit must be 1..={MAX_PAGE_ROWS}")))?
            }
            _ => return Err(bad_request(format!("unknown query parameter {k}"))),
        }
    }
    if out.from.zip(out.to).is_some_and(|(f, t)| f > t) {
        return Err(bad_request("from must not be above to"));
    }
    Ok(out)
}

/// The free read routes, to merge into [`crate::router_with`]'s `extra`.
pub fn routes(store: Arc<BulkStore>) -> Router {
    Router::new()
        .route("/v1/bulk", get(list))
        .route("/v1/bulk/{publisher}/{dataset}", get(info))
        .route("/v1/bulk/{publisher}/{dataset}/rows", get(rows))
        .with_state(store)
}

/// The upload routes, merged OUTSIDE the server guard (own limits).
pub fn upload_routes(store: Arc<BulkStore>) -> Router {
    Router::new()
        .route("/v1/bulk/manifests", post(post_manifest))
        .route(
            "/v1/bulk/{publisher}/{dataset}/shards/{root}",
            put(put_shard),
        )
        .with_state(store)
}

fn no_store(status: StatusCode, v: Value) -> Response {
    let mut r = (status, Json(v)).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn limited(store: &BulkStore, client: Option<Extension<ClientKey>>) -> Result<(), ApiError> {
    let client = client.map_or_else(|| "local".to_string(), |Extension(c)| c.0);
    let scopes: Vec<(String, u32)> = rate_scopes(&client, store.free_per_minute)
        .into_iter()
        .map(|(k, n)| (format!("bulk:{k}"), n))
        .collect();
    if store.rate.take_attempts(&scopes) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many bulk answers from this address",
        )
        .with_header("retry-after", "1".into()))
    }
}

fn path_ids(
    p: Result<Path<(String, String)>, PathRejection>,
) -> Result<(String, String), ApiError> {
    let Path((a, b)) = p.map_err(|e| bad_request(e.body_text()))?;
    if !onyx_core::ids::is_dotted_id(&a) || !onyx_core::ids::is_dotted_id(&b) {
        return Err(bad_request("publisher and dataset are dotted ids"));
    }
    Ok((a, b))
}

async fn list(
    State(store): State<Arc<BulkStore>>,
    client: Option<Extension<ClientKey>>,
    RawQuery(q): RawQuery,
) -> Response {
    let r = (|| {
        limited(&store, client)?;
        if q.is_some_and(|q| !q.is_empty()) {
            return Err(bad_request("GET /v1/bulk takes no query parameters"));
        }
        store.list()
    })();
    match r {
        Ok(v) => no_store(StatusCode::OK, v),
        Err(e) => e.into_response(),
    }
}

async fn info(
    State(store): State<Arc<BulkStore>>,
    client: Option<Extension<ClientKey>>,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Response {
    let r = (|| {
        limited(&store, client)?;
        let (p, d) = path_ids(path)?;
        store.info(&p, &d)
    })();
    match r {
        Ok(v) => no_store(StatusCode::OK, v),
        Err(e) => e.into_response(),
    }
}

async fn rows(
    State(store): State<Arc<BulkStore>>,
    client: Option<Extension<ClientKey>>,
    path: Result<Path<(String, String)>, PathRejection>,
    RawQuery(q): RawQuery,
) -> Response {
    let r = async {
        limited(&store, client)?;
        let (p, d) = path_ids(path)?;
        let q = parse_rows_query(q.as_deref())?;
        store.rows(&p, &d, &q).await
    }
    .await;
    match r {
        Ok(v) => no_store(StatusCode::OK, v),
        Err(e) => e.into_response(),
    }
}

async fn upload<F, Fut>(store: &BulkStore, f: F) -> Response
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(StatusCode, Value), ApiError>>,
{
    let Ok(_permit) = store.uploads.try_acquire() else {
        return ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "overloaded",
            "uploads in progress; try again shortly",
        )
        .with_header("retry-after", "5".into())
        .into_response();
    };
    match tokio::time::timeout(UPLOAD_TIMEOUT, f()).await {
        Ok(Ok((s, v))) => no_store(s, v),
        Ok(Err(e)) => e.into_response(),
        Err(_) => ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            "the upload took too long",
        )
        .into_response(),
    }
}

async fn post_manifest(State(store): State<Arc<BulkStore>>, body: Body) -> Response {
    let s = store.clone();
    upload(&store, || async move {
        let b = axum::body::to_bytes(body, MAX_MANIFEST_BODY)
            .await
            .map_err(|_| {
                ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "body_too_large",
                    "manifest body over its cap",
                )
            })?;
        let v: Value =
            serde_json::from_slice(&b).map_err(|_| bad_request("body: JSON {manifest, report}"))?;
        let m = onyx_bulk::wire::hex_field(&v["manifest"], "manifest")
            .map_err(|e| bad_request(e.to_string()))?;
        let r = onyx_bulk::wire::hex_field(&v["report"], "report")
            .map_err(|e| bad_request(e.to_string()))?;
        s.accept_manifest(&m, &r).await
    })
    .await
}

async fn put_shard(
    State(store): State<Arc<BulkStore>>,
    path: Result<Path<(String, String, String)>, PathRejection>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let s = store.clone();
    upload(&store, || async move {
        let Path((p, d, root)) = path.map_err(|e| bad_request(e.body_text()))?;
        if !onyx_core::ids::is_dotted_id(&p) || !onyx_core::ids::is_dotted_id(&d) {
            return Err(bad_request("publisher and dataset are dotted ids"));
        }
        let (root, canonical) = crate::parse_hash(&root, "root")?;
        if !canonical {
            return Err(bad_request("root must be 0x and 64 lowercase hex digits"));
        }
        s.accept_shard(&p, &d, &root, &headers, body).await
    })
    .await
}

#[cfg(test)]
#[path = "bulk_serve_tests.rs"]
mod tests;
