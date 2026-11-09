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
//! - Catalogs (ADR-0059 amendment 1): `POST /v1/bulk/catalogs` takes a
//!   catalog (one signed report for the newest manifest of each of a
//!   publisher's datasets) under the same anchor rule, after its predecessor
//!   and only as its append-only successor; `POST /v1/bulk/manifests` with
//!   `{manifest, catalog: n}` then takes a manifest that catalog n lists, as if
//!   it had its own report. `GET /v1/bulk/catalogs/{publisher}/{n}` serves a
//!   held catalog.
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
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_bulk::catalog::{self as cat, Catalog, CatalogHeader, CatalogLeaf, LeafProof};
use onyx_bulk::manifest::{shards_tree, Manifest, ShardEntry};
use onyx_bulk::shard::check_shard;
use onyx_bulk::wire::{catalog_json, hex0x, row_json, shard_json};
use onyx_coordinator::cas::{Cas, FsCas, Kind};
use onyx_coordinator::tiers::{Tier, TieredStore};
use onyx_core::{keccak256, Hash32, SignedReport};
use onyx_merkle::MerkleTree;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// Largest `POST /v1/bulk/catalogs` body (hex doubles the Borsh catalog).
pub const MAX_CATALOG_BODY: usize = 2 * cat::MAX_CATALOG_BYTES + 64 * 1024;
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
/// Publisher locks (two publishers may share one: they then wait for each
/// other, never for longer than one upload).
const PUBLISHER_LOCKS: usize = 64;
/// Reports remembered as withdrawn for good (each one cost its poster a
/// report the book recorded, then a reviewer's withdrawal).
const MAX_WITHDRAWN: usize = 100_000;
/// Catalogs stored whole every this many (ADR-0059 amendment 2 draft 4): a
/// rebuild applies at most this many minus one diffs.
pub const CATALOG_CHECKPOINT: u64 = 32;
/// Old-catalog rebuilds at once (`GET /v1/bulk/catalogs/...`).
const MAX_REBUILDS: usize = 2;
/// Catalog file reads at once (`GET /v1/bulk/catalogs/{publisher}/files`,
/// at most `MAX_FILES_BYTES` each; ADR-0059 amendment 3, review CAT-HIST-R1
/// finding 6: apart from the rebuilds uploads need).
const MAX_FILE_READS: usize = 2;
/// Bytes of encoded catalog and manifest answers kept (THREATS
/// SERVE-WRITE-2): every reader of one catalog or manifest shares one copy of
/// its JSON (about 2 MB for a catalog of 9,000 leaves) instead of a fresh one
/// each, so readers that drip cannot multiply it; the oldest used goes first.
/// An answer still being sent stays shared after it leaves the kept set
/// (review SERVE-WRITE-R3 finding 2).
pub const ANSWER_BYTES: usize = 32 << 20;
/// A publisher's catalog fork record (THREATS CAT-6).
const FORK_FILE: &str = "fork.bin";

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
    /// A catalog whose number is a multiple of this is stored whole; any
    /// other as the leaves that differ from the one before (ADR-0059
    /// amendment 2 draft 4).
    pub catalog_checkpoint: u64,
}

impl Default for BulkLimits {
    fn default() -> Self {
        BulkLimits {
            quota_bytes: 16 << 30,
            max_datasets: 64,
            max_total_bytes: 256 << 30,
            warm_after_secs: 7 * 86_400,
            catalog_checkpoint: CATALOG_CHECKPOINT,
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

pub(crate) fn word(b: &[u8], i: usize) -> Option<[u8; 32]> {
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

pub(crate) fn hex_of(v: &Value) -> Result<Vec<u8>, String> {
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

pub(crate) async fn call_at(
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
    let at = decision_block(rpc, chain_id).await?;
    Ok(read_anchor_at(rpc, chain_id, book, report, signer, at)
        .await?
        .state)
}

/// A finalized block, number and hash: every read of one decision is made at
/// one block (review CAT-FIX-R1 M1).
pub type At = (u64, Hash32);

/// The chain's finalized block, after checking the chain is `chain_id`.
pub async fn decision_block(rpc: &dyn JsonRpc, chain_id: u64) -> Result<At, String> {
    let id = qty(&rpc.call("eth_chainId", json!([])).await?)?;
    if id != chain_id {
        return Err(format!("the node's chain is {id}, not {chain_id}"));
    }
    finalized(rpc).await
}

/// What the book says of a report at one block, and whether a refusal is
/// final (THREATS CAT-5): another signature over its payload, or its fact
/// holding no value (a key retirement or compromise withdrew it, or the
/// reviewer cleared a dispute) or another report's. A suspended key, an open
/// dispute and a record not seen yet can each change back: not final.
pub struct Reading {
    /// The anchor, or why not.
    pub state: AnchorState,
    /// The refusal is the book's final word on this report.
    pub withdrawn: bool,
}

/// [`read_anchor`] at block `at` (from [`decision_block`]).
pub async fn read_anchor_at(
    rpc: &dyn JsonRpc,
    chain_id: u64,
    book: &[u8; 20],
    report: &SignedReport,
    signer: &[u8; 20],
    (block_number, block_hash): At,
) -> Result<Reading, String> {
    let not = |why: &'static str, withdrawn: bool| Reading {
        state: AnchorState::Not(why),
        withdrawn,
    };
    let digest = report.payload.digest();
    let bad = || "the signed book's answer does not decode".to_string();
    let r = call_at(rpc, book, "report(bytes32)", &digest, &block_hash).await?;
    if r.is_empty() || word(&r, 0).ok_or_else(bad)? == [0u8; 32] {
        return Ok(not(reason::NOT_FINAL, false));
    }
    let fact = Hash32(word(&r, 1).ok_or_else(bad)?);
    let report_hash = Hash32(word(&r, 3).ok_or_else(bad)?);
    let recorded_at = word_u64(&word(&r, 6).ok_or_else(bad)?).ok_or_else(bad)?;
    if report_hash != report.hash() {
        return Ok(not(reason::OTHER_SIGNATURE, true));
    }
    let f = call_at(rpc, book, "factView(bytes32)", &fact, &block_hash).await?;
    let state = word_u64(&word(&f, 0).ok_or_else(bad)?).ok_or_else(bad)?;
    let accepted = Hash32(word(&f, 1).ok_or_else(bad)?);
    let accepted_at = word_u64(&word(&f, 2).ok_or_else(bad)?).ok_or_else(bad)?;
    if state != 1 || accepted != report_hash {
        // `FactState`: 0 none (withdrawn, or a dispute cleared), 1 accepted
        // (here on another report), 2 disputed: open, unless the report's
        // key is compromised. The book never lets a compromised key's
        // report be kept (`resolveFact` and `postReport` need `_permits`),
        // and an equivocation, the likeliest way a theft shows, leaves the
        // fact disputed for good (review of CAT-5, M1).
        let withdrawn = state != 2 || {
            let k = call_at(
                rpc,
                book,
                "keyState(bytes32)",
                &report.payload.key,
                &block_hash,
            )
            .await?;
            word_u64(&word(&k, 0).ok_or_else(bad)?).ok_or_else(bad)? == 3
        };
        return Ok(not(reason::NOT_ACCEPTED, withdrawn));
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
        return Ok(not(reason::KEY, false));
    }
    Ok(Reading {
        state: AnchorState::Anchored(AnchorView {
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
        }),
        withdrawn: false,
    })
}

/// The digest `book` holds under `(key, sequence)` (`sequenceOf`) at the
/// finalized block of `rpc`'s chain; zero when none (ADR-0059 amendment 1
/// item 4: a catalog-anchored version must have no other report under its
/// own sequence).
pub async fn read_sequence(
    rpc: &dyn JsonRpc,
    book: &[u8; 20],
    key: &Hash32,
    sequence: u64,
) -> Result<Hash32, String> {
    read_sequence_at(rpc, book, key, sequence, finalized(rpc).await?).await
}

/// [`read_sequence`] at block `at` (a history check reads everything at one
/// block; ADR-0059 amendment 3, review CAT-HIST-R1 finding 5).
pub async fn read_sequence_at(
    rpc: &dyn JsonRpc,
    book: &[u8; 20],
    key: &Hash32,
    sequence: u64,
    (_, at): At,
) -> Result<Hash32, String> {
    let mut d = selector("sequenceOf(bytes32,uint64)").to_vec();
    d.extend_from_slice(&key.0);
    d.extend_from_slice(&[0u8; 24]);
    d.extend_from_slice(&sequence.to_be_bytes());
    let r = rpc
        .call(
            "eth_call",
            json!([{"to": hex0x(book), "data": hex0x(&d)}, {"blockHash": at.to_string()}]),
        )
        .await?;
    let b = hex_of(&r)?;
    // An empty answer (no contract there) is no answer, never "no report"
    // (review of the CAT-1 build, finding 9).
    Ok(Hash32(
        word(&b, 0).ok_or("the signed book's sequenceOf answer is empty")?,
    ))
}

/// A report the book recorded (`report(digest)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recorded {
    /// Its key.
    pub key: Hash32,
    /// Its fact.
    pub fact: Hash32,
    /// `keccak256(borsh(value))`.
    pub value_hash: Hash32,
    /// `SignedReport::hash`.
    pub report_hash: Hash32,
    /// Its observation time.
    pub observed: u64,
}

/// Where a recorded digest stands at one block (ADR-0059 amendment 3 item 3,
/// the CAT-5 rule; review CAT-HIST-R1 finding 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    /// Nothing recorded under the digest.
    Unrecorded,
    /// Accepted, its key registered and not suspended.
    Holds(Recorded),
    /// The book's final word withdrew it: its fact holds no value or another
    /// report, or is disputed with the key compromised.
    Withdrawn(Recorded),
    /// Can still change: an open dispute, a suspended key.
    Open(Recorded, &'static str),
}

/// [`Standing`] of `digest` in `book` at block `at`.
pub async fn read_digest_at(
    rpc: &dyn JsonRpc,
    book: &[u8; 20],
    digest: &Hash32,
    (_, block_hash): At,
) -> Result<Standing, String> {
    let bad = || "the signed book's answer does not decode".to_string();
    let r = call_at(rpc, book, "report(bytes32)", digest, &block_hash).await?;
    if r.is_empty() || word(&r, 0).ok_or_else(bad)? == [0u8; 32] {
        return Ok(Standing::Unrecorded);
    }
    let rec = Recorded {
        key: Hash32(word(&r, 0).ok_or_else(bad)?),
        fact: Hash32(word(&r, 1).ok_or_else(bad)?),
        value_hash: Hash32(word(&r, 2).ok_or_else(bad)?),
        report_hash: Hash32(word(&r, 3).ok_or_else(bad)?),
        observed: word_u64(&word(&r, 4).ok_or_else(bad)?).ok_or_else(bad)?,
    };
    let f = call_at(rpc, book, "factView(bytes32)", &rec.fact, &block_hash).await?;
    let state = word_u64(&word(&f, 0).ok_or_else(bad)?).ok_or_else(bad)?;
    let accepted = Hash32(word(&f, 1).ok_or_else(bad)?);
    let k = call_at(rpc, book, "keyState(bytes32)", &rec.key, &block_hash).await?;
    let life = word_u64(&word(&k, 0).ok_or_else(bad)?).ok_or_else(bad)?;
    let suspended = word_u64(&word(&k, 1).ok_or_else(bad)?).ok_or_else(bad)?;
    if state != 1 || accepted != rec.report_hash {
        // As `read_anchor_at`: no value or another report is final; a dispute
        // is final only with the key compromised.
        return Ok(if state != 2 || life == 3 {
            Standing::Withdrawn(rec)
        } else {
            Standing::Open(rec, "its fact is disputed")
        });
    }
    if life == 0 {
        return Ok(Standing::Open(rec, "its key is not registered"));
    }
    if suspended != 0 {
        return Ok(Standing::Open(rec, "its key is suspended"));
    }
    Ok(Standing::Holds(rec))
}

/// One held version. `report` and `anchor` are the manifest's own, or, with
/// `catalog`, those of the catalog that lists it (ADR-0059 amendment 1).
struct Held {
    manifest: Manifest,
    report: SignedReport,
    signer: [u8; 20],
    anchor: AnchorView,
    catalog: Option<Box<(CatalogHeader, LeafProof)>>,
}

/// Where an uploaded catalog fits ([`BulkStore::fit_catalog`]).
// One per upload, never stored: a `Box` buys nothing.
#[allow(clippy::large_enum_variant)]
enum Fit {
    /// Held already (its status).
    Same(Value),
    /// The next one: taken once its anchor reads.
    Next,
    /// Its number is held by another catalog: the refusal, unless the held
    /// ones are withdrawn (THREATS CAT-5).
    Taken(ApiError),
    /// Not the append-only successor of the newest held catalog: the
    /// refusal, or a fork when both are anchored (THREATS CAT-6).
    Broken(ApiError),
}

/// Two anchored catalogs of one publisher that cannot both stand (THREATS
/// CAT-6, ADR-0059 amendment 1 item 3): the same number with two roots, or
/// a catalog that is not the append-only successor of the one before. Kept
/// as evidence in `catalogs/<publisher>/fork.bin`; while it stands, the
/// publisher's catalog datasets are not served and its uploads are refused.
/// It clears by itself when the book withdraws either side for good.
#[derive(Clone, Debug, PartialEq, BorshSerialize, BorshDeserialize)]
struct Fork {
    /// The held catalog's header and report.
    held: (CatalogHeader, SignedReport),
    /// The other catalog's header and report.
    other: (CatalogHeader, SignedReport),
    why: String,
}

/// A catalog stored as the leaves that differ from the one before
/// (`{n}.diff`, ADR-0059 amendment 2 draft 4): a version moved or a
/// dataset added, which is all item 3 lets change.
#[derive(BorshSerialize, BorshDeserialize)]
struct CatalogDiff {
    header: CatalogHeader,
    /// The signed report (Borsh).
    report: Vec<u8>,
    /// The anchor read when it was taken.
    anchor: Vec<u8>,
    /// Leaves that differ from the catalog before, in dataset order.
    leaves: Vec<CatalogLeaf>,
}

/// One stored catalog: whole (the catalog's Borsh) or a diff.
enum Stored {
    Whole(Vec<u8>),
    Diff(Vec<CatalogLeaf>),
}

/// Stored catalog `n` in a publisher's folder: its header, its body as
/// stored, its report and anchor bytes, and its size on disk. `None` when
/// there is neither `{n}.bin` nor `{n}.diff`.
type StoredCatalog = (CatalogHeader, Stored, Vec<u8>, Vec<u8>, u64);

fn read_stored(dir: &FsPath, n: u64) -> Result<Option<StoredCatalog>, String> {
    let bin = dir.join(format!("{n}.bin"));
    let whole = match std::fs::read(&bin) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", bin.display())),
    };
    if let Some(bytes) = whole {
        if dir.join(format!("{n}.diff")).exists() {
            // Never both (a rollback past the first diff, or a hand edit):
            // which one holds is not decided here (review of the build, L1).
            return Err(format!("{}: both {n}.bin and {n}.diff", dir.display()));
        }
        let size = bytes.len() as u64;
        let (c, r, a): (Vec<u8>, Vec<u8>, Vec<u8>) = BorshDeserialize::try_from_slice(&bytes)
            .map_err(|_| format!("{}: does not decode", bin.display()))?;
        let header = CatalogHeader::deserialize(&mut c.as_slice())
            .map_err(|_| format!("{}: catalog header does not decode", bin.display()))?;
        return Ok(Some((header, Stored::Whole(c), r, a, size)));
    }
    let diff = dir.join(format!("{n}.diff"));
    match std::fs::read(&diff) {
        Ok(bytes) => {
            let size = bytes.len() as u64;
            let d = CatalogDiff::try_from_slice(&bytes)
                .map_err(|_| format!("{}: does not decode", diff.display()))?;
            Ok(Some((
                d.header,
                Stored::Diff(d.leaves),
                d.report,
                d.anchor,
                size,
            )))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", diff.display())),
    }
}

/// The leaves of `c` that differ from `before` (a version moved or a
/// dataset added).
fn diff_of(before: &Catalog, c: &Catalog) -> Vec<CatalogLeaf> {
    c.leaves
        .iter()
        .filter(|l| before.find(&l.dataset).map(|i| &before.leaves[i]) != Some(*l))
        .cloned()
        .collect()
}

/// Catalog `n` in a publisher's folder, rebuilt from the nearest whole file
/// at or below it, or from `start` (a checked catalog) when that is nearer,
/// with every diff after it applied; each step is validated (leaf count,
/// leaves root, order) and must link to the one before (review CAT-DIFF-R1
/// M3). At most `CATALOG_CHECKPOINT - 1` diffs.
fn rebuild_catalog(dir: &FsPath, n: u64, start: Option<&Catalog>) -> Result<Catalog, String> {
    let mut k = n;
    let mut cur = loop {
        if let Some(s) = start.filter(|s| s.header.number == k) {
            break s.clone();
        }
        if let Some((header, Stored::Whole(c), ..)) = read_stored(dir, k)? {
            let f = dir.join(format!("{k}.bin"));
            let c = Catalog::try_from_slice(&c)
                .map_err(|_| format!("{}: catalog does not decode", f.display()))?;
            c.validate().map_err(|e| format!("{}: {e}", f.display()))?;
            if c.header != header {
                return Err(format!("{}: header and catalog differ", f.display()));
            }
            break c;
        }
        k = k
            .checked_sub(1)
            .ok_or_else(|| format!("{}: no whole catalog at or below {n}", dir.display()))?;
    };
    for m in k + 1..=n {
        let (header, stored, ..) = read_stored(dir, m)?
            .ok_or_else(|| format!("{}: catalog {m} missing", dir.display()))?;
        let c = match stored {
            Stored::Whole(c) => Catalog::try_from_slice(&c)
                .map_err(|_| format!("{}: catalog {m} does not decode", dir.display()))?,
            Stored::Diff(changed) => Catalog {
                leaves: cat::apply_diff(&cur.leaves, &changed),
                header,
            },
        };
        // Every step: its leaf count and its link to the one before; the
        // whole check (leaves root, order) on the catalog returned, which
        // binds every leaf it has (review of the build, M1: about 30 times
        // less hashing than checking each step).
        if m == n {
            c.validate()
                .map_err(|e| format!("{}: catalog {m}: {e}", dir.display()))?;
        } else if c.leaves.len() as u64 != u64::from(c.header.leaf_count) {
            return Err(format!("{}: catalog {m}: leaf count", dir.display()));
        }
        if c.header.prev != cur.header.root() {
            return Err(format!(
                "{}: catalog {m} does not follow {}",
                dir.display(),
                m - 1
            ));
        }
        cur = c;
    }
    Ok(cur)
}

/// What [`BulkStore::set_aside_withdrawn`] decided.
// One per upload, never stored: a `Box` buys nothing.
#[allow(clippy::large_enum_variant)]
enum Settle {
    /// The withdrawn catalogs are set aside; the new catalog's anchor.
    Done(AnchorView),
    /// The held catalog with that number has the same root and still holds.
    Same,
    /// The held catalog with that number still holds, with another root.
    Fork(Fork),
}

/// One dataset's part of a set-aside ([`BulkStore::plan_cut`]).
struct Cut {
    dataset: String,
    /// Index of the first version that goes.
    from: usize,
    /// (version, root) from `from` on.
    versions: Vec<(u32, Hash32)>,
    /// The version at `from` when the new catalog lists it unchanged: it
    /// stays, anchored by the new catalog.
    keep: Option<u32>,
}

/// One held catalog.
struct HeldCatalog {
    header: CatalogHeader,
    /// The whole catalog with its leaves' tree: kept only for the newest
    /// catalog of a publisher (the next successor check needs it). An older
    /// one is read from its file when asked (review SCALE-3: a catalog a day
    /// of about 9,000 leaves held every body, about 1 MB a day).
    body: Option<Arc<CatalogBody>>,
    report: SignedReport,
    signer: [u8; 20],
    anchor: AnchorView,
}

/// An encoded answer, shared by every reader of it.
type Answer = Arc<axum::body::Bytes>;

/// The encoded catalog and manifest answers (`catalog/{publisher}/{n}`,
/// `manifest/{root}`), each valid only while the held object has the root
/// and report hash it was made for (a set-aside or a replaced report makes
/// it stale; review SERVE-WRITE-R3 finding 6).
#[derive(Default)]
struct AnswerBook {
    /// Every answer still alive, kept or still being sent: one copy an
    /// object, however many read it (review SERVE-WRITE-R3 finding 2).
    live: std::collections::HashMap<String, ((Hash32, Hash32), std::sync::Weak<axum::body::Bytes>)>,
    /// The kept answers by key: their use stamp and a strong reference.
    kept: std::collections::HashMap<String, (u64, Answer)>,
    /// Use order of the kept answers, oldest first.
    order: BTreeMap<u64, String>,
    kept_bytes: usize,
    stamp: u64,
    /// One build at a time a key: later askers wait and take it (review
    /// SERVE-WRITE-R3 finding 1).
    building: std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>,
    /// Answers kept as a new copy (tests count them).
    builds: u64,
}

impl AnswerBook {
    fn get(&mut self, key: &str, check: (Hash32, Hash32)) -> Option<Answer> {
        let (made_for, weak) = self.live.get(key)?;
        // Made for another check: not this answer. It is not dropped here: a
        // reader with an old check must not throw away the valid answer
        // others read (review SERVE-WRITE-R4 finding 1); `put` replaces it.
        if *made_for != check {
            return None;
        }
        let Some(a) = weak.upgrade() else {
            self.live.remove(key);
            return None;
        };
        self.touch(key, a.clone());
        Some(a)
    }

    fn forget(&mut self, key: &str) {
        self.live.remove(key);
        if let Some((stamp, a)) = self.kept.remove(key) {
            self.order.remove(&stamp);
            self.kept_bytes = self.kept_bytes.saturating_sub(a.len());
        }
    }

    /// Kept again as the newest used; the oldest used out past
    /// [`ANSWER_BYTES`].
    fn touch(&mut self, key: &str, a: Answer) {
        if a.len() > ANSWER_BYTES {
            return;
        }
        self.stamp += 1;
        let len = a.len();
        if let Some((old, prev)) = self.kept.insert(key.to_string(), (self.stamp, a)) {
            self.order.remove(&old);
            self.kept_bytes = self.kept_bytes.saturating_sub(prev.len());
        }
        self.kept_bytes += len;
        self.order.insert(self.stamp, key.to_string());
        while self.kept_bytes > ANSWER_BYTES {
            let Some((_, k)) = self.order.pop_first() else {
                break;
            };
            if let Some((_, a)) = self.kept.remove(&k) {
                self.kept_bytes = self.kept_bytes.saturating_sub(a.len());
            }
        }
    }

    /// Keep a built answer, unless one made for the same check is alive:
    /// then that copy is handed out and the new bytes dropped, so one copy an
    /// object never depends on the build gate (review SERVE-WRITE-R4).
    fn put(&mut self, key: String, check: (Hash32, Hash32), bytes: axum::body::Bytes) -> Answer {
        if let Some(a) = self.get(&key, check) {
            return a;
        }
        let a = Arc::new(bytes);
        self.builds += 1;
        self.forget(&key);
        self.live.insert(key.clone(), (check, Arc::downgrade(&a)));
        self.touch(&key, a.clone());
        if self.live.len() > 2 * self.kept.len() + 1_024 {
            self.live.retain(|_, (_, w)| w.strong_count() > 0);
        }
        a
    }
}

/// A turn to build one key's answer; the key's gate goes when it ends.
struct AnswerTurn {
    store: Arc<BulkStore>,
    key: String,
    gate: Arc<tokio::sync::Mutex<()>>,
    _held: tokio::sync::OwnedMutexGuard<()>,
}

impl Drop for AnswerTurn {
    /// The gate goes only when nobody waits on it: the book's, this turn's
    /// and its guard's references are all (review SERVE-WRITE-R4 finding 2:
    /// a turn that ended without an answer left its waiters on a gate the
    /// next arrival no longer saw).
    fn drop(&mut self) {
        if let Ok(mut b) = self.store.answers.lock() {
            if b.building
                .get(&self.key)
                .is_some_and(|g| Arc::ptr_eq(g, &self.gate))
                && Arc::strong_count(&self.gate) <= 3
            {
                b.building.remove(&self.key);
            }
        }
    }
}

/// A catalog and its leaves' tree, built once (a leaf proof per upload
/// rebuilt the tree: about 18,000 hashes for 9,000 leaves; SCALE-3).
struct CatalogBody {
    catalog: Catalog,
    tree: MerkleTree,
    /// The datasets this catalog lists as tombstones (ADR-0059 amendment 4):
    /// removed while it is the publisher's newest held catalog.
    removed: std::collections::BTreeSet<String>,
}

impl CatalogBody {
    fn new(catalog: Catalog) -> Arc<Self> {
        let tree = cat::leaves_tree(&catalog.leaves);
        let removed = catalog
            .leaves
            .iter()
            .filter(|l| l.is_tombstone())
            .map(|l| l.dataset.clone())
            .collect();
        Arc::new(CatalogBody {
            catalog,
            tree,
            removed,
        })
    }
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
    /// Report digest -> (host time of the last chain read, whether it held):
    /// one read covers every manifest a catalog anchors.
    rechecked: Mutex<BTreeMap<Hash32, (u64, bool)>>,
    /// Held catalogs by publisher and number.
    catalogs: RwLock<BTreeMap<String, BTreeMap<u64, HeldCatalog>>>,
    /// Per publisher, the older catalog rebuilt last: uploads against one
    /// catalog come in runs, and a joining node reads 0, 1, 2, ... (one diff
    /// a step; review CAT-DIFF-R1 M1, L3).
    old_body: Mutex<BTreeMap<String, (u64, Arc<CatalogBody>)>>,
    rebuilds: Arc<tokio::sync::Semaphore>,
    /// Encoded catalog and manifest answers (SERVE-WRITE-2).
    answers: Mutex<AnswerBook>,
    file_reads: Arc<tokio::sync::Semaphore>,
    /// Bytes of stored catalogs per publisher (counted in its quota).
    catalog_bytes: Mutex<BTreeMap<String, u64>>,
    /// Keys the book did not know or had suspended, by host time: a POST
    /// under one is refused without a chain read for [`KEY_REFUSAL_SECS`].
    refused_keys: Mutex<BTreeMap<Hash32, u64>>,
    /// One lock per publisher (striped): a catalog upload holds it for
    /// writing from its first check to its last write, a manifest upload for
    /// reading (review CAT-FIX-R1 M2, M3).
    publishers: Vec<tokio::sync::RwLock<()>>,
    /// Digests of reports the book withdrew for good (THREATS CAT-5).
    withdrawn: Mutex<std::collections::BTreeSet<Hash32>>,
    /// Catalog forks by publisher (THREATS CAT-6).
    forks: RwLock<BTreeMap<String, Fork>>,
    /// When each fork was last read against the book (host clock).
    fork_read: Mutex<BTreeMap<String, u64>>,
    free_per_minute: u32,
    rate: Caches,
    /// Erasing removed datasets (ADR-0059 amendment 4 item 5a).
    erase_cfg: EraseConfig,
    /// Every manifest upload and shard write holds it to read; an erase pass
    /// holds it to write from its reference count to its last file (review
    /// CAT-ERASE-R1 H1). Always taken before a publisher lock.
    erase_lock: tokio::sync::RwLock<()>,
    erase: Mutex<erase::EraseState>,
    /// Tests: an erase stops after this many file removals (a crash).
    #[cfg(test)]
    erase_stop: Mutex<Option<usize>>,
}

fn decode_manifest(b: &[u8]) -> Result<Manifest, ApiError> {
    Manifest::from_bytes(b).map_err(|e| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bulk_bad_manifest",
            e.to_string(),
        )
    })
}

/// Store a held version: `v{n}.bin` (manifest, report, anchor) and, for a
/// catalog anchor, `v{n}.catalog.bin` (catalog header and leaf proof),
/// written first; an own report removes a stale catalog file first.
fn write_version(
    dir: &FsPath,
    m: &Manifest,
    r: &SignedReport,
    anchor: &AnchorView,
    catalog: Option<&(CatalogHeader, LeafProof)>,
) -> Result<(), ApiError> {
    let v = m.header.version;
    let io = |e: std::io::Error| internal(format!("bulk store: {e}"));
    let cf = dir.join(format!("v{v}.catalog.bin"));
    match catalog {
        Some(c) => {
            write_atomic(&cf, &borsh::to_vec(c).map_err(|_| internal("encode"))?).map_err(io)?
        }
        None => {
            if cf.exists() {
                std::fs::remove_file(&cf).map_err(io)?;
            }
        }
    }
    let rec = borsh::to_vec(&(
        m.to_bytes(),
        borsh::to_vec(r).unwrap_or_default(),
        anchor_to_bytes(anchor),
    ))
    .map_err(|_| internal("encode"))?;
    write_atomic(&dir.join(format!("v{v}.bin")), &rec).map_err(io)
}

/// Move `from` to `to`; done already (a crash between two moves) is fine.
fn move_aside(from: &FsPath, to: &FsPath) -> std::io::Result<()> {
    if !from.exists() && to.exists() {
        return Ok(());
    }
    if let Some(p) = to.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::rename(from, to)
}

/// Move version `v` (root `root`) of the dataset in `dir` to `withdrawn/`,
/// its record first, then its catalog proof (a proof left alone is never
/// read: loading stops at the first missing version, and `write_version`
/// removes it).
fn move_version_aside(dir: &FsPath, v: u32, root: &Hash32) -> std::io::Result<()> {
    let to = dir
        .join("withdrawn")
        .join(format!("v{v}-{}.bin", root.to_hex()));
    move_aside(&dir.join(format!("v{v}.bin")), &to)?;
    let cf = dir.join(format!("v{v}.catalog.bin"));
    if cf.exists() {
        move_aside(&cf, &to.with_extension("catalog.bin"))?;
    }
    Ok(())
}

/// The leaf rules of a catalog against what a node holds (ADR-0059
/// amendment 1 items 3, 4): no leaf names another root for a held version,
/// and a dataset `prev` does not list enters at version 1 or right after the
/// held version its leaf names. `held_at(dataset, v)` is the held root.
fn check_catalog_leaves(
    c: &Catalog,
    prev: Option<&Catalog>,
    held_at: impl Fn(&str, u32) -> Option<Hash32>,
) -> Result<(), ApiError> {
    for l in &c.leaves {
        // A tombstone is always taken (ADR-0059 amendment 4 item 4): a held
        // version at its number does not stop the removal.
        if l.is_tombstone() && held_at(&l.dataset, l.version).is_some() {
            eprintln!(
                "bulk: {}: a tombstone at version {} over a held version (taken; ADR-0059 amendment 4 item 4)",
                l.dataset, l.version
            );
        }
        if let Some(root) = held_at(&l.dataset, l.version).filter(|_| !l.is_tombstone()) {
            if root != l.root {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "bulk_conflicting_anchor",
                    format!(
                        "{}: version {} is held with another root",
                        l.dataset, l.version
                    ),
                ));
            }
        }
        let listed_before = prev.is_some_and(|p| p.find(&l.dataset).is_some());
        if !listed_before && l.version > 1 && held_at(&l.dataset, l.version - 1) != Some(l.prev) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "bulk_catalog_entry",
                format!(
                    "{} enters at version {}: upload version {} (its previous root) first",
                    l.dataset,
                    l.version,
                    l.version - 1
                ),
            ));
        }
    }
    Ok(())
}

/// A finalized block read behind one a held record was read at: try again.
fn behind(at: At, block: u64) -> ApiError {
    chain_unavailable(format!(
        "its finalized block {} is behind block {block}, where a held record was read",
        at.0
    ))
}

fn chain_unavailable(e: String) -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "bulk_chain_unavailable",
        format!("the chain could not be read ({e}); try again"),
    )
    .with_header("retry-after", "30".into())
}

/// Write `path` through a synced temporary file, then sync its folder: the
/// file is whole and on disk before it counts (review CAT-DIFF-R1 M2).
fn write_durable(path: &FsPath, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no folder"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    std::fs::File::open(dir)?.sync_all()
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
            catalogs: RwLock::new(BTreeMap::new()),
            old_body: Mutex::new(BTreeMap::new()),
            rebuilds: Arc::new(tokio::sync::Semaphore::new(MAX_REBUILDS)),
            answers: Mutex::new(AnswerBook::default()),
            file_reads: Arc::new(tokio::sync::Semaphore::new(MAX_FILE_READS)),
            catalog_bytes: Mutex::new(BTreeMap::new()),
            refused_keys: Mutex::new(BTreeMap::new()),
            publishers: (0..PUBLISHER_LOCKS)
                .map(|_| tokio::sync::RwLock::new(()))
                .collect(),
            withdrawn: Mutex::new(std::collections::BTreeSet::new()),
            forks: RwLock::new(BTreeMap::new()),
            fork_read: Mutex::new(BTreeMap::new()),
            free_per_minute: FREE_PER_MINUTE,
            rate: Caches::new(1, 1),
            erase_cfg: EraseConfig::default(),
            erase_lock: tokio::sync::RwLock::new(()),
            erase: Mutex::new(erase::EraseState::default()),
            #[cfg(test)]
            erase_stop: Mutex::new(None),
        };
        std::fs::create_dir_all(dir).map_err(|e| format!("bulk dir: {e}"))?;
        let tiers = dir.join("tiers.json");
        if tiers.exists() {
            s.tiers
                .load(&tiers)
                .map_err(|e| format!("bulk tiers: {e}"))?;
        }
        s.load_catalogs()?;
        s.load_manifests()?;
        s.load_removals()?;
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

    fn catalogs_dir(&self) -> PathBuf {
        self.dir.join("catalogs")
    }

    /// Every held catalog, checked again: decodes, shape, signature, and each
    /// the successor of the one before.
    fn load_catalogs(&self) -> Result<(), String> {
        let Ok(pubs) = std::fs::read_dir(self.catalogs_dir()) else {
            return Ok(());
        };
        let mut all: BTreeMap<String, BTreeMap<u64, HeldCatalog>> = BTreeMap::new();
        let mut forks = BTreeMap::new();
        let mut cat_bytes = BTreeMap::new();
        for p in pubs.flatten() {
            let mut held: BTreeMap<u64, HeldCatalog> = BTreeMap::new();
            let dir = p.path();
            let count = (0u64..)
                .take_while(|n| {
                    dir.join(format!("{n}.bin")).exists() || dir.join(format!("{n}.diff")).exists()
                })
                .count() as u64;
            // SCALE-5: only the newest catalog and the one before it are
            // rebuilt whole and checked (shape, successor), as when they were
            // taken; an older one by its header alone: its signature, its
            // number and its link to the one before. Its leaves are checked
            // when it is read (`catalog_body`).
            let mut before: Option<Catalog> = None;
            let mut prev_root = Hash32::ZERO;
            let mut bytes_held = 0u64;
            for n in 0..count {
                let f = dir.join(format!("{n}"));
                let (header, _, r, a, size) =
                    read_stored(&dir, n)?.ok_or_else(|| format!("{}: gone", f.display()))?;
                bytes_held += size;
                let catalog = if n + 2 >= count {
                    let c = rebuild_catalog(&dir, n, before.as_ref())?;
                    if c.header != header {
                        return Err(format!("{}: header and catalog differ", f.display()));
                    }
                    Some(c)
                } else {
                    None
                };
                let report = SignedReport::try_from_slice(&r)
                    .map_err(|_| format!("{}: report does not decode", f.display()))?;
                let signer =
                    cat::verify(&header, &report).map_err(|e| format!("{}: {e}", f.display()))?;
                let anchor =
                    anchor_from_bytes(&a).ok_or_else(|| format!("{}: anchor", f.display()))?;
                if header.number != n {
                    return Err(format!("{}: holds number {}", f.display(), header.number));
                }
                if header.prev != prev_root {
                    return Err(format!(
                        "{}: does not follow catalog {}",
                        f.display(),
                        n.saturating_sub(1)
                    ));
                }
                if let (Some(c), Some(prev)) = (&catalog, &before) {
                    c.check_successor(prev)
                        .map_err(|e| format!("{}: {e}", f.display()))?;
                }
                prev_root = header.root();
                held.insert(
                    n,
                    HeldCatalog {
                        header,
                        body: None,
                        report,
                        signer,
                        anchor,
                    },
                );
                before = catalog;
            }
            if let (Some(last), Some(c)) = (held.values_mut().next_back(), before) {
                last.body = Some(CatalogBody::new(c));
            }
            let f = p.path().join(FORK_FILE);
            let read = match std::fs::read(&f) {
                Ok(b) => Some(b),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                // An unreadable mark stops the start (never served as no fork).
                Err(e) => return Err(format!("{}: {e}", f.display())),
            };
            if let Some(bytes) = read {
                let fork = Fork::try_from_slice(&bytes)
                    .map_err(|_| format!("{}: does not decode", f.display()))?;
                for (h, r) in [&fork.held, &fork.other] {
                    cat::verify(h, r).map_err(|e| format!("{}: {e}", f.display()))?;
                }
                forks.insert(fork.held.0.publisher.0.clone(), fork);
            }
            if let Some(first) = held.values().next() {
                let publisher = first.header.publisher.0.clone();
                cat_bytes.insert(publisher.clone(), bytes_held);
                all.insert(publisher, held);
            }
        }
        *self.catalogs.write().map_err(|_| "lock")? = all;
        *self.catalog_bytes.lock().map_err(|_| "lock")? = cat_bytes;
        *self.forks.write().map_err(|_| "lock")? = forks;
        Ok(())
    }

    fn publisher_lock(&self, publisher: &str) -> &tokio::sync::RwLock<()> {
        let i = usize::from(keccak256(publisher.as_bytes()).0[0]) % self.publishers.len();
        &self.publishers[i]
    }

    /// Remember a key the book did not know or had suspended (review CL5-R5).
    fn refuse_key(&self, why: &str, r: &SignedReport, now: u64) {
        if why == reason::KEY {
            if let Ok(mut k) = self.refused_keys.lock() {
                k.insert(r.payload.key, now);
            }
        }
    }

    /// Where catalog `c` fits among its publisher's held catalogs, with no
    /// chain read: held already, the next one (successor and leaf rules
    /// checked), or its number held by another catalog (`Taken`, with the
    /// refusal). Anything else is refused.
    fn fit_catalog(&self, c: &Catalog, r: &SignedReport) -> Result<Fit, ApiError> {
        let publisher = &c.header.publisher.0;
        let n = c.header.number;
        let newest = {
            let cs = self.catalogs.read().map_err(|_| internal("lock"))?;
            let held = cs.get(publisher);
            if let Some(same) = held.and_then(|m| m.get(&n)) {
                if same.header.root() == c.header.root() && same.report == *r {
                    return Ok(Fit::Same(catalog_status(&same.header)));
                }
                return Ok(Fit::Taken(ApiError::new(
                    StatusCode::CONFLICT,
                    "bulk_catalog_conflict",
                    format!("this node holds another catalog {n} of this publisher"),
                )));
            }
            held.and_then(|m| m.last_key_value()).map(|(k, _)| *k)
        };
        let prev = match newest {
            None if n == 0 => None,
            Some(k) if Some(n) == k.checked_add(1) => {
                let p = self.held_body(publisher, k)?;
                if let Err(e) = c.check_successor(&p.catalog) {
                    return Ok(Fit::Broken(ApiError::new(
                        StatusCode::CONFLICT,
                        "bulk_catalog_not_successor",
                        e.to_string(),
                    )));
                }
                Some(p)
            }
            _ => {
                let need = newest.map_or(0, |k| k.saturating_add(1));
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "bulk_catalog_gap",
                    format!("catalogs are taken in order: this node needs catalog {need} next"),
                )
                .with_field("need", json!(need)));
            }
        };
        let ds = self.datasets.read().map_err(|_| internal("lock"))?;
        check_catalog_leaves(c, prev.as_ref().map(|p| &p.catalog), |d, v| {
            ds.get(&(publisher.clone(), d.to_string()))
                .and_then(|d| d.versions.get((v as usize).checked_sub(1)?))
                .map(|h| h.manifest.header.root())
        })?;
        Ok(Fit::Next)
    }

    /// Held catalog `n` of `publisher` whole (the newest from memory; after a
    /// set-aside, one file read).
    fn held_body(&self, publisher: &str, n: u64) -> Result<Arc<CatalogBody>, ApiError> {
        Ok(self
            .catalog_body(publisher, n)
            .map_err(|e| internal(format!("catalog: {e}")))?
            .ok_or_else(|| internal("catalog gone"))?
            .0)
    }

    /// [`read_anchor_at`] on this node's chain and book.
    async fn read_at(
        &self,
        r: &SignedReport,
        signer: &[u8; 20],
        at: At,
    ) -> Result<Reading, ApiError> {
        read_anchor_at(
            self.rpc.as_ref(),
            self.pin.chain_id,
            &self.pin.book,
            r,
            signer,
            at,
        )
        .await
        .map_err(chain_unavailable)
    }

    /// A report the book withdrew for good is refused from then on without a
    /// chain read (review of CAT-5, L1: a public withdrawn catalog replayed).
    fn remember_withdrawn(&self, digest: Hash32) {
        if let Ok(mut w) = self.withdrawn.lock() {
            if w.len() < MAX_WITHDRAWN {
                w.insert(digest);
            }
        }
    }

    /// Catalog `c` has a number another held catalog has (THREATS CAT-5,
    /// ADR-0059 amendment 1 item 3). `c`'s own anchor is read first, so a
    /// catalog the book does not hold costs no other read (review CAT-FIX-R1
    /// L3). Then every held catalog from that number on is read at the same
    /// finalized block (M1), at or past the block each was taken at; only
    /// when the book withdrew every one of them for good (H1: never for a
    /// suspended key, an open dispute or a lagging read) is the set-aside
    /// planned ([`Self::plan_cut`]), checked against `c`, and done, and `c`'s
    /// anchor returned. Otherwise the answer is `taken`.
    async fn set_aside_withdrawn(
        &self,
        c: &Catalog,
        r: &SignedReport,
        signer: &[u8; 20],
        taken: ApiError,
    ) -> Result<Settle, ApiError> {
        let at = decision_block(self.rpc.as_ref(), self.pin.chain_id)
            .await
            .map_err(chain_unavailable)?;
        let own = self.read_at(r, signer, at).await?;
        let anchor = match own.state {
            AnchorState::Anchored(a) => a,
            AnchorState::Not(why) => {
                if own.withdrawn {
                    self.remember_withdrawn(r.payload.digest());
                }
                self.refuse_key(why, r, (self.now)());
                return Err(not_anchored(why));
            }
        };
        let publisher = &c.header.publisher.0;
        // Newest first: (number, root, report, signer, block it was read at).
        type Held5 = (u64, Hash32, SignedReport, [u8; 20], u64);
        let held: Vec<Held5> = {
            let cs = self.catalogs.read().map_err(|_| internal("lock"))?;
            cs.get(publisher)
                .map(|m| {
                    m.range(c.header.number..)
                        .rev()
                        .map(|(k, h)| {
                            let b = h.anchor.block_number;
                            (*k, h.header.root(), h.report.clone(), h.signer, b)
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        if let Some(b) = held.iter().map(|h| h.4).max().filter(|b| *b > at.0) {
            return Err(behind(at, b));
        }
        // The held catalog with this number first: still holding, it is the
        // same catalog re-signed (nothing to change) or a fork (CAT-6).
        for (k, root, report, signer, _) in held.iter().rev() {
            let x = self.read_at(report, signer, at).await?;
            if *k == c.header.number {
                if let AnchorState::Anchored(_) = x.state {
                    if *root == c.header.root() {
                        return Ok(Settle::Same);
                    }
                    let header = self
                        .catalogs
                        .read()
                        .map_err(|_| internal("lock"))?
                        .get(publisher)
                        .and_then(|m| m.get(k))
                        .map(|h| h.header.clone())
                        .ok_or_else(|| internal("catalog gone"))?;
                    return Ok(Settle::Fork(Fork {
                        held: (header, report.clone()),
                        other: (c.header.clone(), r.clone()),
                        why: format!("two anchored catalogs {k} of this publisher"),
                    }));
                }
            }
            if !x.withdrawn {
                return Err(taken);
            }
        }
        let gone: Vec<(u64, Hash32, Hash32)> = held
            .iter()
            .map(|h| (h.0, h.1, h.2.payload.digest()))
            .collect();
        let plan = self.plan_cut(c, &gone, at).await?;
        self.set_aside(c, r, signer, &anchor, &gone, &plan)?;
        Ok(Settle::Done(anchor))
    }

    /// The status of held catalog `n` of `publisher` (an answer of 200).
    fn catalog_status_of(&self, publisher: &str, n: u64) -> Result<Value, ApiError> {
        let cs = self.catalogs.read().map_err(|_| internal("lock"))?;
        Ok(cs
            .get(publisher)
            .and_then(|m| m.get(&n))
            .map(|h| catalog_status(&h.header))
            .unwrap_or(Value::Null))
    }

    /// Catalog `c` is not the append-only successor of the newest held one
    /// (`broken`). A fork (THREATS CAT-6) only when both are anchored at one
    /// finalized block, `c`'s own anchor read first (review CAT-FIX-R1 L3);
    /// otherwise the answer is `broken`.
    async fn broken_fork(
        &self,
        c: &Catalog,
        r: &SignedReport,
        signer: &[u8; 20],
        broken: ApiError,
    ) -> Result<Fork, ApiError> {
        let at = decision_block(self.rpc.as_ref(), self.pin.chain_id)
            .await
            .map_err(chain_unavailable)?;
        let own = self.read_at(r, signer, at).await?;
        if !matches!(own.state, AnchorState::Anchored(_)) {
            if own.withdrawn {
                self.remember_withdrawn(r.payload.digest());
            }
            return Err(broken);
        }
        let publisher = &c.header.publisher.0;
        let newest = self
            .catalogs
            .read()
            .map_err(|_| internal("lock"))?
            .get(publisher)
            .and_then(|m| m.last_key_value())
            .map(|(_, h)| {
                (
                    h.header.clone(),
                    h.report.clone(),
                    h.signer,
                    h.anchor.block_number,
                )
            });
        let Some((header, report, held_signer, block)) = newest else {
            return Err(broken);
        };
        if block > at.0 {
            return Err(behind(at, block));
        }
        if !matches!(
            self.read_at(&report, &held_signer, at).await?.state,
            AnchorState::Anchored(_)
        ) {
            return Err(broken);
        }
        Ok(Fork {
            why: format!(
                "catalog {} is anchored but not the append-only successor of anchored catalog {}: {}",
                c.header.number, header.number, broken.message
            ),
            held: (header, report),
            other: (c.header.clone(), r.clone()),
        })
    }

    /// Record fork `f` (file first, then memory).
    fn mark_fork(&self, f: &Fork) -> Result<(), ApiError> {
        let publisher = &f.held.0.publisher.0;
        let bytes = borsh::to_vec(f).map_err(|_| internal("encode"))?;
        write_durable(&self.catalogs_dir().join(publisher).join(FORK_FILE), &bytes)
            .map_err(|e| internal(format!("bulk store: {e}")))?;
        self.forks
            .write()
            .map_err(|_| internal("lock"))?
            .insert(publisher.clone(), f.clone());
        if let Ok(mut t) = self.fork_read.lock() {
            t.insert(publisher.clone(), (self.now)());
        }
        eprintln!(
            "bulk: catalog fork of {publisher}: {} (THREATS CAT-6)",
            f.why
        );
        Ok(())
    }

    /// The publisher's catalog fork, if it still stands: both sides are read
    /// again at one finalized block at most every [`ANCHOR_RECHECK_SECS`];
    /// when the book withdrew either side for good, the record moves to
    /// `withdrawn/` and the mark clears.
    async fn fork_stands(&self, publisher: &str) -> Result<Option<Fork>, ApiError> {
        let Some(f) = self
            .forks
            .read()
            .map_err(|_| internal("lock"))?
            .get(publisher)
            .cloned()
        else {
            return Ok(None);
        };
        let now = (self.now)();
        let fresh = self
            .fork_read
            .lock()
            .ok()
            .and_then(|t| t.get(publisher).copied())
            .is_some_and(|t| now.saturating_sub(t) < ANCHOR_RECHECK_SECS);
        if fresh {
            return Ok(Some(f));
        }
        // One read per publisher per window, claimed before it starts: the
        // requests meanwhile, and after a failed read, keep the mark without
        // reading (review of CAT-6, L3; a failed read delays a clear, never
        // makes one).
        if let Ok(mut t) = self.fork_read.lock() {
            t.insert(publisher.to_string(), now);
        }
        let at = decision_block(self.rpc.as_ref(), self.pin.chain_id)
            .await
            .map_err(chain_unavailable)?;
        let mut cleared = false;
        for (h, r) in [&f.held, &f.other] {
            let signer = cat::verify(h, r).map_err(|e| internal(format!("fork record: {e}")))?;
            if self.read_at(r, &signer, at).await?.withdrawn {
                cleared = true;
                break;
            }
        }
        if !cleared {
            return Ok(Some(f));
        }
        let mut forks = self.forks.write().map_err(|_| internal("lock"))?;
        if forks.get(publisher) == Some(&f) {
            let dir = self.catalogs_dir().join(publisher);
            let to = dir.join("withdrawn").join(format!(
                "fork-{}-{}.bin",
                f.held.0.root().to_hex(),
                f.other.0.root().to_hex()
            ));
            move_aside(&dir.join(FORK_FILE), &to)
                .map_err(|e| internal(format!("bulk store: {e}")))?;
            forks.remove(publisher);
            eprintln!(
                "bulk: catalog fork of {publisher} cleared: one side withdrawn (THREATS CAT-6)"
            );
        }
        // Another fork marked meanwhile stands (review of CAT-6, L2).
        Ok(forks.get(publisher).cloned())
    }

    /// `GET /v1/bulk/{publisher}/{dataset}`, after re-reading a catalog fork
    /// of its publisher.
    pub async fn info_checked(&self, publisher: &str, dataset: &str) -> Result<Value, ApiError> {
        if let Some(n) = self.removed(publisher, dataset)? {
            return Err(self.gone(publisher, dataset, n));
        }
        self.fork_stands(publisher).await?;
        self.info(publisher, dataset)
    }

    /// Whether `publisher` has a catalog fork mark now (no chain read).
    fn fork_marked(&self, publisher: &str) -> Option<Fork> {
        self.forks.read().ok()?.get(publisher).cloned()
    }

    /// What setting aside the catalogs `gone` (number, root, digest) does to
    /// `c`'s publisher's versions, decided before anything moves. Per
    /// dataset, the first version that goes: the first one a set-aside
    /// catalog anchors, or an earlier one `c` names another root for whose
    /// report the book withdrew for good, read at `at` (review of CAT-5, M3;
    /// one still holding refuses `c`, nothing moved). Every later version
    /// goes with it. When `c` lists that first version unchanged, it stays,
    /// anchored by `c` (M4). Then `c` must fit what stays: the successor of
    /// the catalog before it, every leaf agreeing.
    async fn plan_cut(
        &self,
        c: &Catalog,
        gone: &[(u64, Hash32, Hash32)],
        at: At,
    ) -> Result<Vec<Cut>, ApiError> {
        let publisher = &c.header.publisher.0;
        let roots: Vec<Hash32> = gone.iter().map(|g| g.1).collect();
        // Per dataset: (version, root) of every held version, the first index
        // that goes, and the reports of versions `c` contradicts.
        let mut vs: BTreeMap<String, (Vec<(u32, Hash32)>, usize)> = BTreeMap::new();
        let mut contradicted: Vec<(String, usize, SignedReport, [u8; 20], u64)> = Vec::new();
        {
            let ds = self.datasets.read().map_err(|_| internal("lock"))?;
            for ((_, d), set) in ds
                .range((publisher.clone(), String::new())..)
                .take_while(|((p, _), _)| p == publisher)
            {
                let from = set
                    .versions
                    .iter()
                    .position(|h| {
                        h.catalog
                            .as_ref()
                            .is_some_and(|x| roots.contains(&x.0.root()))
                    })
                    .unwrap_or(set.versions.len());
                if let Some(l) = c
                    .find(d)
                    .map(|i| &c.leaves[i])
                    .filter(|l| !l.is_tombstone())
                {
                    let i = l.version as usize - 1;
                    if let Some(h) = set
                        .versions
                        .get(i)
                        .filter(|h| i < from && h.manifest.header.root() != l.root)
                    {
                        contradicted.push((
                            d.clone(),
                            i,
                            h.report.clone(),
                            h.signer,
                            h.anchor.block_number,
                        ));
                    }
                }
                let list = set
                    .versions
                    .iter()
                    .map(|h| (h.manifest.header.version, h.manifest.header.root()))
                    .collect();
                vs.insert(d.clone(), (list, from));
            }
        }
        let mut read: BTreeMap<Hash32, bool> = BTreeMap::new();
        for (d, i, report, signer, block) in contradicted {
            if block > at.0 {
                return Err(behind(at, block));
            }
            let digest = report.payload.digest();
            let withdrawn = match read.get(&digest) {
                Some(w) => *w,
                None => {
                    let w = self.read_at(&report, &signer, at).await?.withdrawn;
                    read.insert(digest, w);
                    w
                }
            };
            if !withdrawn {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "bulk_conflicting_anchor",
                    format!(
                        "{d}: version {} is held with another root whose report the book has not withdrawn",
                        i + 1
                    ),
                ));
            }
            if let Some(x) = vs.get_mut(&d) {
                x.1 = x.1.min(i);
            }
        }
        let mut plan = Vec::new();
        for (d, (list, from)) in &vs {
            if *from >= list.len() {
                continue;
            }
            let keep = c
                .find(d)
                .map(|i| &c.leaves[i])
                .filter(|l| list[*from] == (l.version, l.root))
                .map(|_| list[*from].0);
            plan.push(Cut {
                dataset: d.clone(),
                from: *from,
                versions: list[*from..].to_vec(),
                keep,
            });
        }
        // `c` against what stays.
        let n = c.header.number;
        let prev = match n.checked_sub(1) {
            Some(k) => Some(self.held_body(publisher, k)?),
            None => None,
        };
        if let Some(p) = &prev {
            c.check_successor(&p.catalog).map_err(|e| {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "bulk_catalog_not_successor",
                    e.to_string(),
                )
            })?;
        }
        check_catalog_leaves(c, prev.as_ref().map(|p| &p.catalog), |d, v| {
            let (list, from) = vs.get(d)?;
            let stays = from + usize::from(plan.iter().any(|x| x.dataset == d && x.keep.is_some()));
            let i = (v as usize).checked_sub(1)?;
            (i < stays).then(|| list.get(i).map(|x| x.1)).flatten()
        })?;
        Ok(plan)
    }

    /// Do a planned set-aside ([`Self::plan_cut`]) for catalog `c` (report
    /// `r`, anchor `anchor`): per dataset, its versions from the cut go to
    /// `withdrawn/`, newest first, so a crash leaves versions 1..k on disk
    /// (review of CAT-5, M2); a version `c` lists unchanged is then written
    /// back under `c`'s anchor (a crash before that leaves it to be uploaded
    /// again, never a record and proof that disagree). A dataset left with
    /// no version leaves the list, its fork mark aside too (L3). Then the
    /// catalogs `gone`, newest first, so the held ones on disk are always
    /// 0..k and the next upload sets the rest aside again (CAT-FIX-R1 M3).
    /// Their digests are refused from then on.
    fn set_aside(
        &self,
        c: &Catalog,
        r: &SignedReport,
        signer: &[u8; 20],
        anchor: &AnchorView,
        gone: &[(u64, Hash32, Hash32)],
        plan: &[Cut],
    ) -> Result<(), ApiError> {
        let io = |e: std::io::Error| internal(format!("bulk store: {e}"));
        let publisher = &c.header.publisher.0;
        let tree = plan
            .iter()
            .any(|x| x.keep.is_some())
            .then(|| cat::leaves_tree(&c.leaves));
        for cut in plan {
            let dir = self.dataset_dir(publisher, &cut.dataset);
            for (v, root) in cut.versions.iter().rev() {
                move_version_aside(&dir, *v, root).map_err(io)?;
            }
            let key = (publisher.clone(), cut.dataset.clone());
            let mut ds = self.datasets.write().map_err(|_| internal("lock"))?;
            let Some(d) = ds.get_mut(&key) else { continue };
            if d.versions.get(cut.from).map(|h| h.manifest.header.root())
                != cut.versions.first().map(|v| v.1)
            {
                continue;
            }
            let kept = match (cut.keep, &tree) {
                (Some(_), Some(tree)) => {
                    let lp = c
                        .find(&cut.dataset)
                        .and_then(|i| c.leaf_proof_in(tree, i))
                        .ok_or_else(|| internal("leaf proof"))?;
                    let mut h = d.versions.swap_remove(cut.from);
                    let proof = Box::new((c.header.clone(), lp));
                    write_version(&dir, &h.manifest, r, anchor, Some(&*proof))?;
                    h.report = r.clone();
                    h.signer = *signer;
                    h.anchor = anchor.clone();
                    h.catalog = Some(proof);
                    Some(h)
                }
                _ => None,
            };
            d.versions.truncate(cut.from);
            d.versions.extend(kept);
            if d.versions.is_empty() {
                let f = dir.join("forked.txt");
                if f.exists() {
                    move_aside(&f, &dir.join("withdrawn").join("forked.txt")).map_err(io)?;
                }
                ds.remove(&key);
            }
        }
        let dir = self.catalogs_dir().join(publisher);
        // The newest that stays, whole, read before anything moves: it is
        // put in place under the same write lock that removes the last
        // set-aside catalog, so removals (ADR-0059 amendment 4) read its
        // tombstones from that moment (review of the CAT-3 build, finding 3).
        let stays = gone
            .iter()
            .map(|g| g.0)
            .min()
            .and_then(|m| m.checked_sub(1))
            .map(|k| self.held_body(publisher, k).map(|b| (k, b)))
            .transpose()?;
        if let Ok(mut c) = self.old_body.lock() {
            c.remove(publisher);
        }
        for (k, root, digest) in gone {
            // Whole or a diff; diffs above a set-aside one go first (newest
            // first), so what stays is always a prefix 0..k.
            let ext = if dir.join(format!("{k}.bin")).exists() {
                "bin"
            } else {
                "diff"
            };
            let from = dir.join(format!("{k}.{ext}"));
            let size = std::fs::metadata(&from).map_or(0, |m| m.len());
            let to = dir
                .join("withdrawn")
                .join(format!("{k}-{}.{ext}", root.to_hex()));
            move_aside(&from, &to).map_err(io)?;
            if let Ok(mut b) = self.catalog_bytes.lock() {
                if let Some(x) = b.get_mut(publisher) {
                    *x = x.saturating_sub(size);
                }
            }
            let mut cs = self.catalogs.write().map_err(|_| internal("lock"))?;
            if let Some(m) = cs.get_mut(publisher) {
                m.remove(k);
                if let Some((sk, body)) = &stays {
                    if let Some(mut last) = m.last_entry().filter(|e| e.key() == sk) {
                        let h = last.get_mut();
                        if h.body.is_none() {
                            h.body = Some(body.clone());
                        }
                    }
                }
                if m.is_empty() {
                    cs.remove(publisher);
                }
            }
            self.mark(*digest, false);
            self.remember_withdrawn(*digest);
        }
        self.mark(r.payload.digest(), true);
        eprintln!(
            "bulk: set aside {} withdrawn catalog(s) of {publisher} from number {}; {} dataset(s) cut, {} kept under the new catalog (THREATS CAT-5)",
            gone.len(),
            gone.last().map_or(0, |g| g.0),
            plan.len(),
            plan.iter().filter(|x| x.keep.is_some()).count()
        );
        Ok(())
    }

    /// Take a catalog and its signed report (ADR-0059 amendment 1 items 3-5):
    /// its shape and signature, at most `max_datasets` leaves, after the
    /// newest held catalog of its publisher as its append-only successor (or
    /// number 0), each dataset entering at version 1 or after a held version
    /// its leaf names, no leaf disagreeing with a held version, and the report
    /// recorded and accepted at the finalized block with the key active. A
    /// catalog whose number another held catalog has is taken only when the
    /// book withdrew every held catalog from that number on: those are set
    /// aside first (THREATS CAT-5).
    pub async fn accept_catalog(
        &self,
        catalog: &[u8],
        report: &[u8],
    ) -> Result<(StatusCode, Value), ApiError> {
        let bad =
            |s: String| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "bulk_bad_catalog", s);
        if catalog.len() > cat::MAX_CATALOG_BYTES {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body_too_large",
                "catalog over its cap",
            ));
        }
        let c = Catalog::try_from_slice(catalog)
            .map_err(|_| bad("catalog: not a Borsh catalog".into()))?;
        c.validate().map_err(|e| bad(e.to_string()))?;
        let r = SignedReport::try_from_slice(report)
            .map_err(|_| bad_request("report: not a Borsh SignedReport"))?;
        let signer = cat::verify(&c.header, &r).map_err(|e| bad(format!("signature: {e}")))?;
        if c.leaves.len() > self.limits.max_datasets {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "bulk_quota_exceeded",
                format!(
                    "a publisher may hold at most {} datasets on this node",
                    self.limits.max_datasets
                ),
            ));
        }
        let publisher = c.header.publisher.0.clone();
        let n = c.header.number;
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
        if self
            .withdrawn
            .lock()
            .is_ok_and(|w| w.contains(&r.payload.digest()))
        {
            return Err(not_anchored(reason::NOT_ACCEPTED));
        }
        // No erase pass meanwhile: a catalog can bring a removed dataset back
        // (a set-aside) or name an erased root (review of the CAT-3 erase
        // build, finding 1). Taken before the publisher lock, never nested.
        let _no_erase = self.erase_lock.read().await;
        // One catalog upload of a publisher at a time, and no manifest upload
        // of it meanwhile (review CAT-FIX-R1 M2, M3).
        let _one = self.publisher_lock(&publisher).write().await;
        if let Some(f) = self.fork_stands(&publisher).await? {
            return Err(publisher_forked(&f));
        }
        let known = match self.fit_catalog(&c, &r)? {
            Fit::Same(status) => return Ok((StatusCode::OK, status)),
            Fit::Next => None,
            Fit::Broken(e) => {
                let f = self.broken_fork(&c, &r, &signer, e).await?;
                self.mark_fork(&f)?;
                return Err(publisher_forked(&f));
            }
            Fit::Taken(taken) => {
                let held_n = self.catalog_status_of(&publisher, n)?;
                match self.set_aside_withdrawn(&c, &r, &signer, taken).await? {
                    Settle::Same => return Ok((StatusCode::OK, held_n)),
                    Settle::Fork(f) => {
                        self.mark_fork(&f)?;
                        return Err(publisher_forked(&f));
                    }
                    Settle::Done(a) => match self.fit_catalog(&c, &r)? {
                        Fit::Next => Some(a),
                        Fit::Same(status) => return Ok((StatusCode::OK, status)),
                        Fit::Taken(e) | Fit::Broken(e) => return Err(e),
                    },
                }
            }
        };
        let anchor = match known {
            Some(a) => a,
            None => match self.anchor_of(&r, &signer).await? {
                AnchorState::Anchored(a) => a,
                AnchorState::Not(why) => {
                    self.refuse_key(why, &r, now);
                    return Err(not_anchored(why));
                }
            },
        };
        // Whole every `catalog_checkpoint`, else the leaves that differ from
        // the one before (ADR-0059 amendment 2 draft 4), written durably
        // before it counts; the diff is checked to rebuild this catalog.
        let next = self
            .catalogs
            .read()
            .map_err(|_| internal("lock"))?
            .get(&publisher)
            .and_then(|m| m.last_key_value())
            .map_or(0, |(k, _)| k + 1);
        if next != n {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "bulk_catalog_gap",
                "another catalog was taken meanwhile; upload again",
            ));
        }
        let report_bytes = borsh::to_vec(&r).map_err(|_| internal("encode"))?;
        let (name, rec) = if n % self.limits.catalog_checkpoint.max(1) == 0 {
            let rec = borsh::to_vec(&(catalog.to_vec(), report_bytes, anchor_to_bytes(&anchor)))
                .map_err(|_| internal("encode"))?;
            (format!("{n}.bin"), rec)
        } else {
            let before = self.held_body(&publisher, n - 1)?;
            let leaves = diff_of(&before.catalog, &c);
            if cat::apply_diff(&before.catalog.leaves, &leaves) != c.leaves {
                return Err(internal("a catalog diff does not rebuild its catalog"));
            }
            let d = CatalogDiff {
                header: c.header.clone(),
                report: report_bytes,
                anchor: anchor_to_bytes(&anchor),
                leaves,
            };
            (
                format!("{n}.diff"),
                borsh::to_vec(&d).map_err(|_| internal("encode"))?,
            )
        };
        self.check_catalog_quota(&publisher, rec.len() as u64)?;
        write_durable(&self.catalogs_dir().join(&publisher).join(name), &rec)
            .map_err(|e| internal(format!("bulk store: {e}")))?;
        if let Ok(mut b) = self.catalog_bytes.lock() {
            *b.entry(publisher.clone()).or_default() += rec.len() as u64;
        }
        let mut cs = self.catalogs.write().map_err(|_| internal("lock"))?;
        let held = cs.entry(publisher.clone()).or_default();
        if held.last_key_value().map_or(0, |(k, _)| k + 1) != n {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "bulk_catalog_gap",
                "another catalog was taken meanwhile; upload again",
            ));
        }
        self.mark(r.payload.digest(), true);
        let status = catalog_status(&c.header);
        // The one before is no longer the newest: its body is read from its
        // file when asked.
        if let Some(mut before) = held.last_entry() {
            before.get_mut().body = None;
        }
        held.insert(
            n,
            HeldCatalog {
                header: c.header.clone(),
                body: Some(CatalogBody::new(c)),
                report: r,
                signer,
                anchor,
            },
        );
        Ok((StatusCode::CREATED, status))
    }

    /// A held catalog and its report (`GET /v1/bulk/catalogs/{publisher}/{n}`);
    /// `Err` when it is held but cannot be rebuilt (answered 500, never "not
    /// held": review CAT-DIFF-R1 L1).
    pub fn catalog_record(
        &self,
        publisher: &str,
        n: u64,
    ) -> Result<Option<(Catalog, SignedReport)>, String> {
        Ok(self
            .catalog_body(publisher, n)?
            .map(|(body, report)| (body.catalog.clone(), report)))
    }

    /// The number of `publisher`'s newest held catalog when it lists
    /// `dataset` as a tombstone (ADR-0059 amendment 4 item 3: removal is
    /// this derived state, nothing held is deleted, and a set-aside of that
    /// catalog ends it).
    ///
    /// Fails closed (review of the CAT-3 build, finding 3): a poisoned lock
    /// or a newest catalog not held whole is an error, never "not removed".
    pub fn removed(&self, publisher: &str, dataset: &str) -> Result<Option<u64>, ApiError> {
        let cs = self.catalogs.read().map_err(|_| internal("lock"))?;
        let Some((n, h)) = cs.get(publisher).and_then(|m| m.last_key_value()) else {
            return Ok(None);
        };
        let b = h
            .body
            .as_ref()
            .ok_or_else(|| internal("the newest catalog is not held whole: try again"))?;
        Ok(b.removed.contains(dataset).then_some(*n))
    }

    /// A removed dataset's 410, with its erase phase and due time when this
    /// node keeps a record of it (ADR-0059 amendment 4 item 5a).
    fn gone(&self, publisher: &str, dataset: &str, n: u64) -> ApiError {
        let e = dataset_removed(n);
        match self.erase_json(publisher, dataset) {
            Some(v) => e.with_field("erase", v),
            None => e,
        }
    }

    /// The newest held catalog body of every publisher that removed a
    /// dataset (for listings: one read of the catalogs, never under the
    /// datasets lock).
    fn removals(&self) -> Result<BTreeMap<String, Arc<CatalogBody>>, ApiError> {
        let cs = self.catalogs.read().map_err(|_| internal("lock"))?;
        let mut out = BTreeMap::new();
        for (p, m) in cs.iter() {
            let Some((_, h)) = m.last_key_value() else {
                continue;
            };
            let b = h
                .body
                .clone()
                .ok_or_else(|| internal("a newest catalog is not held whole: try again"))?;
            if !b.removed.is_empty() {
                out.insert(p.clone(), b);
            }
        }
        Ok(out)
    }

    /// The root and report hash of held catalog `n`: what a kept answer
    /// for it must match.
    fn catalog_check(&self, publisher: &str, n: u64) -> Option<(Hash32, Hash32)> {
        let cs = self.catalogs.read().ok()?;
        let h = cs.get(publisher)?.get(&n)?;
        Some((h.header.root(), h.report.hash()))
    }

    /// The live answer under `key` when it was made for `check` (kept, or
    /// still being sent to someone); a stale one is dropped (SERVE-WRITE-2).
    fn answer(&self, key: &str, check: (Hash32, Hash32)) -> Option<Answer> {
        self.answers.lock().ok()?.get(key, check)
    }

    /// Wait for the turn to build `key`'s answer: the first asker builds, the
    /// others find it built when their turn comes.
    async fn answer_turn(self: &Arc<Self>, key: &str) -> AnswerTurn {
        let gate = self
            .answers
            .lock()
            .map(|mut b| b.building.entry(key.to_string()).or_default().clone())
            .unwrap_or_default();
        let held = gate.clone().lock_owned().await;
        AnswerTurn {
            store: self.clone(),
            key: key.to_string(),
            gate,
            _held: held,
        }
    }

    /// Keep a built answer and hand it out.
    fn keep_answer(
        &self,
        key: String,
        check: (Hash32, Hash32),
        bytes: axum::body::Bytes,
    ) -> Answer {
        match self.answers.lock() {
            Ok(mut b) => b.put(key, check, bytes),
            Err(_) => Arc::new(bytes),
        }
    }

    /// The report hash of held manifest `root`, without copying it.
    fn manifest_check(&self, root: &Hash32) -> Option<(Hash32, Hash32)> {
        let ds = self.datasets.read().ok()?;
        ds.values()
            .flat_map(|d| d.versions.iter())
            .find(|h| h.manifest.header.root() == *root)
            .map(|h| (*root, h.report.hash()))
    }

    /// Held catalog `n` without a rebuild (shared, not copied): the newest,
    /// or the one rebuilt last for its publisher. `None` otherwise (also
    /// when not held).
    fn catalog_ready(&self, publisher: &str, n: u64) -> Option<(Arc<CatalogBody>, SignedReport)> {
        let cs = self.catalogs.read().ok()?;
        let h = cs.get(publisher)?.get(&n)?;
        if let Some(b) = &h.body {
            return Some((b.clone(), h.report.clone()));
        }
        let c = self.old_body.lock().ok()?;
        let (k, b) = c.get(publisher)?;
        (*k == n && b.catalog.header == h.header).then(|| (b.clone(), h.report.clone()))
    }

    /// Held catalogs of `publisher` from `from` on, as stored (ADR-0059
    /// amendment 3 item 1), Borsh `CatalogFiles`: to the end of `from`'s
    /// block of `catalog_checkpoint` or the newest, at most `cap` bytes (at
    /// least one catalog). Each file's header must be the one taken at
    /// upload; a file gone meanwhile (a set-aside) ends the answer there.
    /// Written from the stored bytes: no leaf is decoded, nothing rebuilt
    /// (review of the CAT-1 build, finding 5). `Ok(None)` when `from` is not
    /// held.
    fn catalog_files(
        &self,
        publisher: &str,
        from: u64,
        cap: usize,
    ) -> Result<Option<Vec<u8>>, String> {
        let (newest, headers) = {
            let cs = self.catalogs.read().map_err(|_| "lock")?;
            let Some(m) = cs.get(publisher) else {
                return Ok(None);
            };
            let Some((&newest, _)) = m.last_key_value() else {
                return Ok(None);
            };
            if !m.contains_key(&from) {
                return Ok(None);
            }
            let step = self.limits.catalog_checkpoint.max(1);
            let end = newest.min((from / step).saturating_add(1).saturating_mul(step) - 1);
            let headers: Vec<(u64, CatalogHeader)> = m
                .range(from..=end)
                .map(|(k, h)| (*k, h.header.clone()))
                .collect();
            (newest, headers)
        };
        let dir = self.catalogs_dir().join(publisher);
        // `CatalogFiles`: newest (u64), then a u32 count and each
        // `CatalogFile` (header, report, `FileLeaves` variant and leaves).
        let mut out = newest.to_le_bytes().to_vec();
        out.extend_from_slice(&0u32.to_le_bytes());
        let mut count = 0u32;
        let mut want = from;
        for (k, held) in headers {
            if k != want {
                break;
            }
            let Some((header, stored, report, _, _)) = read_stored(&dir, k)? else {
                break;
            };
            if header != held {
                break;
            }
            let head = borsh::to_vec(&header).map_err(|e| e.to_string())?;
            let mut file = head.clone();
            file.extend_from_slice(&report);
            match stored {
                // A whole file's catalog is borsh(header) then borsh(leaves).
                Stored::Whole(c) => {
                    let leaves = c
                        .get(head.len()..)
                        .filter(|_| c.starts_with(&head))
                        .ok_or_else(|| format!("catalog {k}: whole file and header differ"))?;
                    file.push(0);
                    file.extend_from_slice(leaves);
                }
                Stored::Diff(l) => {
                    file.push(1);
                    file.extend_from_slice(&borsh::to_vec(&l).map_err(|e| e.to_string())?);
                }
            }
            if count > 0 && out.len().saturating_add(file.len()) > cap {
                break;
            }
            out.extend_from_slice(&file);
            count += 1;
            want = k + 1;
        }
        if count == 0 {
            return Ok(None);
        }
        out[8..12].copy_from_slice(&count.to_le_bytes());
        Ok(Some(out))
    }

    /// Held catalog `n` of `publisher` whole, with its report: the newest
    /// from memory, an older one rebuilt from its files (from the
    /// publisher's last rebuilt catalog when that is below it and still the
    /// held one) and checked against the header taken at upload. `Ok(None)`
    /// when not held.
    fn catalog_body(
        &self,
        publisher: &str,
        n: u64,
    ) -> Result<Option<(Arc<CatalogBody>, SignedReport)>, String> {
        let cached = self
            .old_body
            .lock()
            .ok()
            .and_then(|c| c.get(publisher).cloned());
        let (header, report, start) = {
            let cs = self.catalogs.read().map_err(|_| "lock")?;
            let Some(m) = cs.get(publisher) else {
                return Ok(None);
            };
            let Some(h) = m.get(&n) else {
                return Ok(None);
            };
            if let Some(b) = &h.body {
                return Ok(Some((b.clone(), h.report.clone())));
            }
            let start = cached
                .filter(|(k, b)| *k <= n && m.get(k).is_some_and(|x| x.header == b.catalog.header));
            (h.header.clone(), h.report.clone(), start)
        };
        if let Some((k, b)) = &start {
            if *k == n {
                return Ok(Some((b.clone(), report)));
            }
        }
        let dir = self.catalogs_dir().join(publisher);
        let catalog = rebuild_catalog(&dir, n, start.as_ref().map(|(_, b)| &b.catalog))?;
        if catalog.header != header {
            return Err(format!(
                "{}: catalog {n} is not the one taken",
                dir.display()
            ));
        }
        let body = CatalogBody::new(catalog);
        if let Ok(mut c) = self.old_body.lock() {
            c.insert(publisher.to_string(), (n, body.clone()));
        }
        Ok(Some((body, report)))
    }

    /// Stored catalog bytes of `publisher`.
    fn catalog_bytes_of(&self, publisher: &str) -> u64 {
        self.catalog_bytes
            .lock()
            .ok()
            .and_then(|c| c.get(publisher).copied())
            .unwrap_or(0)
    }

    /// Room for `add` more catalog bytes of `publisher`: its datasets'
    /// latest versions and stored catalogs within its quota, and the node
    /// within its total (ADR-0059 amendment 2 draft 4 item 7).
    fn check_catalog_quota(&self, publisher: &str, add: u64) -> Result<(), ApiError> {
        // An erased dataset's bytes are gone (ADR-0059 amendment 4 item 5a).
        let erased = self.erased_datasets();
        let (mine, all) = {
            let ds = self.datasets.read().map_err(|_| internal("lock"))?;
            ds.iter().fold((0u64, 0u64), |(mine, all), (k, d)| {
                let p = &k.0;
                let b = if erased.contains(k) {
                    0
                } else {
                    d.latest().map_or(0, |h| h.manifest.header.byte_count)
                };
                (
                    if p == publisher {
                        mine.saturating_add(b)
                    } else {
                        mine
                    },
                    all.saturating_add(b),
                )
            })
        };
        let cats: u64 = self
            .catalog_bytes
            .lock()
            .map(|c| c.values().sum())
            .unwrap_or(0);
        let mine = mine
            .saturating_add(self.catalog_bytes_of(publisher))
            .saturating_add(add);
        if mine > self.limits.quota_bytes {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "bulk_quota_exceeded",
                format!(
                    "a publisher may hold at most {} bytes (datasets and catalogs) on this node",
                    self.limits.quota_bytes
                ),
            ));
        }
        if all.saturating_add(cats).saturating_add(add) > self.limits.max_total_bytes {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "bulk_node_full",
                "this node holds no more bulk data",
            ));
        }
        Ok(())
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
                    let cf = d.path().join(format!("v{v}.catalog.bin"));
                    let catalog: Option<Box<(CatalogHeader, LeafProof)>> = match std::fs::read(&cf)
                    {
                        Ok(b) => Some(Box::new(
                            BorshDeserialize::try_from_slice(&b)
                                .map_err(|_| format!("{}: does not decode", cf.display()))?,
                        )),
                        Err(_) => None,
                    };
                    let signer = match &catalog {
                        None => onyx_bulk::anchor::verify(&manifest.header, &report)
                            .map_err(|e| format!("{}: {e}", f.display()))?,
                        Some(c) => {
                            c.0.proves(&c.1)
                                .map_err(|e| format!("{}: {e}", cf.display()))?;
                            if !c.1.leaf.names(&manifest.header) {
                                return Err(format!("{}: leaf of another manifest", cf.display()));
                            }
                            cat::verify(&c.0, &report)
                                .map_err(|e| format!("{}: {e}", f.display()))?
                        }
                    };
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
                        catalog,
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
        .map_err(chain_unavailable)
    }

    /// Take a manifest and its signed report (see the module docs).
    pub async fn accept_manifest(
        &self,
        manifest: &[u8],
        report: &[u8],
    ) -> Result<(StatusCode, Value), ApiError> {
        self.accept_manifest_as(manifest, report, false).await
    }

    /// [`Self::accept_manifest`]; `public` (the upload route, not a storage
    /// node's own fetch for a deal) also refuses a removed dataset, read
    /// under the publisher lock (review of the CAT-3 build, finding 6).
    async fn accept_manifest_as(
        &self,
        manifest: &[u8],
        report: &[u8],
        public: bool,
    ) -> Result<(StatusCode, Value), ApiError> {
        let m = decode_manifest(manifest)?;
        let r = SignedReport::try_from_slice(report)
            .map_err(|_| bad_request("report: not a Borsh SignedReport"))?;
        let signer = onyx_bulk::anchor::verify(&m.header, &r).map_err(|e| {
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "bulk_bad_manifest",
                format!("signature: {e}"),
            )
        })?;
        // The erase lock first, then the publisher's (review CAT-ERASE-R1 H1).
        let _no_erase = self.erase_lock.read().await;
        let _one = self.publisher_lock(&m.header.publisher.0).read().await;
        if public {
            if let Some(n) = self.removed(&m.header.publisher.0, &m.header.dataset)? {
                return Err(dataset_removed(n));
            }
        }
        if let Some(f) = self.fork_stands(&m.header.publisher.0).await? {
            return Err(publisher_forked(&f));
        }
        self.accept_held(m, r, signer, None, None).await
    }

    /// Take a manifest that held catalog `number` of its publisher lists
    /// (ADR-0059 amendment 1 item 5): the leaf names exactly this manifest,
    /// and no report of another root uses the manifest's own sequence (item 4).
    pub async fn accept_manifest_in_catalog(
        &self,
        manifest: &[u8],
        number: u64,
    ) -> Result<(StatusCode, Value), ApiError> {
        let m = decode_manifest(manifest)?;
        // Held until the version is written: no catalog is set aside meanwhile.
        // The erase lock first, then the publisher's (review CAT-ERASE-R1 H1).
        let _no_erase = self.erase_lock.read().await;
        let _one = self.publisher_lock(&m.header.publisher.0).read().await;
        // No version of a removed dataset comes back through an older
        // catalog (ADR-0059 amendment 4 item 3, review CAT-TOMB-R1 H1).
        if let Some(n) = self.removed(&m.header.publisher.0, &m.header.dataset)? {
            return Err(dataset_removed(n));
        }
        if let Some(f) = self.fork_stands(&m.header.publisher.0).await? {
            return Err(publisher_forked(&f));
        }
        let h = &m.header;
        let (signer, anchor) = {
            let cs = self.catalogs.read().map_err(|_| internal("lock"))?;
            let c = cs
                .get(&h.publisher.0)
                .and_then(|n| n.get(&number))
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::CONFLICT,
                        "bulk_catalog_unknown",
                        format!("this node holds no catalog {number} of this publisher: upload it first"),
                    )
                })?;
            (c.signer, c.anchor.clone())
        };
        // The newest catalog is in memory; an older one is rebuilt from its
        // files, at most `MAX_REBUILDS` at a time (rare: uploads follow the
        // newest catalog; review of the build, M1).
        let _permit = if self.catalog_ready(&h.publisher.0, number).is_some() {
            None
        } else {
            Some(
                self.rebuilds
                    .acquire()
                    .await
                    .map_err(|_| internal("rebuilds"))?,
            )
        };
        let (body, report) = self
            .catalog_body(&h.publisher.0, number)
            .map_err(|e| internal(format!("catalog: {e}")))?
            .ok_or_else(|| internal("catalog gone"))?;
        let i = body
            .catalog
            .find(&h.dataset)
            .filter(|i| body.catalog.leaves[*i].names(h))
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "bulk_not_in_catalog",
                    format!("catalog {number} does not list this manifest"),
                )
            })?;
        let lp = body
            .catalog
            .leaf_proof_in(&body.tree, i)
            .ok_or_else(|| internal("leaf proof"))?;
        let header = body.catalog.header.clone();
        let own = onyx_bulk::anchor::payload(h, &signer);
        let used = self.sequence_used(&own.key, own.sequence).await?;
        if used != Hash32::ZERO && used != own.digest() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "bulk_conflicting_anchor",
                "the signed book holds another report under this manifest's own sequence (another root for this version)",
            ));
        }
        let digest = report.payload.digest();
        let fresh = self
            .rechecked
            .lock()
            .ok()
            .and_then(|c| c.get(&digest).copied())
            .is_some_and(|(t, held)| held && (self.now)().saturating_sub(t) < ANCHOR_RECHECK_SECS);
        self.accept_held(
            m,
            report,
            signer,
            Some(Box::new((header, lp))),
            fresh.then_some(anchor),
        )
        .await
    }

    /// The digest the book holds under `(key, sequence)` at the finalized
    /// block (zero: none).
    async fn sequence_used(&self, key: &Hash32, sequence: u64) -> Result<Hash32, ApiError> {
        read_sequence(self.rpc.as_ref(), &self.pin.book, key, sequence)
            .await
            .map_err(chain_unavailable)
    }

    async fn accept_held(
        &self,
        m: Manifest,
        r: SignedReport,
        signer: [u8; 20],
        catalog: Option<Box<(CatalogHeader, LeafProof)>>,
        known: Option<AnchorView>,
    ) -> Result<(StatusCode, Value), ApiError> {
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
        // A held manifest for this version under another report is read at
        // the same finalized block as this one (review CAT-FIX-R1 M1).
        let held_other = {
            let ds = self.datasets.read().map_err(|_| internal("lock"))?;
            ds.get(&key)
                .and_then(|d| d.versions.get(h.version as usize - 1))
                .filter(|o| o.report != r)
                .map(|o| {
                    let b = o.anchor.block_number;
                    (o.manifest.header.root(), o.report.clone(), o.signer, b)
                })
        };
        // Through a catalog, the same root whose held report was read as
        // holding within the recheck window: nothing to change, no chain read
        // (a refresh uploads every unchanged version again under its newest
        // catalog). A manifest's own new report always reads the chain: it is
        // how a publisher re-signs after a compromise the cache has not seen
        // yet (CL5-6).
        if let Some((other_root, report, _, _)) = held_other.as_ref().filter(|_| catalog.is_some())
        {
            let fresh = self
                .rechecked
                .lock()
                .ok()
                .and_then(|c| c.get(&report.payload.digest()).copied())
                .is_some_and(|(t, held)| held && now.saturating_sub(t) < ANCHOR_RECHECK_SECS);
            if *other_root == root && fresh {
                let ds = self.datasets.read().map_err(|_| internal("lock"))?;
                if let Some(o) = ds
                    .get(&key)
                    .and_then(|d| d.versions.get(h.version as usize - 1))
                {
                    return Ok((StatusCode::OK, self.missing_json(&o.manifest)));
                }
            }
        }
        let at = match (&known, &held_other) {
            (Some(_), None) => None,
            _ => Some(
                decision_block(self.rpc.as_ref(), self.pin.chain_id)
                    .await
                    .map_err(chain_unavailable)?,
            ),
        };
        let anchor = match (known, at) {
            (Some(a), _) => a,
            (None, Some(at)) => match self.read_at(&r, &signer, at).await?.state {
                AnchorState::Anchored(a) => a,
                AnchorState::Not(why) => {
                    self.refuse_key(why, &r, now);
                    return Err(not_anchored(why));
                }
            },
            (None, None) => return Err(internal("no block read")),
        };
        // Another root for this version: withdrawn for good (its key
        // compromised from before it, review CL5-R4), it and every later
        // version go and this one takes its place; still anchored, two
        // anchored manifests exist for one version (a fork, below); not
        // decided yet (an open dispute, a suspended key), wait (CAT-FIX-R1
        // H1). The same root under another report is re-anchored (CL5-6)
        // only when the held report no longer holds, so nobody moves a
        // version's anchor to another catalog that lists it while its own
        // holds (review of CAT-5, M4).
        let (withdraw, keep_anchor) = match (held_other, at) {
            (Some((other_root, report, other_signer, block)), Some(at)) => {
                if block > at.0 {
                    return Err(behind(at, block));
                }
                let x = self.read_at(&report, &other_signer, at).await?;
                let holds = matches!(x.state, AnchorState::Anchored(_));
                self.mark(report.payload.digest(), holds);
                match x.state {
                    _ if other_root == root => (None, holds),
                    AnchorState::Anchored(_) => (None, false),
                    AnchorState::Not(_) if x.withdrawn => (Some(other_root), false),
                    AnchorState::Not(why) => {
                        return Err(ApiError::new(
                            StatusCode::CONFLICT,
                            "bulk_conflicting_anchor",
                            format!(
                                "version {} is held with another root whose report the book has not decided on ({why}); try again later",
                                h.version
                            ),
                        )
                        .with_header("retry-after", "60".into()))
                    }
                }
            }
            _ => (None, false),
        };
        let mut ds = self.datasets.write().map_err(|_| internal("lock"))?;
        self.check_quota(&ds, &key, h.byte_count)?;
        let d = ds.entry(key.clone()).or_default();
        let dir = self.dataset_dir(&key.0, &key.1);
        if let Some(other_root) = withdraw {
            let i = h.version as usize - 1;
            if d.versions.get(i).map(|o| o.manifest.header.root()) == Some(other_root) {
                // Newest first, memory after the files: a crash leaves
                // versions 1..k on disk (review of CAT-5, M2).
                for w in d.versions[i..].iter().rev() {
                    let h = &w.manifest.header;
                    move_version_aside(&dir, h.version, &h.root())
                        .map_err(|e| internal(format!("bulk store: {e}")))?;
                }
                d.versions.truncate(i);
                if let Some(prev) = d.versions.last() {
                    m.check_successor(&prev.manifest).map_err(|e| {
                        ApiError::new(StatusCode::CONFLICT, "bulk_not_successor", e.to_string())
                    })?;
                }
            }
        }
        if let Some(other) = d.versions.get_mut(h.version as usize - 1) {
            if other.manifest.header.root() == root && other.report != r {
                if keep_anchor {
                    return Ok((StatusCode::OK, self.missing_json(&other.manifest)));
                }
                write_version(&dir, &m, &r, &anchor, catalog.as_deref())?;
                self.mark(r.payload.digest(), true);
                other.report = r;
                other.signer = signer;
                other.anchor = anchor;
                other.catalog = catalog;
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
        write_version(&dir, &m, &r, &anchor, catalog.as_deref())?;
        let body = self.missing_json(&m);
        self.mark(r.payload.digest(), true);
        d.versions.push(Held {
            manifest: m,
            report: r,
            signer,
            anchor,
            catalog,
        });
        Ok((StatusCode::CREATED, body))
    }

    fn check_quota(
        &self,
        ds: &BTreeMap<Key, Dataset>,
        key: &Key,
        bytes: u64,
    ) -> Result<(), ApiError> {
        let cats = self
            .catalog_bytes
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default();
        let mut publisher_bytes = bytes.saturating_add(cats.get(&key.0).copied().unwrap_or(0));
        let mut total = bytes.saturating_add(cats.values().sum());
        let mut count = 1usize;
        // An erased dataset's bytes are gone; it still counts toward the cap
        // (ADR-0059 amendment 4 item 5a; review CAT-ERASE-R1 L4).
        let erased = self.erased_datasets();
        for (k, d) in ds {
            let b = if erased.contains(k) {
                0
            } else {
                d.latest().map_or(0, |h| h.manifest.header.byte_count)
            };
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
        // A root of a record moved aside after erasure may be any held
        // version's (review CAT-ERASE-R1 M4): the publisher restores them all.
        let h = if h.manifest.shards.iter().any(|e| e.root == *root) || !self.restorable(root) {
            h
        } else {
            d.versions
                .iter()
                .rev()
                .find(|v| v.manifest.shards.iter().any(|e| e.root == *root))
                .unwrap_or(h)
        };
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
        // No erase pass from here to the stored file (review CAT-ERASE-R1 H1),
        // taken only once the body is read and checked, so a slow upload never
        // holds a pass, and every upload behind it, for its body (review of the
        // build, finding 7); the entry is read again under it.
        let _no_erase = self.erase_lock.read().await;
        self.entry_of(publisher, dataset, root)?;
        // Stored even when the root is held already: every upload is checked
        // against this dataset's entry and schema (review CL5-R3), and the
        // put of identical bytes changes nothing.
        // The answer says "stored" only when the file is in the hot store under
        // this root and the tier is tracked (review BULK-LOSS-1).
        let tiers = self.tiers.clone();
        let tiers_file = self.dir.join("tiers.json");
        let want = *root;
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let key = tiers.put(Kind::Shard, &bytes).map_err(|e| e.to_string())?;
            if key != want || !tiers.hot().has(Kind::Shard, &want) || tiers.tier(&want).is_none() {
                return Err(format!("the shard did not read back under {want}"));
            }
            tiers.save(&tiers_file).map_err(|e| e.to_string())
        })
        .await
        .map_err(|_| internal("store task"))?
        .map_err(|err| internal(format!("store: {err}")))?;
        if let Ok(mut t) = self.touched.lock() {
            t.insert(*root, (self.now)());
        }
        // Stored again: a held manifest names it, so it is served again.
        self.unerase(root);
        eprintln!(
            "onyx-node: bulk: stored shard {root} of {publisher}/{dataset} ({} bytes{})",
            e.bytes,
            if already { ", held already" } else { "" }
        );
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

    /// The held manifest with this root and its signed report, whatever its
    /// dataset or version (the storage network's peers fetch it, ADR-0060;
    /// they check its root and anchor themselves).
    pub fn manifest_record(&self, root: &Hash32) -> Option<(Manifest, SignedReport)> {
        let ds = self.datasets.read().ok()?;
        ds.values()
            .flat_map(|d| d.versions.iter())
            .find(|h| h.manifest.header.root() == *root)
            .map(|h| (h.manifest.clone(), h.report.clone()))
    }

    /// Whether a shard with this root is held (hot or warm).
    pub fn holds_shard(&self, root: &Hash32) -> bool {
        self.tiers.tier(root).is_some()
    }

    /// A held shard's file, checked against its root as [`Self::shard_file`]
    /// does and rewound, with its length: `GET /v1/bulk/shards/{root}` sends
    /// it from this handle in frames instead of holding it in memory (THREATS
    /// SERVE-WRITE-1).
    pub async fn shard_open(&self, root: &Hash32) -> Option<(std::fs::File, u64)> {
        self.tiers.tier(root)?;
        if self.erased_root(root) {
            return None;
        }
        let (tiers, r) = (self.tiers.clone(), *root);
        tokio::task::spawn_blocking(move || tiers.open_checked(Kind::Shard, &r))
            .await
            .ok()
            .flatten()
    }

    /// A held shard file, read back through the CAS (which rechecks its
    /// root); `None` when not held or damaged on disk.
    pub async fn shard_file(&self, root: &Hash32) -> Option<Vec<u8>> {
        self.tiers.tier(root)?;
        if self.erased_root(root) {
            return None;
        }
        let (tiers, r) = (self.tiers.clone(), *root);
        tokio::task::spawn_blocking(move || tiers.get(Kind::Shard, &r))
            .await
            .ok()
            .flatten()
    }

    /// Store a shard file fetched from a peer: exactly the checks of a
    /// publisher's upload (`accept_shard`).
    pub async fn put_shard(
        &self,
        publisher: &str,
        dataset: &str,
        root: &Hash32,
        file: Vec<u8>,
    ) -> Result<(StatusCode, Value), ApiError> {
        self.accept_shard(
            publisher,
            dataset,
            root,
            &HeaderMap::new(),
            Body::from(file),
        )
        .await
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

    /// Record a chain read of the report with this digest.
    fn mark(&self, digest: Hash32, held: bool) {
        if let Ok(mut c) = self.rechecked.lock() {
            c.insert(digest, ((self.now)(), held));
        }
    }

    /// The version to serve: the newest whose anchor still holds, each read
    /// again at most [`ANCHOR_RECHECK_SECS`] (host clock) after the last read.
    /// A version the book no longer holds (its key compromised from before it,
    /// or its fact disputed) is skipped, so the honest earlier versions keep
    /// serving (review CL5-R4); none holding is a refusal.
    async fn serving(&self, key: &Key) -> Result<Held, ApiError> {
        type Version = (Hash32, SignedReport, [u8; 20]);
        if let Some(n) = self.removed(&key.0, &key.1)? {
            return Err(self.gone(&key.0, &key.1, n));
        }
        let (versions, in_catalog): (Vec<Version>, bool) = {
            let ds = self.datasets.read().map_err(|_| internal("lock"))?;
            let d = ds.get(key).ok_or_else(unknown_dataset)?;
            if d.forked.is_some() {
                return Err(forked(d));
            }
            let v = d
                .versions
                .iter()
                .rev()
                .map(|h| (h.manifest.header.root(), h.report.clone(), h.signer))
                .collect();
            (v, d.versions.iter().any(|h| h.catalog.is_some()))
        };
        if in_catalog {
            if let Some(f) = self.fork_stands(&key.0).await? {
                return Err(catalog_forked(&f));
            }
        }
        let mut first_refusal = None;
        for (root, report, signer) in versions {
            let now = (self.now)();
            let digest = report.payload.digest();
            let last = self
                .rechecked
                .lock()
                .ok()
                .and_then(|c| c.get(&digest).copied());
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
                        self.mark(digest, true);
                        true
                    }
                    AnchorState::Not(why) => {
                        first_refusal.get_or_insert(why);
                        self.mark(digest, false);
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
                        catalog: h.catalog.clone(),
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

    /// `GET /v1/bulk`: every dataset, or with a [`ListQuery`] the datasets
    /// whose `publisher/dataset` holds every search word, one page at a time
    /// in id order. `total` counts every match; `next` is the cursor of the
    /// page after (`null` on the last).
    pub fn list(&self, q: &ListQuery) -> Result<Value, ApiError> {
        // Publishers with a catalog fork mark (taken before, never under, the
        // datasets lock).
        let forked_pubs: std::collections::BTreeSet<String> = self
            .forks
            .read()
            .map_err(|_| internal("lock"))?
            .keys()
            .cloned()
            .collect();
        // Removed datasets are left out (ADR-0059 amendment 4 item 3).
        let removals = self.removals()?;
        // Records waiting and datasets erased (item 5a), before any other lock.
        let erase = self.erase_counts();
        // Each publisher's newest held catalog, so a watch sees a refresh
        // that stopped (launch checklist LC-17): small, one per publisher.
        let catalogs: Vec<Value> = self
            .catalogs
            .read()
            .map_err(|_| internal("lock"))?
            .iter()
            .filter_map(|(p, m)| {
                let (n, h) = m.last_key_value()?;
                Some(json!({
                    "publisher": p,
                    "number": n,
                    "root": h.header.root().to_string(),
                    "createdUnix": h.header.created_unix,
                    "leaves": h.header.leaf_count,
                    "removed": h.body.as_ref().map(|b| b.removed.len()),
                    "erase": erase.get(p).map(|(w, e)| json!({"waiting": w, "erased": e})),
                }))
            })
            .collect();
        let ds = self.datasets.read().map_err(|_| internal("lock"))?;
        let matches = |p: &str, d: &str| {
            let id = format!("{p}/{d}");
            if !q.words.iter().all(|w| id.contains(w.as_str())) {
                return false;
            }
            let words = id_words(&id);
            q.any.iter().all(|g| g.iter().any(|t| t.matches(&words)))
        };
        let (mut total, mut more) = (0usize, false);
        let mut page = Vec::new();
        for ((p, d), x) in ds.iter() {
            let Some(h) = x.latest() else { continue };
            if !matches(p, d) || removals.get(p).is_some_and(|b| b.removed.contains(d)) {
                continue;
            }
            total += 1;
            if q.after.as_ref().is_some_and(|a| (p, d) <= (&a.0, &a.1)) {
                continue;
            }
            if q.limit.is_some_and(|n| page.len() >= n) {
                more = true;
                continue;
            }
            let cat_fork =
                forked_pubs.contains(p) && x.versions.iter().any(|h| h.catalog.is_some());
            page.push((p, d, (h, x.forked.is_some() || cat_fork)));
        }
        let next = more
            .then(|| page.last().map(|(p, d, _)| format!("{p}/{d}")))
            .flatten();
        let items: Vec<Value> = page
            .into_iter()
            .map(|(p, d, (h, forked))| {
                let hd = &h.manifest.header;
                let stored = h
                    .manifest
                    .shards
                    .iter()
                    .filter(|e| self.tiers.tier(&e.root).is_some())
                    .count();
                json!({
                    "publisher": p, "dataset": d, "version": hd.version,
                    "root": hd.root().to_string(), "rows": hd.row_count, "bytes": hd.byte_count,
                    "shards": hd.shard_count, "stored_shards": stored,
                    "key_min": hd.key_min.to_string(), "key_max": hd.key_max.to_string(),
                    "license": hd.rights.license, "forked": forked,
                    "created_unix": hd.created_unix,
                })
            })
            .collect();
        Ok(json!({"datasets": items, "total": total, "next": next, "catalogs": catalogs}))
    }

    /// `GET /v1/bulk/{publisher}/{dataset}`.
    pub fn info(&self, publisher: &str, dataset: &str) -> Result<Value, ApiError> {
        // The mark is read before, never under, the datasets lock.
        let mark = self.fork_marked(publisher);
        let ds = self.datasets.read().map_err(|_| internal("lock"))?;
        let d = ds
            .get(&(publisher.to_string(), dataset.to_string()))
            .ok_or_else(unknown_dataset)?;
        if d.forked.is_some() {
            return Err(forked(d));
        }
        if let Some(f) = mark.filter(|_| d.versions.iter().any(|h| h.catalog.is_some())) {
            return Err(catalog_forked(&f));
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
            "anchor": anchor_json(h),
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
        let held = self.serving(&key).await?;
        let anchor = anchor_json(&held);
        let manifest = held.manifest;
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
            "anchor": anchor,
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

fn catalog_status(h: &CatalogHeader) -> Value {
    json!({
        "publisher": h.publisher.0,
        "number": h.number,
        "root": h.root().to_string(),
        "leaves": h.leaf_count,
    })
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

/// The answer's `anchor`: the report's anchor view and, for a version a
/// catalog lists, `catalog` (its header and the leaf proof).
fn anchor_json(h: &Held) -> Value {
    let mut a = h.anchor.to_json(&h.report);
    if let Some(c) = &h.catalog {
        a["catalog"] = catalog_json(&c.0, &c.1);
    }
    a
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

/// ADR-0059 amendment 4 item 3: the publisher removed the dataset with a
/// tombstone in catalog `n`, its newest catalog this node holds.
fn dataset_removed(n: u64) -> ApiError {
    ApiError::new(
        StatusCode::GONE,
        "bulk_dataset_removed",
        format!("the publisher removed this dataset in its catalog {n}"),
    )
    .with_field("catalog", json!(n))
}

/// `GET /v1/bulk/shards/{root}` for a root this node erased (ADR-0059
/// amendment 4 item 5a).
fn shard_removed() -> ApiError {
    ApiError::new(
        StatusCode::GONE,
        "bulk_shard_removed",
        "the publisher removed the dataset of this shard and this node erased it",
    )
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

/// An upload refused while the publisher's catalogs are forked (THREATS CAT-6).
fn publisher_forked(f: &Fork) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "bulk_publisher_forked",
        format!(
            "this publisher's catalogs are forked ({}): nothing of it is taken until the book withdraws either side",
            f.why
        ),
    )
    .with_field("fork", fork_json(f))
}

/// A catalog dataset not served while its publisher's catalogs are forked.
fn catalog_forked(f: &Fork) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "bulk_dataset_forked",
        format!(
            "not served: this publisher's catalogs are forked ({}) until the book withdraws either side",
            f.why
        ),
    )
    .with_field("fork", fork_json(f))
}

/// The fork's two sides: both headers and reports, for anyone to check.
fn fork_json(f: &Fork) -> Value {
    let side = |(h, r): &(CatalogHeader, SignedReport)| {
        json!({
            "number": h.number,
            "root": h.root().to_string(),
            "header": hex0x(&borsh::to_vec(h).unwrap_or_default()),
            "report": hex0x(&borsh::to_vec(r).unwrap_or_default()),
        })
    };
    json!({"why": f.why, "held": side(&f.held), "other": side(&f.other)})
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

/// Datasets per `GET /v1/bulk` page at most.
pub const MAX_LIST_PAGE: usize = 500;
/// Characters of a `GET /v1/bulk` search at most.
pub const MAX_SEARCH_CHARS: usize = 64;
/// Groups of an `any` search at most.
pub const MAX_ANY_GROUPS: usize = 8;
/// Terms of an `any` search at most, in all groups.
pub const MAX_ANY_TERMS: usize = 200;
/// Characters of one `any` term at most.
pub const MAX_ANY_TERM_CHARS: usize = 48;

/// One `any` term: whole words of the id in a row (`cost-basis` is the words
/// cost and basis next to each other); a last `*` lets the last word be a
/// prefix (`realiz*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnyTerm {
    /// Lowercase words.
    pub words: Vec<String>,
    /// The last word may be a prefix.
    pub prefix: bool,
}

impl AnyTerm {
    fn matches(&self, id: &[&str]) -> bool {
        let n = self.words.len();
        n > 0
            && id.len() >= n
            && id.windows(n).any(|w| {
                w.iter().zip(&self.words).enumerate().all(|(i, (a, b))| {
                    if self.prefix && i == n - 1 {
                        a.starts_with(b.as_str())
                    } else {
                        *a == b
                    }
                })
            })
    }
}

/// The words of a dataset id for `any` (split at anything but a-z and 0-9).
fn id_words(id: &str) -> Vec<&str> {
    id.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect()
}

/// `GET /v1/bulk` query: `q` (search words, each must be in
/// `publisher/dataset`; letters, digits, `.`, `-`, `_`, `/` and spaces),
/// `any` (groups split by `;`, terms by `,`: every group must have a term
/// among the id's whole words, [`AnyTerm`]), `limit` (1..=[`MAX_LIST_PAGE`])
/// and `after` (the `next` cursor of the page before). None given: every
/// dataset, as before.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ListQuery {
    /// Lowercase search words.
    pub words: Vec<String>,
    /// `any` groups: each must match.
    pub any: Vec<Vec<AnyTerm>>,
    /// Page size; `None` lists every match.
    pub limit: Option<usize>,
    /// Only ids after this `(publisher, dataset)`.
    pub after: Option<(String, String)>,
}

/// `+` as a space and `%XX` escapes; `None` for a broken escape or non-UTF-8.
fn url_decode(v: &str) -> Option<String> {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(h, 16).ok()?);
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Parse the `GET /v1/bulk` query (see [`ListQuery`]).
pub fn parse_list_query(q: Option<&str>) -> Result<ListQuery, ApiError> {
    let mut out = ListQuery::default();
    let mut seen: Vec<&str> = Vec::new();
    for pair in q.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if seen.contains(&k) {
            return Err(bad_request(format!("{k} given twice")));
        }
        seen.push(k);
        let v = url_decode(v).ok_or_else(|| bad_request(format!("{k}: a broken % escape")))?;
        match k {
            "q" => {
                let ok = |c: char| c.is_ascii_alphanumeric() || " .-_/".contains(c);
                if v.chars().count() > MAX_SEARCH_CHARS || !v.chars().all(ok) {
                    return Err(bad_request(format!(
                        "q: at most {MAX_SEARCH_CHARS} letters, digits, spaces and . - _ /"
                    )));
                }
                out.words = v
                    .split(' ')
                    .filter(|w| !w.is_empty())
                    .map(str::to_ascii_lowercase)
                    .collect();
            }
            "limit" => {
                out.limit = Some(
                    v.parse::<usize>()
                        .ok()
                        .filter(|n| (1..=MAX_LIST_PAGE).contains(n))
                        .ok_or_else(|| bad_request(format!("limit must be 1..={MAX_LIST_PAGE}")))?,
                )
            }
            "any" => {
                let bad = || {
                    bad_request(format!(
                        "any: at most {MAX_ANY_GROUPS} groups split by ; of at most {MAX_ANY_TERMS} terms split by , (letters, digits and -, at most {MAX_ANY_TERM_CHARS} each, a last * for a prefix)"
                    ))
                };
                let mut terms = 0usize;
                for g in v.split(';') {
                    let mut group = Vec::new();
                    for t in g.split(',').filter(|t| !t.is_empty()) {
                        let (body, prefix) = match t.strip_suffix('*') {
                            Some(b) => (b, true),
                            None => (t, false),
                        };
                        let words: Vec<String> =
                            body.split('-').map(str::to_ascii_lowercase).collect();
                        if t.len() > MAX_ANY_TERM_CHARS
                            || words.iter().any(|w| {
                                w.is_empty() || !w.chars().all(|c| c.is_ascii_alphanumeric())
                            })
                        {
                            return Err(bad());
                        }
                        group.push(AnyTerm { words, prefix });
                        terms += 1;
                    }
                    if group.is_empty() {
                        return Err(bad());
                    }
                    out.any.push(group);
                }
                if out.any.len() > MAX_ANY_GROUPS || terms > MAX_ANY_TERMS {
                    return Err(bad());
                }
            }
            "after" => {
                let (p, d) = v
                    .split_once('/')
                    .filter(|(p, d)| {
                        onyx_core::ids::is_dotted_id(p) && onyx_core::ids::is_dotted_id(d)
                    })
                    .ok_or_else(|| bad_request("after: publisher/dataset, from the page's next"))?;
                out.after = Some((p.to_string(), d.to_string()));
            }
            _ => return Err(bad_request(format!("unknown query parameter {k}"))),
        }
    }
    Ok(out)
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

/// The storage network's peer reads (ADR-0060), merged OUTSIDE the server
/// guard like the uploads: a held manifest by root and a held shard file by
/// root (up to 64 MiB, so outside the 12 s deadline).
pub fn peer_routes(store: Arc<BulkStore>) -> Router {
    Router::new()
        .route("/v1/bulk/manifests/{root}", get(get_manifest))
        .route(
            "/v1/bulk/catalogs/{publisher}/files",
            get(get_catalog_files),
        )
        .route("/v1/bulk/catalogs/{publisher}/{n}", get(get_catalog))
        .route("/v1/bulk/shards/{root}", get(get_shard))
        .with_state(store)
}

/// The upload routes, merged OUTSIDE the server guard (own limits).
pub fn upload_routes(store: Arc<BulkStore>) -> Router {
    Router::new()
        .route("/v1/bulk/manifests", post(post_manifest))
        .route("/v1/bulk/catalogs", post(post_catalog))
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
        store.list(&parse_list_query(q.as_deref())?)
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
    let r = async {
        limited(&store, client)?;
        let (p, d) = path_ids(path)?;
        store.info_checked(&p, &d).await
    }
    .await;
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
        let v: Value = serde_json::from_slice(&b)
            .map_err(|_| bad_request("body: JSON {manifest, report} or {manifest, catalog}"))?;
        let m = onyx_bulk::wire::hex_field(&v["manifest"], "manifest")
            .map_err(|e| bad_request(e.to_string()))?;
        // A removed dataset takes no manifest on this route, old or new
        // (ADR-0059 amendment 4 item 3); a storage node's own fetch of a
        // deal's manifest does not come this way.
        let h = decode_manifest(&m)?.header;
        if let Some(n) = s.removed(&h.publisher.0, &h.dataset)? {
            return Err(dataset_removed(n));
        }
        if let Some(n) = v.get("catalog").filter(|c| !c.is_null()) {
            let n = n
                .as_u64()
                .ok_or_else(|| bad_request("catalog: the number of a held catalog"))?;
            return s.accept_manifest_in_catalog(&m, n).await;
        }
        let r = onyx_bulk::wire::hex_field(&v["report"], "report")
            .map_err(|e| bad_request(e.to_string()))?;
        s.accept_manifest_as(&m, &r, true).await
    })
    .await
}

async fn post_catalog(State(store): State<Arc<BulkStore>>, body: Body) -> Response {
    let s = store.clone();
    upload(&store, || async move {
        let b = axum::body::to_bytes(body, MAX_CATALOG_BODY)
            .await
            .map_err(|_| {
                ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "body_too_large",
                    "catalog body over its cap",
                )
            })?;
        let v: Value =
            serde_json::from_slice(&b).map_err(|_| bad_request("body: JSON {catalog, report}"))?;
        let c = onyx_bulk::wire::hex_field(&v["catalog"], "catalog")
            .map_err(|e| bad_request(e.to_string()))?;
        let r = onyx_bulk::wire::hex_field(&v["report"], "report")
            .map_err(|e| bad_request(e.to_string()))?;
        s.accept_catalog(&c, &r).await
    })
    .await
}

/// A shared answer sent in [`FILE_FRAME`] slices of one copy: hyper never
/// holds more than its buffer of it per connection, whatever its write
/// strategy (SERVE-WRITE-2).
struct SharedBody {
    bytes: Answer,
    at: usize,
}

impl http_body::Body for SharedBody {
    type Data = axum::body::Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = &mut *self;
        if this.at >= this.bytes.len() {
            return std::task::Poll::Ready(None);
        }
        let end = this.bytes.len().min(this.at.saturating_add(FILE_FRAME));
        let chunk = this.bytes.slice(this.at..end);
        this.at = end;
        std::task::Poll::Ready(Some(Ok(http_body::Frame::data(chunk))))
    }

    fn is_end_stream(&self) -> bool {
        self.at >= self.bytes.len()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(self.bytes.len().saturating_sub(self.at) as u64)
    }
}

/// Encoded JSON at its own size (`to_vec` doubles as it grows: the slack
/// would be kept and counted short; review SERVE-WRITE-R3 finding 3).
fn json_bytes(v: &Value) -> Result<axum::body::Bytes, ApiError> {
    let mut b = serde_json::to_vec(v).map_err(|_| internal("answer encoding"))?;
    b.shrink_to_fit();
    Ok(axum::body::Bytes::from(b))
}

/// A 200 JSON answer from shared bytes, not stored by caches.
fn shared_json(bytes: Answer) -> Response {
    let mut r = (StatusCode::OK, Body::new(SharedBody { bytes, at: 0 })).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// `GET /v1/bulk/catalogs/{publisher}/{n}`: `{publisher, number, root,
/// catalog, report}` (Borsh hex), the body `POST /v1/bulk/catalogs` takes.
/// Encoded once and shared while the held catalog stays the same
/// (SERVE-WRITE-2).
async fn get_catalog(
    State(store): State<Arc<BulkStore>>,
    client: Option<Extension<ClientKey>>,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Response {
    let r = async {
        limited(&store, client)?;
        let Path((p, n)) = path.map_err(|e| bad_request(e.body_text()))?;
        let n: u64 = n.parse().map_err(|_| bad_request("n: a catalog number"))?;
        let key = format!("catalog/{p}/{n}");
        let not_held = || {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "bulk_unknown_catalog",
                "this node holds no such catalog",
            )
        };
        let check = store.catalog_check(&p, n).ok_or_else(not_held)?;
        if let Some(a) = store.answer(&key, check) {
            return Ok(a);
        }
        let _turn = store.answer_turn(&key).await;
        // Read again: the held catalog may have changed while this waited.
        let check = store.catalog_check(&p, n).ok_or_else(not_held)?;
        if let Some(a) = store.answer(&key, check) {
            return Ok(a);
        }
        // The newest, or the one rebuilt last, at once; an older catalog is
        // rebuilt from its files a few at a time, off the async threads, the
        // permit held by the rebuild itself (review CAT-DIFF-R1 L3, review
        // of the build M2).
        let found = match store.catalog_ready(&p, n) {
            Some((b, rep)) => Ok(Some((b.catalog.clone(), rep))),
            None => {
                let permit = store
                    .rebuilds
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| internal("rebuilds"))?;
                let (s, q) = (store.clone(), p.clone());
                tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    s.catalog_record(&q, n)
                })
                .await
                .map_err(|_| internal("rebuild task"))?
            }
        };
        let (c, rep) = found
            .map_err(|e| internal(format!("catalog: {e}")))?
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "bulk_unknown_catalog",
                    "this node holds no such catalog",
                )
            })?;
        let v = json!({
            "publisher": p,
            "number": n,
            "root": c.header.root().to_string(),
            "catalog": hex0x(&borsh::to_vec(&c).unwrap_or_default()),
            "report": hex0x(&borsh::to_vec(&rep).unwrap_or_default()),
        });
        let body = json_bytes(&v)?;
        Ok::<_, ApiError>(store.keep_answer(key, (c.header.root(), rep.hash()), body))
    }
    .await;
    match r {
        Ok(b) => shared_json(b),
        Err(e) => e.into_response(),
    }
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
        if let Some(n) = s.removed(&p, &d)? {
            return Err(dataset_removed(n));
        }
        s.accept_shard(&p, &d, &root, &headers, body).await
    })
    .await
}

fn root_of(p: Result<Path<String>, PathRejection>) -> Result<Hash32, ApiError> {
    let Path(root) = p.map_err(|e| bad_request(e.body_text()))?;
    let (root, canonical) = crate::parse_hash(&root, "root")?;
    if !canonical {
        return Err(bad_request("root must be 0x and 64 lowercase hex digits"));
    }
    Ok(root)
}

/// `GET /v1/bulk/manifests/{root}`: `{root, manifest, report}` (Borsh hex),
/// the body `POST /v1/bulk/manifests` takes. Encoded once and shared while
/// the held report stays the same (SERVE-WRITE-2).
async fn get_manifest(
    State(store): State<Arc<BulkStore>>,
    client: Option<Extension<ClientKey>>,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let r = async {
        limited(&store, client)?;
        let root = root_of(path)?;
        let not_held = || {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "bulk_unknown_manifest",
                "this node holds no manifest with this root",
            )
        };
        let key = format!("manifest/{root}");
        let check = store.manifest_check(&root).ok_or_else(not_held)?;
        if let Some(a) = store.answer(&key, check) {
            return Ok(a);
        }
        let _turn = store.answer_turn(&key).await;
        // Read again: the held report may have changed while this waited.
        let check = store.manifest_check(&root).ok_or_else(not_held)?;
        if let Some(a) = store.answer(&key, check) {
            return Ok(a);
        }
        let (m, rep) = store.manifest_record(&root).ok_or_else(not_held)?;
        let v = json!({
            "root": root.to_string(),
            "manifest": hex0x(&m.to_bytes()),
            "report": hex0x(&borsh::to_vec(&rep).unwrap_or_default()),
        });
        let body = json_bytes(&v)?;
        Ok::<_, ApiError>(store.keep_answer(key, (root, rep.hash()), body))
    }
    .await;
    match r {
        Ok(b) => shared_json(b),
        Err(e) => e.into_response(),
    }
}

/// Bytes per frame of a files answer: hyper asks for the next frame only once
/// its write buffer has room, so the answer's permit lasts until about the
/// last frame is written (review of the CAT-1 fix, finding 1).
const FILES_FRAME: usize = 256 << 10;
/// Seconds a files answer may take to send at least: then its bytes and its
/// permit are let go, and the body ends with an error when it is asked for
/// another frame. A reader that never reads again still holds its connection
/// slot, hyper's queued frames (about 0.7 MiB) and the kernel's send buffer
/// until the server's write stall timeout closes the connection
/// (`onyx_node::serve`, THREATS SERVE-WRITE-1).
pub const FILES_SEND_SECS: u64 = 60;
/// Bytes a second an honest reader is given at least: a larger answer gets
/// longer than [`FILES_SEND_SECS`] (review CAT-HIST-R4 finding 5).
pub const FILES_MIN_RATE: usize = 128 << 10;

/// How long an answer of `bytes` may take to send.
fn files_deadline(bytes: usize) -> std::time::Duration {
    std::time::Duration::from_secs(FILES_SEND_SECS.max((bytes / FILES_MIN_RATE) as u64 + 10))
}

/// A files answer being sent: its bytes, how far, and its permit.
struct Sending {
    data: Vec<u8>,
    total: usize,
    at: usize,
    expired: bool,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    /// Dropped on release: the deadline timer then ends early.
    done: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Sending {
    fn release(&mut self) {
        self.data = Vec::new();
        self.permit = None;
        self.done = None;
    }
}

/// The files route's body: [`FILES_FRAME`] bytes a frame, each a copy (a
/// frame hyper still queues never keeps the whole answer), the permit let go
/// with the last frame, or after [`FILES_SEND_SECS`] by a timer.
struct FilesBody(Arc<Mutex<Sending>>);

/// A files answer that took longer than [`FILES_SEND_SECS`].
#[derive(Debug)]
struct SendExpired;

impl std::fmt::Display for SendExpired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the answer took longer than {FILES_SEND_SECS} s to send")
    }
}

impl std::error::Error for SendExpired {}

impl FilesBody {
    fn new(data: Vec<u8>, permit: tokio::sync::OwnedSemaphorePermit) -> Self {
        let total = data.len();
        let (done, sent) = tokio::sync::oneshot::channel::<()>();
        let s = Arc::new(Mutex::new(Sending {
            data,
            total,
            at: 0,
            expired: false,
            permit: Some(permit),
            done: Some(done),
        }));
        let weak = Arc::downgrade(&s);
        let deadline = files_deadline(total);
        tokio::spawn(async move {
            // Sent (or dropped) before the deadline: nothing to do.
            if tokio::time::timeout(deadline, sent).await.is_ok() {
                return;
            }
            if let Some(s) = weak.upgrade() {
                if let Ok(mut s) = s.lock() {
                    if s.at < s.total {
                        s.expired = true;
                    }
                    s.release();
                }
            }
        });
        FilesBody(s)
    }
}

impl http_body::Body for FilesBody {
    type Data = axum::body::Bytes;
    type Error = SendExpired;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let Ok(mut s) = self.0.lock() else {
            return std::task::Poll::Ready(Some(Err(SendExpired)));
        };
        if s.expired {
            return std::task::Poll::Ready(Some(Err(SendExpired)));
        }
        if s.at >= s.total {
            s.release();
            return std::task::Poll::Ready(None);
        }
        let end = s.total.min(s.at.saturating_add(FILES_FRAME));
        let chunk = axum::body::Bytes::copy_from_slice(&s.data[s.at..end]);
        s.at = end;
        if s.at >= s.total {
            s.release();
        }
        std::task::Poll::Ready(Some(Ok(http_body::Frame::data(chunk))))
    }

    fn is_end_stream(&self) -> bool {
        self.0.lock().map_or(true, |s| s.at >= s.total)
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(
            self.0
                .lock()
                .map_or(0, |s| s.total.saturating_sub(s.at) as u64),
        )
    }
}

/// `GET /v1/bulk/catalogs/{publisher}/files?from=n`: the held catalogs from
/// n on as stored, Borsh `CatalogFiles` (ADR-0059 amendment 3 item 1), for a
/// client that checks a publisher's whole catalog history itself. Two answers
/// at a time, each holding its permit until its last frame or for at most
/// [`FILES_SEND_SECS`]; busy: 503 with `retry-after`.
async fn get_catalog_files(
    State(store): State<Arc<BulkStore>>,
    client: Option<Extension<ClientKey>>,
    path: Result<Path<String>, PathRejection>,
    RawQuery(q): RawQuery,
) -> Response {
    let r = async {
        limited(&store, client)?;
        let Path(p) = path.map_err(|e| bad_request(e.body_text()))?;
        let mut from = None;
        for kv in q
            .as_deref()
            .unwrap_or("")
            .split('&')
            .filter(|s| !s.is_empty())
        {
            match kv.split_once('=') {
                Some(("from", v)) if from.is_none() => {
                    from = Some(
                        v.parse::<u64>()
                            .map_err(|_| bad_request("from: a catalog number"))?,
                    )
                }
                _ => return Err(bad_request("the only parameter is from (a catalog number)")),
            }
        }
        let from = from.ok_or_else(|| bad_request("from: a catalog number"))?;
        let permit = store.file_reads.clone().try_acquire_owned().map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "bulk_busy",
                "catalog files are being sent to others: try again",
            )
            .with_header("retry-after", "10".into())
        })?;
        let s = store.clone();
        let (found, permit) = tokio::task::spawn_blocking(move || {
            (s.catalog_files(&p, from, cat::MAX_FILES_BYTES), permit)
        })
        .await
        .map_err(|_| internal("file read task"))?;
        let bytes = found
            .map_err(|e| internal(format!("catalog files: {e}")))?
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "bulk_unknown_catalog",
                    "this node holds no such catalog",
                )
            })?;
        Ok::<_, ApiError>(FilesBody::new(bytes, permit))
    }
    .await;
    match r {
        Ok(body) => {
            let mut resp = (StatusCode::OK, Body::new(body)).into_response();
            let h = resp.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            resp
        }
        Err(e) => e.into_response(),
    }
}

/// Bytes per frame of a streamed shard file: hyper asks for the next frame
/// only once its write buffer (64 KiB, `onyx_node::serve::MAX_BUF`) has room,
/// so an answer being sent holds a few frames (about 0.2 MiB with tokio's
/// file buffer), never the file (THREATS SERVE-WRITE-1).
const FILE_FRAME: usize = 64 << 10;

/// A checked shard file sent from its open handle, [`FILE_FRAME`] bytes a
/// frame. A file that ends before its checked length fails the body, so the
/// reader sees a cut answer, never a short one.
struct FileBody {
    file: tokio::fs::File,
    left: u64,
}

impl FileBody {
    fn new(file: std::fs::File, len: u64) -> Self {
        FileBody {
            file: tokio::fs::File::from_std(file),
            left: len,
        }
    }
}

impl http_body::Body for FileBody {
    type Data = axum::body::Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        use tokio::io::AsyncRead;
        let this = &mut *self;
        if this.left == 0 {
            return std::task::Poll::Ready(None);
        }
        let want = usize::try_from(this.left).map_or(FILE_FRAME, |l| l.min(FILE_FRAME));
        // A fresh buffer a frame: nothing is kept between frames but tokio's
        // own read buffer.
        let mut buf = vec![0u8; want];
        let mut rb = tokio::io::ReadBuf::new(&mut buf);
        match std::pin::Pin::new(&mut this.file).poll_read(cx, &mut rb) {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Some(Err(e))),
            std::task::Poll::Ready(Ok(())) if rb.filled().is_empty() => {
                std::task::Poll::Ready(Some(Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the shard file ended before its checked length",
                ))))
            }
            std::task::Poll::Ready(Ok(())) => {
                let chunk = axum::body::Bytes::copy_from_slice(rb.filled());
                this.left -= chunk.len() as u64;
                std::task::Poll::Ready(Some(Ok(http_body::Frame::data(chunk))))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.left == 0
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(self.left)
    }
}

/// `GET /v1/bulk/shards/{root}`: the shard file's bytes. The reader checks
/// them against its own manifest entry (a node never trusts a peer's bytes).
/// The node checks the file against its root under a load permit, then sends
/// it from the same open file in [`FILE_FRAME`] frames with no permit held:
/// a slow reader holds a connection slot and about one frame, not the shard
/// (THREATS SERVE-WRITE-1).
async fn get_shard(
    State(store): State<Arc<BulkStore>>,
    client: Option<Extension<ClientKey>>,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let r = async {
        limited(&store, client)?;
        let root = root_of(path)?;
        if store.erased_root(&root) {
            return Err(shard_removed());
        }
        let _permit = store.loads.acquire().await.map_err(|_| internal("loads"))?;
        store.shard_open(&root).await.ok_or_else(|| {
            shard_missing("this node does not hold this shard (or it did not read back)")
        })
    }
    .await;
    match r {
        Ok((file, len)) => {
            let mut resp = (StatusCode::OK, Body::new(FileBody::new(file, len))).into_response();
            let h = resp.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            resp
        }
        Err(e) => e.into_response(),
    }
}

#[path = "bulk_erase.rs"]
mod erase;
pub use erase::{EraseConfig, EraseMode, ErasePass, MAX_SHARDS_PER_PASS, MIN_GRACE_DAYS};

#[cfg(test)]
#[path = "bulk_serve_tests.rs"]
pub(crate) mod tests;
