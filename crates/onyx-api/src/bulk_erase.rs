//! Erasing a removed dataset's shards after a grace period (ADR-0059
//! amendment 4 item 5a, slice 1; THREATS CAT-3, design review CAT-ERASE-R1).
//!
//! A dataset the newest held catalog of its publisher lists as a tombstone
//! gets one durable record under `<bulk dir>/removals/<publisher>/`. An hourly
//! pass ([`BulkStore::erase_pass`]) reads the book at one finalized block and,
//! once the grace is out with the publisher's key still active, erases the
//! shard files no held manifest of another dataset names. Manifests, catalogs
//! and set-aside files are never erased: they carry no rows, and the history
//! check reads them.

use super::*;
use std::collections::BTreeSet;

/// `ONYX_BULK_ERASE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EraseMode {
    /// No records and no erasure.
    Off,
    /// Records and logged plans; nothing is deleted (the default).
    DryRun,
    /// A plan logged at an earlier pass and unchanged is carried out.
    On,
}

/// The least grace, in days (ADR-0059 amendment 4 item 5a).
pub const MIN_GRACE_DAYS: u64 = 30;
/// Shards erased at most per pass, by default.
pub const MAX_SHARDS_PER_PASS: usize = 10_000;

/// How a node erases removed datasets (`ONYX_BULK_ERASE_*`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EraseConfig {
    /// `ONYX_BULK_ERASE`.
    pub mode: EraseMode,
    /// `ONYX_BULK_ERASE_GRACE_DAYS` in seconds (at least 30 days).
    pub grace_secs: u64,
    /// `ONYX_BULK_ERASE_HOLD`: publishers, or `publisher/dataset` names,
    /// this node never erases.
    pub hold: BTreeSet<String>,
    /// `ONYX_BULK_ERASE_MAX_SHARDS`: shards erased at most per pass (a
    /// dataset larger than this is erased alone in its pass).
    pub max_shards: usize,
}

impl Default for EraseConfig {
    fn default() -> Self {
        EraseConfig {
            mode: EraseMode::DryRun,
            grace_secs: MIN_GRACE_DAYS * 86_400,
            hold: BTreeSet::new(),
            max_shards: MAX_SHARDS_PER_PASS,
        }
    }
}

impl EraseConfig {
    fn holds(&self, publisher: &str, dataset: &str) -> bool {
        self.hold.contains(publisher) || self.hold.contains(&format!("{publisher}/{dataset}"))
    }
}

/// Where a record stands.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Not erased; `plan` is the digest of the last plan logged and the block
    /// it was made at (a plan is carried out only when seen again later).
    Waiting { plan: Option<(Hash32, u64)> },
    /// Erasing these roots (from this block and its time): a start finishes it.
    Erasing {
        block: u64,
        at: u64,
        roots: Vec<Hash32>,
    },
    /// Erased these roots; `kept` were named by another dataset.
    Erased {
        block: u64,
        at: u64,
        roots: Vec<Hash32>,
        kept: Vec<Hash32>,
    },
}

/// One removed dataset's record (`<bulk dir>/removals/<p>/<d>.bin`).
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Removal {
    pub(crate) publisher: String,
    pub(crate) dataset: String,
    /// The first catalog of the run (the newest held catalogs, walked back,
    /// that list the tombstone) under the newest catalog's key, and its root.
    pub(crate) first: u64,
    pub(crate) first_root: Hash32,
    /// The newest catalog's key when the record was made.
    pub(crate) key: Hash32,
    /// When the book recorded the first catalog's report (chain time).
    pub(crate) recorded_at: u64,
    /// The time of the block at which this node took the first catalog (its
    /// anchor read): a catalog anchored early and uploaded late still shows
    /// its removal for the whole grace (review CAT-ERASE-R1 M2).
    pub(crate) seen_at: u64,
    pub(crate) phase: Phase,
}

impl Removal {
    /// When the grace starts: the later of the two times.
    pub(crate) fn since(&self) -> u64 {
        self.recorded_at.max(self.seen_at)
    }

    pub(crate) fn due(&self, grace: u64) -> u64 {
        self.since().saturating_add(grace)
    }

    /// The roots marked erased (none while waiting).
    fn roots(&self) -> &[Hash32] {
        match &self.phase {
            Phase::Waiting { .. } => &[],
            Phase::Erasing { roots, .. } | Phase::Erased { roots, .. } => roots,
        }
    }

    pub(crate) fn phase_name(&self) -> &'static str {
        match self.phase {
            Phase::Waiting { .. } => "waiting",
            Phase::Erasing { .. } => "erasing",
            Phase::Erased { .. } => "erased",
        }
    }

    fn erasing_or_erased(&self) -> bool {
        !matches!(self.phase, Phase::Waiting { .. })
    }
}

/// The node's erase state (one lock, never held across a chain read).
#[derive(Default)]
pub(crate) struct EraseState {
    pub(crate) records: BTreeMap<Key, Removal>,
    /// Roots of erasing and erased records: no route serves them.
    erased: BTreeSet<Hash32>,
    /// Roots of records moved aside after erasure: a shard upload takes one
    /// for any held version that names it (review CAT-ERASE-R1 M4).
    restorable: BTreeSet<Hash32>,
    /// The last line logged per dataset: a line per change.
    said: BTreeMap<Key, String>,
}

impl EraseState {
    fn rebuild_erased(&mut self) {
        self.erased = self
            .records
            .values()
            .flat_map(|r| r.roots().iter().copied())
            .collect();
    }
}

/// What one pass did (for the node's log).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ErasePass {
    /// Records waiting out their grace or held.
    pub waiting: usize,
    /// Plans logged this pass (dry run, or a first sighting).
    pub planned: usize,
    /// Datasets erased this pass.
    pub erased: usize,
    /// Shard files removed this pass.
    pub shards: usize,
    /// Shards kept because another dataset names them.
    pub kept: usize,
}

/// The run of a removed dataset as the held catalogs show it now.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Run {
    newest: u64,
    newest_root: Hash32,
    first: u64,
    first_root: Hash32,
    key: Hash32,
    recorded_at: u64,
    /// The block this node read the first catalog's anchor at.
    read_block: u64,
}

/// A publisher's newest held catalog: (number, root, key, recorded time,
/// read block).
type NewestCatalog = (u64, Hash32, Hash32, u64, u64);

/// The finalized block (number, hash) and its time, after checking the chain.
pub(crate) async fn finalized_time(rpc: &dyn JsonRpc, chain_id: u64) -> Result<(At, u64), String> {
    let id = qty(&rpc.call("eth_chainId", json!([])).await?)?;
    if id != chain_id {
        return Err(format!("the node's chain is {id}, not {chain_id}"));
    }
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
    let t = qty(&b["timestamp"])?;
    Ok(((n, Hash32(h)), t))
}

/// The time of block `number`.
async fn block_time(rpc: &dyn JsonRpc, number: u64) -> Result<u64, String> {
    let b = rpc
        .call(
            "eth_getBlockByNumber",
            json!([format!("0x{number:x}"), false]),
        )
        .await?;
    if b.is_null() {
        return Err(format!("the node has no block {number}"));
    }
    qty(&b["timestamp"])
}

/// `keyState(key)` at block `at`: (life, suspended). Life 1 is active.
async fn key_state(
    rpc: &dyn JsonRpc,
    book: &[u8; 20],
    key: &Hash32,
    at: &At,
) -> Result<(u64, bool), String> {
    let k = call_at(rpc, book, "keyState(bytes32)", key, &at.1).await?;
    let bad = || "the signed book's answer does not decode".to_string();
    let life = word_u64(&word(&k, 0).ok_or_else(bad)?).ok_or_else(bad)?;
    let suspended = word_u64(&word(&k, 1).ok_or_else(bad)?).ok_or_else(bad)?;
    Ok((life, suspended != 0))
}

fn io(e: std::io::Error) -> String {
    format!("bulk erase record: {e}")
}

impl BulkStore {
    /// How this store erases removed datasets (default: dry run).
    pub fn with_erase(mut self, cfg: EraseConfig) -> Self {
        self.erase_cfg = cfg;
        self
    }

    fn removals_dir(&self) -> PathBuf {
        self.dir.join("removals")
    }

    fn record_path(&self, publisher: &str, dataset: &str) -> PathBuf {
        // Dotted ids (checked by the manifest and the catalog): no `/`, no `..`.
        self.removals_dir()
            .join(publisher)
            .join(format!("{dataset}.bin"))
    }

    /// Read every record at start, before any route serves: an erasing or
    /// erased record's roots are not served (a start finishes an erase). A
    /// record that does not decode fails the start.
    pub(crate) fn load_removals(&self) -> Result<(), String> {
        let mut st = EraseState::default();
        let Ok(pubs) = std::fs::read_dir(self.removals_dir()) else {
            return Ok(());
        };
        for p in pubs.flatten() {
            for f in std::fs::read_dir(p.path()).map_err(io)?.flatten() {
                let path = f.path();
                if path.is_dir() {
                    // Records moved aside: their erased roots may come back.
                    for g in std::fs::read_dir(&path).map_err(io)?.flatten() {
                        // A `.tmp` left by a cut-short move is not a record
                        // (review of the build, finding 3).
                        if g.path().extension().is_none_or(|e| e != "bin") {
                            continue;
                        }
                        let bytes = std::fs::read(g.path()).map_err(io)?;
                        let r = Removal::try_from_slice(&bytes)
                            .map_err(|_| format!("{}: does not decode", g.path().display()))?;
                        st.restorable.extend(r.roots().iter().copied());
                    }
                    continue;
                }
                if path.extension().is_none_or(|e| e != "bin") {
                    continue;
                }
                let bytes = std::fs::read(&path).map_err(io)?;
                let r = Removal::try_from_slice(&bytes)
                    .map_err(|_| format!("{}: does not decode", path.display()))?;
                st.records
                    .insert((r.publisher.clone(), r.dataset.clone()), r);
            }
        }
        st.rebuild_erased();
        *self.erase.lock().map_err(|_| "lock")? = st;
        Ok(())
    }

    fn write_record(&self, r: &Removal) -> Result<(), String> {
        let bytes = borsh::to_vec(r).map_err(|_| "encode".to_string())?;
        write_durable(&self.record_path(&r.publisher, &r.dataset), &bytes).map_err(io)
    }

    /// Move a record aside (its dataset is back, or its run changed): its
    /// erased roots are no longer marked and a shard upload takes them again.
    fn move_record_aside(&self, st: &mut EraseState, r: &Removal, why: &str) -> Result<(), String> {
        let from = self.record_path(&r.publisher, &r.dataset);
        let to = self
            .removals_dir()
            .join(&r.publisher)
            .join("withdrawn")
            .join(format!("{}-{}-{}.bin", r.dataset, r.first, r.seen_at));
        let bytes = borsh::to_vec(r).map_err(|_| "encode".to_string())?;
        write_durable(&to, &bytes).map_err(io)?;
        match std::fs::remove_file(&from) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(e)),
        }
        let key = (r.publisher.clone(), r.dataset.clone());
        st.records.remove(&key);
        if r.erasing_or_erased() {
            st.restorable.extend(r.roots().iter().copied());
            eprintln!(
                "onyx-node: bulk: erase: {}/{}: {why} after erasure: re-upload {} shards",
                r.publisher,
                r.dataset,
                r.roots().len()
            );
        } else {
            eprintln!(
                "onyx-node: bulk: erase: {}/{}: {why}",
                r.publisher, r.dataset
            );
        }
        st.said.remove(&key);
        st.rebuild_erased();
        Ok(())
    }

    /// Whether `root` is marked erased (no route serves it).
    pub(crate) fn erased_root(&self, root: &Hash32) -> bool {
        self.erase.lock().is_ok_and(|s| s.erased.contains(root))
    }

    /// Whether a shard upload of `root` may name any held version (a root of
    /// a record moved aside after erasure).
    pub(crate) fn restorable(&self, root: &Hash32) -> bool {
        self.erase.lock().is_ok_and(|s| s.restorable.contains(root))
    }

    /// Datasets erasing or erased: their bytes leave the quota.
    pub(crate) fn erased_datasets(&self) -> BTreeSet<Key> {
        self.erase
            .lock()
            .map(|s| {
                s.records
                    .iter()
                    .filter(|(_, r)| r.erasing_or_erased())
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Per publisher: (records waiting, datasets erased).
    pub(crate) fn erase_counts(&self) -> BTreeMap<String, (usize, usize)> {
        let mut out: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        if let Ok(s) = self.erase.lock() {
            for ((p, _), r) in &s.records {
                let e = out.entry(p.clone()).or_default();
                if r.erasing_or_erased() {
                    e.1 += 1;
                } else {
                    e.0 += 1;
                }
            }
        }
        out
    }

    /// The erase fields of a removed dataset's 410 (phase and due time).
    pub(crate) fn erase_json(&self, publisher: &str, dataset: &str) -> Option<Value> {
        let s = self.erase.lock().ok()?;
        let r = s
            .records
            .get(&(publisher.to_string(), dataset.to_string()))?;
        Some(json!({"phase": r.phase_name(), "due": r.due(self.erase_cfg.grace_secs)}))
    }

    /// A root stored again (a held manifest names it now): it is served
    /// again and its record no longer lists it. The records are written under
    /// the erase state's lock, so two uploads at once never write one record
    /// together (review of the build, finding 2).
    pub(crate) fn unerase(&self, root: &Hash32) {
        let Ok(mut s) = self.erase.lock() else { return };
        s.restorable.remove(root);
        if !s.erased.contains(root) {
            return;
        }
        let mut changed = Vec::new();
        for r in s.records.values_mut() {
            let list = match &mut r.phase {
                Phase::Waiting { .. } => continue,
                Phase::Erasing { roots, .. } | Phase::Erased { roots, .. } => roots,
            };
            let before = list.len();
            list.retain(|x| x != root);
            if list.len() != before {
                changed.push(r.clone());
            }
        }
        s.rebuild_erased();
        for r in changed {
            if let Err(e) = self.write_record(&r) {
                eprintln!("onyx-node: bulk: erase: {}/{}: {e}", r.publisher, r.dataset);
            }
        }
    }

    /// The newest held catalog of `publisher`: (number, root, key, recorded
    /// time, read block).
    fn newest_of(&self, publisher: &str) -> Result<Option<NewestCatalog>, String> {
        let cs = self.catalogs.read().map_err(|_| "lock")?;
        Ok(cs
            .get(publisher)
            .and_then(|m| m.last_key_value())
            .map(|(n, h)| {
                (
                    *n,
                    h.header.root(),
                    h.report.payload.key,
                    h.anchor.recorded_at,
                    h.anchor.block_number,
                )
            }))
    }

    /// The runs of `datasets`' tombstones in `publisher`'s held catalogs, in
    /// one walk back from the newest: each older catalog is rebuilt once,
    /// whatever the number of datasets (review of the build, finding 7).
    fn runs_of(
        &self,
        publisher: &str,
        datasets: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, Run>, String> {
        let Some((newest, newest_root, key, recorded_at, read_block)) =
            self.newest_of(publisher)?
        else {
            return Ok(BTreeMap::new());
        };
        let mut runs: BTreeMap<String, Run> = datasets
            .iter()
            .map(|d| {
                (
                    d.clone(),
                    Run {
                        newest,
                        newest_root,
                        first: newest,
                        first_root: newest_root,
                        key,
                        recorded_at,
                        read_block,
                    },
                )
            })
            .collect();
        let mut open: BTreeSet<String> = datasets.clone();
        let mut n = newest;
        while n > 0 && !open.is_empty() {
            let prev = n - 1;
            let held = {
                let cs = self.catalogs.read().map_err(|_| "lock")?;
                cs.get(publisher).and_then(|m| m.get(&prev)).map(|h| {
                    (
                        h.report.payload.key,
                        h.header.root(),
                        h.anchor.recorded_at,
                        h.anchor.block_number,
                    )
                })
            };
            let Some((k, r, at, block)) = held else { break };
            let Some((body, _)) = self.catalog_body(publisher, prev)? else {
                break;
            };
            open.retain(|d| body.removed.contains(d));
            if k == key {
                for d in &open {
                    if let Some(run) = runs.get_mut(d) {
                        run.first = prev;
                        run.first_root = r;
                        run.recorded_at = at;
                        run.read_block = block;
                    }
                }
            }
            n = prev;
        }
        Ok(runs)
    }

    /// The highest block a held catalog of `publisher` was read at.
    fn last_catalog_read(&self, publisher: &str) -> u64 {
        self.catalogs
            .read()
            .ok()
            .and_then(|cs| {
                cs.get(publisher)
                    .and_then(|m| m.values().map(|h| h.anchor.block_number).max())
            })
            .unwrap_or(0)
    }

    /// Why a waiting record is not erased at block `at` (time `now`), or
    /// `None` when it is due. Chain reads only, outside every lock. The text
    /// names no block, so a hold is logged once (review of the build, finding 8).
    async fn erase_guard(
        &self,
        r: &Removal,
        newest: u64,
        at: At,
        now: u64,
    ) -> Result<Option<String>, String> {
        let (p, d) = (&r.publisher, &r.dataset);
        if self.erase_cfg.holds(p, d) {
            return Ok(Some("held: by the operator (ONYX_BULK_ERASE_HOLD)".into()));
        }
        let due = r.due(self.erase_cfg.grace_secs);
        if now < due {
            return Ok(Some(format!("waiting, due {due}")));
        }
        if self.fork_marked(p).is_some() {
            return Ok(Some("held: a catalog fork of the publisher stands".into()));
        }
        if at.0 < self.last_catalog_read(p) {
            return Ok(Some(
                "held: the finalized block is behind a held catalog's read".into(),
            ));
        }
        let reports = {
            let cs = self.catalogs.read().map_err(|_| "lock")?;
            let m = cs.get(p.as_str());
            let pick = |n: u64| {
                m.and_then(|m| m.get(&n))
                    .map(|h| (h.report.clone(), h.signer))
            };
            (pick(r.first), pick(newest))
        };
        let (Some(first), Some(last)) = reports else {
            return Ok(Some("held: a catalog of the run is no longer held".into()));
        };
        for (what, (rep, signer)) in [("first", first), ("newest", last)] {
            let reading = read_anchor_at(
                self.rpc.as_ref(),
                self.pin.chain_id,
                &self.pin.book,
                &rep,
                &signer,
                at,
            )
            .await?;
            if !matches!(reading.state, AnchorState::Anchored(_)) {
                return Ok(Some(format!(
                    "held: the run's {what} catalog does not read anchored"
                )));
            }
            if what == "newest" {
                let (life, suspended) =
                    key_state(self.rpc.as_ref(), &self.pin.book, &rep.payload.key, &at).await?;
                if life != 1 || suspended {
                    return Ok(Some(format!(
                        "held: the publisher's key is not active (life {life}, suspended {suspended})"
                    )));
                }
            }
        }
        Ok(None)
    }

    /// The shard roots to erase for `key` and those kept because another
    /// dataset names them: candidates are every version's, held and set
    /// aside; references are the held versions of every other dataset except
    /// those erasing or erased and still removed (review CAT-ERASE-R1 M1; a
    /// dataset due in the same pass is a reference, so a shared root goes
    /// with the last of them, a plan later: review of the build, finding 1).
    fn plan_of(&self, key: &Key) -> Result<(Vec<Hash32>, Vec<Hash32>), String> {
        // Erased datasets that are still removed (one that came back is a
        // reference again), read before the datasets lock.
        let mut gone = BTreeSet::new();
        for k in self.erased_datasets() {
            if self.removed(&k.0, &k.1).map_err(|e| e.message)?.is_some() {
                gone.insert(k);
            }
        }
        let mut cand: BTreeSet<Hash32> = BTreeSet::new();
        let aside = self.dataset_dir(&key.0, &key.1).join("withdrawn");
        if let Ok(files) = std::fs::read_dir(&aside) {
            for f in files.flatten() {
                let path = f.path();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !name.ends_with(".bin") || name.ends_with(".catalog.bin") {
                    continue;
                }
                let bytes = std::fs::read(&path).map_err(io)?;
                let (m, _, _): (Vec<u8>, Vec<u8>, Vec<u8>) =
                    BorshDeserialize::try_from_slice(&bytes)
                        .map_err(|_| format!("{}: does not decode", path.display()))?;
                let m = Manifest::from_bytes(&m).map_err(|e| format!("{}: {e}", path.display()))?;
                cand.extend(m.shards.iter().map(|e| e.root));
            }
        }
        let ds = self.datasets.read().map_err(|_| "lock")?;
        if let Some(x) = ds.get(key) {
            for h in &x.versions {
                cand.extend(h.manifest.shards.iter().map(|e| e.root));
            }
        }
        let mut refs: BTreeSet<Hash32> = BTreeSet::new();
        for (k, x) in ds.iter() {
            if k == key || gone.contains(k) {
                continue;
            }
            for h in &x.versions {
                refs.extend(h.manifest.shards.iter().map(|e| e.root));
            }
        }
        let (kept, roots): (Vec<Hash32>, Vec<Hash32>) =
            cand.into_iter().partition(|r| refs.contains(r));
        Ok((roots, kept))
    }

    fn put_record(&self, r: &Removal, line: &str) -> Result<(), String> {
        let mut s = self.erase.lock().map_err(|_| "lock")?;
        self.write_record(r)?;
        let key = (r.publisher.clone(), r.dataset.clone());
        s.records.insert(key.clone(), r.clone());
        s.rebuild_erased();
        s.said.insert(key, line.into());
        Ok(())
    }

    /// Steps 1 to 5 of the erase (ADR-0059 amendment 4 item 5a): the record
    /// first, then the marks, then the tier index saved, then the files, then
    /// the record again. The file work runs off the async threads. Returns
    /// the files removed.
    async fn erase_roots(
        &self,
        r: &mut Removal,
        roots: Vec<Hash32>,
        kept: Vec<Hash32>,
        block: u64,
        at: u64,
    ) -> Result<usize, String> {
        r.phase = Phase::Erasing {
            block,
            at,
            roots: roots.clone(),
        };
        self.put_record(r, "erasing")?;
        let set: BTreeSet<Hash32> = roots.iter().copied().collect();
        if let Ok(mut l) = self.loaded.lock() {
            l.retain(|((root, _), _)| !set.contains(root));
        }
        if let Ok(mut t) = self.touched.lock() {
            t.retain(|root, _| !set.contains(root));
        }
        eprintln!(
            "onyx-node: bulk: erase: {}/{}: erasing {} shards, {} shared kept, block {block}",
            r.publisher,
            r.dataset,
            roots.len(),
            kept.len()
        );
        #[cfg(test)]
        let stop = self.erase_stop.lock().map(|s| *s).unwrap_or(None);
        #[cfg(not(test))]
        let stop: Option<usize> = None;
        let (tiers, file, list) = (
            self.tiers.clone(),
            self.dir.join("tiers.json"),
            roots.clone(),
        );
        let removed = tokio::task::spawn_blocking(move || -> Result<usize, String> {
            for root in &list {
                tiers.forget_bulk(root);
            }
            tiers.save(&file).map_err(|e| format!("tiers file: {e}"))?;
            let mut removed = 0;
            for root in &list {
                if stop.is_some_and(|k| removed >= k) {
                    return Err("stopped (test)".into());
                }
                match tiers.drop_bulk_files(root) {
                    Ok(()) => removed += 1,
                    // Stored again meanwhile: a held manifest names it now.
                    Err(onyx_coordinator::tiers::TierError::Tracked(_)) => {}
                    Err(e) => return Err(format!("erase {root}: {e}")),
                }
            }
            Ok(removed)
        })
        .await
        .map_err(|_| "erase task".to_string())??;
        r.phase = Phase::Erased {
            block,
            at,
            roots,
            kept,
        };
        self.put_record(r, "erased")?;
        eprintln!(
            "onyx-node: bulk: erase: {}/{}: erased",
            r.publisher, r.dataset
        );
        Ok(removed)
    }

    fn say(&self, key: &Key, line: String) {
        let Ok(mut s) = self.erase.lock() else { return };
        if s.said.get(key) != Some(&line) {
            eprintln!("onyx-node: bulk: erase: {}/{}: {line}", key.0, key.1);
            s.said.insert(key.clone(), line);
        }
    }

    /// Whether the newest catalog and the run's first are still held as
    /// `run` and `r` say (the re-check under the locks; review of the build,
    /// finding 5).
    fn same_run(&self, r: &Removal, run: &Run) -> bool {
        self.catalogs.read().is_ok_and(|cs| {
            cs.get(&r.publisher).is_some_and(|m| {
                m.last_key_value()
                    .is_some_and(|(n, h)| *n == run.newest && h.header.root() == run.newest_root)
                    && m.get(&r.first)
                        .is_some_and(|h| h.header.root() == r.first_root)
            })
        })
    }

    /// One erase pass (hourly, after the sweep; ADR-0059 amendment 4 item
    /// 5a). `Err` is a chain or store that could not be read: nothing more
    /// is decided this pass.
    pub async fn erase_pass(&self) -> Result<ErasePass, String> {
        let mode = self.erase_cfg.mode;
        if mode == EraseMode::Off {
            return Ok(ErasePass::default());
        }
        let (at, now) = finalized_time(self.rpc.as_ref(), self.pin.chain_id).await?;
        // 1. The records against the catalogs held now (no chain read but
        //    the times of the blocks new runs' first catalogs were taken at).
        let newest = self.removals().map_err(|e| e.message)?;
        let known: BTreeMap<Key, Removal> = self.erase.lock().map_err(|_| "lock")?.records.clone();
        let mut runs: BTreeMap<Key, Run> = BTreeMap::new();
        for (p, body) in &newest {
            let Some((n, n_root, n_key, _, _)) = self.newest_of(p)? else {
                continue;
            };
            let mut walk = BTreeSet::new();
            for d in &body.removed {
                let key = (p.clone(), d.clone());
                // A record whose first catalog is still held unchanged under
                // the newest key keeps its run (successors keep a tombstone).
                match known.get(&key) {
                    Some(r)
                        if r.key == n_key
                            && self.catalogs.read().is_ok_and(|cs| {
                                cs.get(p)
                                    .and_then(|m| m.get(&r.first))
                                    .is_some_and(|h| h.header.root() == r.first_root)
                            }) =>
                    {
                        runs.insert(
                            key,
                            Run {
                                newest: n,
                                newest_root: n_root,
                                first: r.first,
                                first_root: r.first_root,
                                key: r.key,
                                recorded_at: r.recorded_at,
                                read_block: 0,
                            },
                        );
                    }
                    _ => {
                        walk.insert(d.clone());
                    }
                }
            }
            for (d, run) in self.runs_of(p, &walk)? {
                runs.insert((p.clone(), d), run);
            }
        }
        let changed = |r: &Removal, run: &Run| {
            run.first != r.first || run.first_root != r.first_root || run.key != r.key
        };
        let mut taken: BTreeMap<Key, u64> = BTreeMap::new();
        for (key, run) in &runs {
            if known.get(key).is_none_or(|r| changed(r, run)) {
                taken.insert(
                    key.clone(),
                    block_time(self.rpc.as_ref(), run.read_block).await?,
                );
            }
        }
        {
            let mut st = self.erase.lock().map_err(|_| "lock")?;
            let old: Vec<Removal> = st.records.values().cloned().collect();
            for r in old {
                let key = (r.publisher.clone(), r.dataset.clone());
                match runs.get(&key) {
                    None => self.move_record_aside(
                        &mut st,
                        &r,
                        "back (its tombstone's catalog was set aside)",
                    )?,
                    Some(run) if changed(&r, run) => self.move_record_aside(
                        &mut st,
                        &r,
                        "its run changed (another first catalog or key)",
                    )?,
                    Some(_) => {}
                }
            }
            for (key, run) in &runs {
                if st.records.contains_key(key) {
                    continue;
                }
                let Some(seen_at) = taken.get(key).copied() else {
                    continue;
                };
                let r = Removal {
                    publisher: key.0.clone(),
                    dataset: key.1.clone(),
                    first: run.first,
                    first_root: run.first_root,
                    key: run.key,
                    recorded_at: run.recorded_at,
                    seen_at,
                    phase: Phase::Waiting { plan: None },
                };
                self.write_record(&r)?;
                st.records.insert(key.clone(), r);
            }
        }
        // 2. The guard, read at one block outside every lock.
        let records: Vec<Removal> = self
            .erase
            .lock()
            .map_err(|_| "lock")?
            .records
            .values()
            .cloned()
            .collect();
        let mut out = ErasePass::default();
        let mut due: Vec<Key> = Vec::new();
        let mut finish: Vec<Key> = Vec::new();
        for r in &records {
            let key = (r.publisher.clone(), r.dataset.clone());
            match r.phase {
                Phase::Erased { .. } => continue,
                // A start finishes an erase it began (the guard held then),
                // unless the operator holds it now (review of the build, 6).
                Phase::Erasing { .. } => {
                    if self.erase_cfg.holds(&r.publisher, &r.dataset) {
                        self.say(
                            &key,
                            "held: by the operator (ONYX_BULK_ERASE_HOLD), erase not finished"
                                .into(),
                        );
                    } else {
                        finish.push(key);
                    }
                }
                Phase::Waiting { plan } => {
                    let newest_n = runs.get(&key).map_or(r.first, |x| x.newest);
                    match self.erase_guard(r, newest_n, at, now).await? {
                        Some(why) => {
                            out.waiting += 1;
                            self.say(&key, why);
                            // A plan counts only while the dataset stays due:
                            // a hold or a guard that fails starts it again
                            // (review of the build, finding 6).
                            if plan.is_some() {
                                let mut w = r.clone();
                                w.phase = Phase::Waiting { plan: None };
                                self.put_record(&w, "held")?;
                            }
                        }
                        None => due.push(key),
                    }
                }
            }
        }
        if due.is_empty() && finish.is_empty() {
            return Ok(out);
        }
        // 3. Plans and erasure, with no upload meanwhile (review CAT-ERASE-R1 H1).
        let _no_uploads = self.erase_lock.write().await;
        let mut budget = self.erase_cfg.max_shards;
        let mut first_erase = true;
        for key in finish.iter().chain(due.iter()) {
            let _one = self.publisher_lock(&key.0).write().await;
            let Some(mut r) = self
                .erase
                .lock()
                .map_err(|_| "lock")?
                .records
                .get(key)
                .cloned()
            else {
                continue;
            };
            // Still removed with the same run, under the locks.
            let Some(run) = runs.get(key) else { continue };
            if self.removed(&key.0, &key.1).map_err(|e| e.message)? != Some(run.newest)
                || !self.same_run(&r, run)
            {
                continue;
            }
            let (mut roots, mut kept) = self.plan_of(key)?;
            if let Phase::Erasing {
                roots: began,
                block,
                at: t,
                ..
            } = r.phase.clone()
            {
                // Finish only what it began; a root named meanwhile is kept.
                let began: BTreeSet<Hash32> = began.into_iter().collect();
                kept.retain(|x| began.contains(x));
                roots.retain(|x| began.contains(x));
                if mode != EraseMode::On {
                    self.say(key, "erasing (finished when ONYX_BULK_ERASE=on)".into());
                    continue;
                }
                out.kept += kept.len();
                out.shards += self.erase_roots(&mut r, roots, kept, block, t).await?;
                out.erased += 1;
                continue;
            }
            let digest = keccak256(&roots.iter().flat_map(|x| x.0).collect::<Vec<u8>>());
            let seen = match &r.phase {
                Phase::Waiting { plan: Some((d, b)) } if *d == digest => Some(*b),
                _ => None,
            };
            let fits = first_erase || budget >= roots.len();
            let go = mode == EraseMode::On && seen.is_some_and(|b| b < at.0) && fits;
            if !go {
                if seen.is_none() {
                    r.phase = Phase::Waiting {
                        plan: Some((digest, at.0)),
                    };
                    self.put_record(&r, "planned")?;
                    out.planned += 1;
                }
                out.waiting += 1;
                self.say(
                    key,
                    format!(
                        "plan {digest}: {} shards, {} shared kept{}",
                        roots.len(),
                        kept.len(),
                        match (mode, fits) {
                            (EraseMode::On, true) => "",
                            (EraseMode::On, false) => " (next pass: this one's budget is spent)",
                            _ => " (dry run)",
                        }
                    ),
                );
                continue;
            }
            first_erase = false;
            budget = budget.saturating_sub(roots.len());
            out.kept += kept.len();
            out.shards += self.erase_roots(&mut r, roots, kept, at.0, now).await?;
            out.erased += 1;
        }
        Ok(out)
    }
}
