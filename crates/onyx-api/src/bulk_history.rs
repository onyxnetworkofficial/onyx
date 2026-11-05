//! A client check of a publisher's catalog history (ADR-0059 amendment 3,
//! THREATS CAT-1): `onyx-check --bulk <rows> --history <node>`.
//!
//! The node's stored catalogs are read from 0 (`GET
//! /v1/bulk/catalogs/{publisher}/files`) and each is rebuilt and checked as a
//! node checks it before it takes it ([`Walker`]); the asked dataset's entry,
//! and a rows answer anchored by its own report, are followed down their
//! own-report manifests until they meet a version the catalogs list (or
//! version 1); then, at one finalized block, every anchor, and for every key
//! of the publisher this check knows, that no other live report holds a
//! catalog number, a version of the dataset, the catalog after the newest
//! served or the version after the newest seen.

use crate::bulk_serve::{
    call_at, decision_block, read_anchor_at, read_digest_at, read_sequence_at, word, AnchorState,
    At, Standing,
};
use crate::roots::JsonRpc;
use crate::storage::{HttpGet, MAX_MANIFEST_ANSWER};
use borsh::BorshDeserialize;
use onyx_bulk::anchor::{key_id, sequence_for};
use onyx_bulk::catalog::{
    sequence_for_catalog, CatalogFile, CatalogFiles, CatalogLeaf, WalkError, Walker,
    MAX_CATALOG_BYTES, MAX_FILES_BYTES,
};
use onyx_bulk::manifest::Manifest;
use onyx_bulk::wire::{hex0x, hex_field, Checked};
use onyx_core::{keccak256, Hash32, SignedReport, SourceId, Value as Val};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

/// What the caller adds to a history check.
#[derive(Clone, Debug, Default)]
pub struct HistoryOpts {
    /// The node's base URL (`https://host`).
    pub node: String,
    /// Signer addresses of the publisher the caller knows (`--keys`).
    pub keys: Vec<[u8; 20]>,
    /// Scan `KeyRegistered` from this block (`--scan-keys-from`).
    pub scan_from: Option<u64>,
    /// Blocks per `eth_getLogs` query of the scan.
    pub logs_range: u64,
}

/// How a history check ended (amendment 3 item 4).
#[derive(Debug)]
pub enum HistoryOutcome {
    /// Every check held (exit 0): the report.
    Verified(Value),
    /// Refused (exit 1): whose fault ("publisher" or "node") and why.
    Refused(&'static str, String),
    /// Nothing decided yet (exit 3).
    Undecided(String),
}

impl HistoryOutcome {
    /// Exit code: 0 verified, 1 refused, 3 undecided.
    pub fn code(&self) -> i32 {
        match self {
            HistoryOutcome::Verified(_) => 0,
            HistoryOutcome::Refused(..) => 1,
            HistoryOutcome::Undecided(_) => 3,
        }
    }

    /// The `history` part of the JSON report.
    pub fn report(&self) -> Value {
        match self {
            HistoryOutcome::Verified(v) => v.clone(),
            HistoryOutcome::Refused(fault, why) => json!({
                "ok": false, "decided": true, "fault": fault, "error": why,
            }),
            HistoryOutcome::Undecided(why) => json!({
                "ok": false, "decided": false, "error": why, "retryLater": true,
            }),
        }
    }
}

/// What the report says a check without a key scan cannot catch.
pub const HISTORY_LIMIT: &str = "a key that signed nothing in this walk and that you did not name (--keys) or scan (--scan-keys-from) is not read: a second catalog, a second version root, or a newer catalog under such a key is not caught here; nodes that hold the original catalogs mark a rewrite forked";

enum Stop {
    Refused(&'static str, String),
    Undecided(String),
}

fn node(s: impl Into<String>) -> Stop {
    Stop::Refused("node", s.into())
}

fn publisher_fault(s: impl Into<String>) -> Stop {
    Stop::Refused("publisher", s.into())
}

fn undecided(s: impl Into<String>) -> Stop {
    Stop::Undecided(s.into())
}

/// `KeyRegistered(bytes32 indexed key, address indexed signer, string source,
/// string origin, bytes32 provenance, address by, string reason)`.
fn key_registered_topic() -> Hash32 {
    keccak256(b"KeyRegistered(bytes32,address,string,string,bytes32,address,string)")
}

/// Waits between tries of a busy GET, and how many tries (a fixed wait: the
/// node's `retry-after` is 1 s for the rate limit and 10 s for busy files).
pub const RETRY_SECS: u64 = 10;
const TRIES: u32 = 7;

/// GET `url`: the body on 200, `None` on 404; a busy node (429, 503) is tried
/// again after [`RETRY_SECS`] (the bulk rate limit, amendment 3 item 6).
async fn fetch(get: &dyn HttpGet, url: &str, max: usize) -> Result<Option<Vec<u8>>, Stop> {
    for _ in 0..TRIES {
        let (status, body) = get
            .get(url, max)
            .await
            .map_err(|e| undecided(format!("{url}: {e}")))?;
        match status {
            200 => return Ok(Some(body)),
            404 => return Ok(None),
            429 | 503 => tokio::time::sleep(Duration::from_secs(RETRY_SECS)).await,
            s => return Err(undecided(format!("{url}: status {s}"))),
        }
    }
    Err(undecided(format!("{url}: the node stayed busy")))
}

struct KeyInfo {
    signer: [u8; 20],
    found: BTreeSet<&'static str>,
    catalogs: Vec<u64>,
}

/// One version of the asked dataset the check saw: its root, and the first
/// catalog that lists it (`None`: only its own report).
struct Version {
    version: u32,
    root: Hash32,
    catalog: Option<u64>,
}

/// Where an own-report chain starts.
enum OwnStart<'a> {
    /// A rows answer anchored by its own report (checked by `check_bulk`).
    Answer(&'a Checked),
    /// A root and its version.
    Root(Hash32, u32),
}

/// One own-report manifest of a chain.
struct Own {
    version: u32,
    root: Hash32,
    report: SignedReport,
    signer: [u8; 20],
}

/// What the check has gathered.
struct State<'a> {
    rpc: &'a dyn JsonRpc,
    get: &'a dyn HttpGet,
    chain_id: u64,
    book: [u8; 20],
    base: String,
    publisher: SourceId,
    dataset: String,
    at: At,
    keys: BTreeMap<Hash32, KeyInfo>,
    versions: BTreeMap<u32, Version>,
}

impl State<'_> {
    fn add_key(&mut self, k: Hash32, signer: [u8; 20], how: &'static str) {
        self.keys
            .entry(k)
            .or_insert_with(|| KeyInfo {
                signer,
                found: BTreeSet::new(),
                catalogs: Vec::new(),
            })
            .found
            .insert(how);
    }

    /// The anchor of `report` at the block: anchored, or undecided. A report
    /// the book withdrew for good while the node still serves it means the
    /// node has not set it aside yet (CAT-5; review of the build, finding 4).
    async fn anchored(
        &self,
        report: &SignedReport,
        signer: &[u8; 20],
        what: &str,
    ) -> Result<bool, Stop> {
        let r = read_anchor_at(self.rpc, self.chain_id, &self.book, report, signer, self.at)
            .await
            .map_err(undecided)?;
        match r.state {
            AnchorState::Anchored(_) => Ok(true),
            AnchorState::Not(why) if r.withdrawn => Err(undecided(format!(
                "{what}: the book withdrew its report for good ({why}) and the node still serves it: it has not set it aside yet; check again later or on another node"
            ))),
            AnchorState::Not(why) => Err(undecided(format!(
                "{what}: its report does not read anchored at block {} yet ({why}); your chain node may lag",
                self.at.0
            ))),
        }
    }

    /// Whether `read_anchor_at` reads `report` anchored at the block (no
    /// verdict: for fault attribution).
    async fn reads_anchored(&self, report: &SignedReport, signer: &[u8; 20]) -> Result<bool, Stop> {
        let r = read_anchor_at(self.rpc, self.chain_id, &self.book, report, signer, self.at)
            .await
            .map_err(undecided)?;
        Ok(matches!(r.state, AnchorState::Anchored(_)))
    }

    /// Follow own-report manifests down from `start` (a rows answer's own
    /// manifest, taken from the verified answer itself, or a root and
    /// version) until a version the catalogs list or version 1, and collect
    /// them. Each fetched manifest must be that version of the dataset with
    /// its own report; its key joins the known keys (anchors are read later,
    /// once every key is known: review of the CAT-1 fix, finding 5). A
    /// manifest this node does not serve leaves the check undecided (it may
    /// lag the rows node: fix review finding 2). At a listed version the root
    /// named must be the listed one: two signed roots are the publisher's
    /// when the naming report reads anchored, else undecided (finding 4).
    async fn collect_own(&mut self, start: OwnStart<'_>, out: &mut Vec<Own>) -> Result<(), Stop> {
        let (mut root, mut v, mut first) = match start {
            OwnStart::Answer(c) => (c.manifest_root, c.header.version, Some(c)),
            OwnStart::Root(r, v) => (r, v, None),
        };
        // The report that names `root`: the answer's, or the manifest above.
        let mut namer: Option<(SignedReport, [u8; 20])> = None;
        while v >= 1 {
            // Listed in the catalogs, or met already by another chain.
            let seen = self
                .versions
                .get(&v)
                .map(|x| x.root)
                .or_else(|| out.iter().find(|o| o.version == v).map(|o| o.root));
            if let Some(listed) = seen {
                if listed == root {
                    return Ok(());
                }
                let anchored = match &namer {
                    Some((r, s)) => self.reads_anchored(r, s).await?,
                    None => match first {
                        Some(c) => self.reads_anchored(&c.report, &c.signer).await?,
                        None => false,
                    },
                };
                return Err(if anchored {
                    publisher_fault(format!(
                        "version {v}: root {listed} (from the catalogs or the chain below them) and an anchored signed manifest names {root}"
                    ))
                } else {
                    undecided(format!(
                        "version {v}: root {listed} (from the catalogs or the chain below them), a manifest names {root}, and its report does not read anchored"
                    ))
                });
            }
            let (header, rep, signer) = match first.take() {
                Some(c) => (c.header.clone(), c.report.clone(), c.signer),
                None => {
                    let url = format!("{}/v1/bulk/manifests/{root}", self.base);
                    let body = fetch(self.get, &url, MAX_MANIFEST_ANSWER).await?.ok_or_else(|| {
                        undecided(format!(
                            "this node does not serve manifest {root} (version {v}): it may lag; check again later or on another node"
                        ))
                    })?;
                    let a: Value = serde_json::from_slice(&body)
                        .map_err(|_| node("a manifest answer is not JSON"))?;
                    let m = hex_field(&a["manifest"], "manifest")
                        .ok()
                        .and_then(|b| Manifest::from_bytes(&b).ok())
                        .ok_or_else(|| node("a manifest answer does not decode"))?;
                    let rep = hex_field(&a["report"], "report")
                        .ok()
                        .and_then(|b| SignedReport::try_from_slice(&b).ok())
                        .ok_or_else(|| node("a manifest report does not decode"))?;
                    let h = m.header;
                    if h.root() != root
                        || h.version != v
                        || h.dataset != self.dataset
                        || h.publisher != self.publisher
                    {
                        return Err(node(format!(
                            "manifest {root} is not version {v} of the dataset"
                        )));
                    }
                    let signer = onyx_bulk::anchor::verify(&h, &rep).map_err(|e| {
                        node(format!(
                            "version {v}: the report is not the manifest's own: {e}"
                        ))
                    })?;
                    (h, rep, signer)
                }
            };
            self.add_key(rep.payload.key, signer, "manifest");
            namer = Some((rep.clone(), signer));
            out.push(Own {
                version: v,
                root,
                report: rep,
                signer,
            });
            if v == 1 {
                break;
            }
            root = header.prev;
            v -= 1;
        }
        Ok(())
    }

    /// Every collected own report holds at the block, or another known key's
    /// live report states the same root (review of the build, finding 8);
    /// then its version counts as seen.
    async fn check_own(&mut self, own: Vec<Own>) -> Result<(), Stop> {
        for o in own {
            if !self.reads_anchored(&o.report, &o.signer).await?
                && !self.restated(o.version, &o.root).await?
            {
                self.anchored(&o.report, &o.signer, &format!("version {}", o.version))
                    .await?;
            }
            self.versions.entry(o.version).or_insert(Version {
                version: o.version,
                root: o.root,
                catalog: None,
            });
        }
        Ok(())
    }

    /// A live report of version `v` with root `root` under any known key.
    async fn restated(&self, v: u32, root: &Hash32) -> Result<bool, Stop> {
        for k in self.keys.keys() {
            let d = read_sequence_at(
                self.rpc,
                &self.book,
                k,
                sequence_for(&self.dataset, v),
                self.at,
            )
            .await
            .map_err(undecided)?;
            if d == Hash32::ZERO {
                continue;
            }
            if let Standing::Holds(r) = read_digest_at(self.rpc, &self.book, &d, self.at)
                .await
                .map_err(undecided)?
            {
                if r.value_hash == value_hash_of(root) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

/// Run the history check of `checked` (an answer `check_bulk` verified) on
/// the node `opts.node` and the caller's chain node `rpc`.
pub async fn check_history(
    get: &dyn HttpGet,
    rpc: &dyn JsonRpc,
    chain_id: u64,
    book: [u8; 20],
    checked: &Checked,
    opts: &HistoryOpts,
) -> HistoryOutcome {
    match run(get, rpc, chain_id, book, checked, opts).await {
        Ok(v) => HistoryOutcome::Verified(v),
        Err(Stop::Refused(f, w)) => HistoryOutcome::Refused(f, w),
        Err(Stop::Undecided(w)) => HistoryOutcome::Undecided(w),
    }
}

async fn run(
    get: &dyn HttpGet,
    rpc: &dyn JsonRpc,
    chain_id: u64,
    book: [u8; 20],
    checked: &Checked,
    opts: &HistoryOpts,
) -> Result<Value, Stop> {
    let publisher = checked.header.publisher.clone();
    let dataset = checked.header.dataset.clone();
    let base = opts.node.trim_end_matches('/').to_string();
    let mut walker = Walker::new(publisher.clone(), dataset.clone());
    let mut newest: Option<u64> = None;
    let mut at: Option<At> = None;
    let mut rules: Option<(String, CatalogFile)> = None;
    'walk: loop {
        let from = walker.next_number();
        if newest.is_some_and(|n| from > n) {
            break;
        }
        let url = format!("{base}/v1/bulk/catalogs/{}/files?from={from}", publisher.0);
        let Some(body) = fetch(get, &url, MAX_FILES_BYTES + MAX_CATALOG_BYTES).await? else {
            if from == 0 {
                break;
            }
            return Err(undecided(format!(
                "the node no longer serves catalog {from} (set aside during the walk?): check again"
            )));
        };
        let files = CatalogFiles::try_from_slice(&body)
            .map_err(|_| node("the catalog files answer does not decode"))?;
        if files.files.is_empty() {
            return Err(node("the catalog files answer holds no catalog"));
        }
        if newest.is_none() {
            newest = Some(files.newest);
            // Every chain read at one block, read once the node has said
            // how far its catalogs go (review CAT-HIST-R1 finding 7).
            at = Some(decision_block(rpc, chain_id).await.map_err(undecided)?);
        }
        for f in &files.files {
            if newest.is_some_and(|n| f.header.number > n) {
                break 'walk;
            }
            match walker.take(f) {
                Ok(()) => {}
                Err(WalkError::Node(e)) => return Err(node(e)),
                Err(WalkError::Rules(e)) => {
                    rules = Some((e, f.clone()));
                    break 'walk;
                }
            }
        }
    }
    let at = match at {
        Some(a) => a,
        None => decision_block(rpc, chain_id).await.map_err(undecided)?,
    };
    let mut st = State {
        rpc,
        get,
        chain_id,
        book,
        base,
        publisher: publisher.clone(),
        dataset: dataset.clone(),
        at,
        keys: BTreeMap::new(),
        versions: BTreeMap::new(),
    };
    let steps = &walker.steps;
    if let Some((e, f)) = rules {
        // A signed catalog that breaks the rules: the publisher's fault when
        // it and the one before are anchored (signed and public), else the
        // node served what it should never have taken.
        let mut anchored = match onyx_bulk::catalog::verify(&f.header, &f.report) {
            Ok(signer) => st.reads_anchored(&f.report, &signer).await?,
            Err(_) => false,
        };
        if let Some(s) = steps.last() {
            anchored &= st.reads_anchored(&s.report, &s.signer).await?;
        }
        return Err(if anchored {
            publisher_fault(format!(
                "a signed, anchored catalog breaks the catalog rules: {e}"
            ))
        } else {
            node(format!(
                "the node serves a catalog that breaks the rules and is not anchored: {e}"
            ))
        });
    }

    // Keys: the walk's, the rows answer's own (review of the build, finding
    // 2), the caller's, and a scan's.
    for s in steps {
        st.add_key(s.report.payload.key, s.signer, "walk");
        if let Some(k) = st.keys.get_mut(&s.report.payload.key) {
            k.catalogs.push(s.number);
        }
    }
    st.add_key(checked.report.payload.key, checked.signer, "answer");
    for s in &opts.keys {
        st.add_key(key_id(&publisher, s), *s, "caller");
    }
    let mut scanned = None;
    if let Some(from) = opts.scan_from.filter(|f| *f <= at.0) {
        for (k, signer) in scan_keys(rpc, &book, &publisher, from, at, opts.logs_range).await? {
            st.add_key(k, signer, "scan");
        }
        scanned = Some((from, at.0));
    }

    // The dataset's versions in the catalogs.
    for s in steps {
        if let Some(l) = &s.leaf {
            st.versions.entry(l.version).or_insert(Version {
                version: l.version,
                root: l.root,
                catalog: Some(s.number),
            });
        }
    }

    // The rows answer in the chain (review of the build, finding 3).
    let leaf = CatalogLeaf::of(&checked.header);
    if let Some(h) = &checked.catalog {
        match usize::try_from(h.number).ok().and_then(|n| steps.get(n)) {
            None => {
                return Err(undecided(format!(
                    "the rows answer names catalog {}, past the newest this node serves ({}): it may lag the node that answered the rows",
                    h.number,
                    newest.map_or("none".into(), |n| n.to_string())
                )))
            }
            Some(s) if s.root != h.root() => {
                let both = st.reads_anchored(&checked.report, &checked.signer).await?
                    && st.reads_anchored(&s.report, &s.signer).await?;
                return Err(if both {
                    publisher_fault(format!(
                        "two anchored catalogs {}: the rows answer's and this node's",
                        h.number
                    ))
                } else {
                    undecided(format!(
                        "the rows answer's catalog {} is not this node's, and not both read anchored",
                        h.number
                    ))
                });
            }
            Some(s) if s.leaf.as_ref() != Some(&leaf) => {
                return Err(node(format!(
                    "catalog {} does not list the rows answer's version",
                    h.number
                )))
            }
            Some(_) => {}
        }
    }

    // Every catalog anchored at the block.
    for s in steps {
        st.anchored(&s.report, &s.signer, &format!("catalog {}", s.number))
            .await?;
    }
    // Own reports: below the version the dataset entered the catalogs at
    // (amendment 1 item 3, CAT-R1 L2), and a rows answer anchored by its own
    // report, down to a version the catalogs list (finding 2).
    let mut own = Vec::new();
    if let Some(l) = walker.entered().and_then(|s| s.leaf.clone()) {
        if l.version > 1 {
            st.collect_own(OwnStart::Root(l.prev, l.version - 1), &mut own)
                .await?;
        }
    }
    if checked.catalog.is_none() {
        st.collect_own(OwnStart::Answer(checked), &mut own).await?;
    }
    st.check_own(own).await?;

    // No other live report under any known key.
    let mut set_aside: Vec<Value> = Vec::new();
    let keys: Vec<Hash32> = st.keys.keys().copied().collect();
    let top_version = st.versions.keys().next_back().copied().unwrap_or(0);
    let next_catalog = newest.map_or(0, |n| n + 1);
    for k in &keys {
        for s in steps {
            if *k == s.report.payload.key {
                continue;
            }
            let d = read_sequence_at(rpc, &book, k, sequence_for_catalog(s.number), at)
                .await
                .map_err(undecided)?;
            if d == Hash32::ZERO || d == s.report.payload.digest() {
                continue;
            }
            let r = read_digest_at(rpc, &book, &d, at)
                .await
                .map_err(undecided)?;
            stand(d, *k, format!("catalog {}", s.number), r, &mut set_aside)?;
        }
        for v in st.versions.values() {
            let d = read_sequence_at(rpc, &book, k, sequence_for(&dataset, v.version), at)
                .await
                .map_err(undecided)?;
            if d == Hash32::ZERO {
                continue;
            }
            let r = read_digest_at(rpc, &book, &d, at)
                .await
                .map_err(undecided)?;
            let same = match &r {
                Standing::Holds(x) | Standing::Withdrawn(x) | Standing::Open(x, _) => {
                    x.value_hash == value_hash_of(&v.root)
                }
                Standing::Unrecorded => true,
            };
            if !same {
                stand(d, *k, format!("version {}", v.version), r, &mut set_aside)?;
            }
        }
        // What comes after the newest served (review CAT-HIST-R1 finding 4,
        // and with no catalog at all, catalog 0: review of the build,
        // finding 1).
        for (seq, what) in [
            (
                sequence_for_catalog(next_catalog),
                format!("catalog {next_catalog}"),
            ),
            (
                sequence_for(&dataset, top_version + 1),
                format!("version {} of the dataset", top_version + 1),
            ),
        ] {
            let d = read_sequence_at(rpc, &book, k, seq, at)
                .await
                .map_err(undecided)?;
            if d == Hash32::ZERO {
                continue;
            }
            match read_digest_at(rpc, &book, &d, at)
                .await
                .map_err(undecided)?
            {
                Standing::Withdrawn(_) | Standing::Unrecorded => {}
                _ => {
                    return Err(undecided(format!(
                        "the book holds {what} (key {k}), which this node did not serve"
                    )))
                }
            }
        }
    }

    Ok(json!({
        "ok": true,
        "catalogs": steps.len(),
        "newest": newest,
        "entered": walker.entered().map(|s| s.number),
        "versions": st.versions.values().map(|v| json!({
            "version": v.version, "root": v.root.to_string(), "firstCatalog": v.catalog,
        })).collect::<Vec<_>>(),
        "keys": st.keys.iter().map(|(k, i)| json!({
            "key": k.to_string(),
            "signer": hex0x(&i.signer),
            "found": i.found.iter().collect::<Vec<_>>(),
            "catalogs": {
                "count": i.catalogs.len(),
                "first": i.catalogs.first(),
                "last": i.catalogs.last(),
            },
        })).collect::<Vec<_>>(),
        "setAside": set_aside,
        "created": {
            "first": steps.first().map(|s| s.created_unix),
            "last": steps.last().map(|s| s.created_unix),
        },
        "checkedAt": {"number": at.0, "hash": at.1.to_string()},
        "keysScanned": scanned.map(|(from, to)| json!({"from": from, "to": to})),
        "limit": if scanned.is_some() { Value::Null } else { json!(HISTORY_LIMIT) },
    }))
}

/// Another report `d` under key `k` where the walk holds one (a catalog
/// number, a version): live, it is a rewrite with two signed reports; withdrawn
/// for good, it is listed as set aside; otherwise nothing is decided yet
/// (review CAT-HIST-R1 finding 1).
fn stand(
    d: Hash32,
    k: Hash32,
    what: String,
    st: Standing,
    set_aside: &mut Vec<Value>,
) -> Result<(), Stop> {
    match st {
        Standing::Unrecorded => Ok(()),
        Standing::Holds(_) => Err(publisher_fault(format!(
            "{what}: the book holds two live reports (another one, {d}, under key {k})"
        ))),
        Standing::Withdrawn(_) => {
            set_aside.push(json!({"what": what, "digest": d.to_string(), "key": k.to_string()}));
            Ok(())
        }
        Standing::Open(_, why) => Err(undecided(format!(
            "{what}: another report ({d}, key {k}) may still count: {why}"
        ))),
    }
}

/// `keccak256(borsh(Value::Bytes(root)))`: the book's value hash of a
/// manifest's own report (`onyx_bulk::anchor::payload`).
fn value_hash_of(root: &Hash32) -> Hash32 {
    keccak256(&borsh::to_vec(&Val::Bytes(root.0.to_vec())).unwrap_or_default())
}

/// The `source` string of a `KeyRegistered` log's data (its first dynamic
/// field), when it decodes.
fn log_source(data: &[u8]) -> Option<String> {
    let at = usize::try_from(u64::from_be_bytes(word(data, 0)?[24..].try_into().ok()?)).ok()?;
    let lw = data.get(at..at.checked_add(32)?)?;
    let len = usize::try_from(u64::from_be_bytes(lw[24..].try_into().ok()?)).ok()?;
    let b = data.get(at + 32..at.checked_add(32)?.checked_add(len)?)?;
    String::from_utf8(b.to_vec()).ok()
}

/// The publisher's keys from `KeyRegistered` logs, `from` to the block `at`,
/// `range` blocks a query: a log whose source decodes as another publisher's
/// is skipped, every other is confirmed by `keyState(key).source`.
async fn scan_keys(
    rpc: &dyn JsonRpc,
    book: &[u8; 20],
    publisher: &SourceId,
    from: u64,
    at: At,
    range: u64,
) -> Result<Vec<(Hash32, [u8; 20])>, Stop> {
    let range = range.max(1);
    let topic = key_registered_topic();
    let want = keccak256(publisher.0.as_bytes());
    let mut out = Vec::new();
    let mut lo = from;
    while lo <= at.0 {
        let hi = lo.saturating_add(range - 1).min(at.0);
        let logs = rpc
            .call(
                "eth_getLogs",
                json!([{
                    "address": hex0x(book),
                    "fromBlock": format!("0x{lo:x}"),
                    "toBlock": format!("0x{hi:x}"),
                    "topics": [topic.to_string()],
                }]),
            )
            .await
            .map_err(|e| undecided(format!("key scan, blocks {lo}..{hi}: {e}")))?;
        let logs = logs
            .as_array()
            .cloned()
            .ok_or_else(|| undecided(format!("key scan, blocks {lo}..{hi}: not a list")))?;
        for l in logs {
            let key = l["topics"][1]
                .as_str()
                .and_then(|t| hex::decode(t.trim_start_matches("0x")).ok())
                .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
                .map(Hash32)
                .ok_or_else(|| undecided("key scan: a log without its key"))?;
            let data = l["data"]
                .as_str()
                .and_then(|t| hex::decode(t.trim_start_matches("0x")).ok());
            if data
                .as_deref()
                .and_then(log_source)
                .is_some_and(|s| s != publisher.0)
            {
                continue;
            }
            let k = call_at(rpc, book, "keyState(bytes32)", &key, &at.1)
                .await
                .map_err(undecided)?;
            let bad = || undecided("key scan: keyState does not decode");
            if word(&k, 3).ok_or_else(bad)? == want.0 {
                let s = word(&k, 2).ok_or_else(bad)?;
                let mut signer = [0u8; 20];
                signer.copy_from_slice(&s[12..]);
                out.push((key, signer));
            }
        }
        lo = hi + 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_registered_logs_source_decodes() {
        // source "a.b", origin "x", provenance, by, reason "r".
        let mut d = Vec::new();
        let w = |n: u64| {
            let mut x = [0u8; 32];
            x[24..].copy_from_slice(&n.to_be_bytes());
            x
        };
        for x in [w(160), w(224), [7u8; 32], w(0), w(288)] {
            d.extend_from_slice(&x);
        }
        for s in ["a.b", "x", "r"] {
            d.extend_from_slice(&w(s.len() as u64));
            let mut b = s.as_bytes().to_vec();
            b.resize(32, 0);
            d.extend_from_slice(&b);
        }
        assert_eq!(log_source(&d).as_deref(), Some("a.b"));
        assert_eq!(log_source(&d[..100]), None);
        assert_eq!(log_source(&[]), None);
    }
}
