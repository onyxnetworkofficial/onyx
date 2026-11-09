//! Storage nodes learn of a removal (ADR-0059 amendment 4 item 5a, slice 2a;
//! THREATS CAT-3, design review CAT-LEARN-R1, build review CAT-LEARN-R2).
//!
//! A storage node holds its deals' manifests by root and no catalogs, so
//! [`BulkStore::removed`] never names their datasets. Its learn task
//! (`storage::StorageNode::learn_pass`) asks catalog nodes, then each deal's
//! source, then its peers for the publisher's newest leaf of each placed
//! dataset (`GET /v1/bulk/catalogs/{publisher}/leaf/{dataset}`, answered here
//! by [`BulkStore::leaf_answer`]) and hands every answer to
//! [`BulkStore::learn_from`], which keeps a tombstone only when every check
//! holds at one finalized block. Kept proofs are one durable record
//! `<bulk dir>/learned/<publisher>/<dataset>.bin`; while it is kept (with
//! `ONYX_STORAGE_REMOVALS=on`) rows, info and the listing answer `410
//! bulk_dataset_removed` with `learned`, and the public uploads refuse the
//! dataset. It is kept apart from [`BulkStore::removed`] (catalog-only: the
//! erase pass reads it), and a storage node's own fetches never consult it,
//! so its deals' challenges are untouched and the shards stay readable by
//! root (THREATS CAT-ERASE-2).
//!
//! One pass at a time ([`LearnPass`] holds the turn): every file is written
//! by the pass outside the state's lock, then swapped in memory under it.

use super::*;
use std::collections::BTreeSet;

/// `ONYX_STORAGE_REMOVALS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemovalsMode {
    /// A kept proof stops rows, info, the listing and the public uploads
    /// (the default).
    On,
    /// Records and logs only: nothing served changes.
    Log,
    /// No learning, and kept records are not used.
    Off,
}

/// One proof of a removal: a catalog's header, the dataset's tombstone with
/// its path and neighbours, the catalog's signed report, when the book
/// recorded that report and the time of the finalized block this node first
/// verified it at.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Proof {
    pub(crate) header: CatalogHeader,
    pub(crate) leaf: LeafProof,
    pub(crate) report: SignedReport,
    pub(crate) recorded_at: u64,
    pub(crate) seen_at: u64,
}

impl Proof {
    pub(crate) fn key(&self) -> Hash32 {
        self.report.payload.key
    }

    pub(crate) fn number(&self) -> u64 {
        self.header.number
    }
}

/// One learned removal (`<bulk dir>/learned/<publisher>/<dataset>.bin`): the
/// highest-numbered proof that holds, and under every other key and its own
/// the first proof this node kept (review CAT-LEARN-R2 4). [`Learned::first`]
/// follows slice 1's rule: the first catalog under the newest catalog's key
/// starts slice 2b's grace, so newer catalogs and keys taking turns never
/// restart it. Every proof is read again each pass and goes only on the
/// book's final word on its own catalog.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Learned {
    pub(crate) publisher: String,
    pub(crate) dataset: String,
    pub(crate) newest: Proof,
    /// Each key's first proof, older than `newest`, in catalog order.
    pub(crate) firsts: Vec<Proof>,
    /// The finalized block (number, hash) `newest` last read anchored at:
    /// written with the record, then kept in memory (a record is never
    /// written again just for it; review CAT-LEARN-R2 3).
    pub(crate) checked: (u64, Hash32),
}

impl Learned {
    /// The first proof under the newest proof's key.
    pub(crate) fn first(&self) -> &Proof {
        let k = self.newest.key();
        self.firsts
            .iter()
            .find(|p| p.key() == k)
            .unwrap_or(&self.newest)
    }

    /// `p` (newer than `newest`) becomes the newest; the old newest stays as
    /// its key's first unless that key has one.
    fn take_newer(&mut self, p: Proof) {
        let old = std::mem::replace(&mut self.newest, p);
        if !self.firsts.iter().any(|f| f.key() == old.key()) {
            self.firsts.push(old);
        }
        self.firsts.sort_by_key(Proof::number);
    }

    /// Without the proofs whose reports are `gone`: `None` when none is left;
    /// `Some(true)` when the newest went (the newest left takes its place).
    fn without(&mut self, gone: &BTreeSet<Hash32>) -> Option<bool> {
        self.firsts.retain(|f| !gone.contains(&f.report.hash()));
        if !gone.contains(&self.newest.report.hash()) {
            return Some(false);
        }
        self.newest = self.firsts.pop()?;
        Some(true)
    }

    fn proofs(&self) -> impl Iterator<Item = &Proof> {
        std::iter::once(&self.newest).chain(self.firsts.iter())
    }
}

/// The node's learned removals.
#[derive(Default)]
pub(crate) struct LearnState {
    records: BTreeMap<Key, Learned>,
    /// Records kept in memory whose file could not be written: written again
    /// at the next recheck.
    unsaved: BTreeSet<Key>,
    /// The last line logged per dataset: a line per change.
    said: BTreeMap<Key, String>,
}

/// What the book said of one report in a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read {
    Anchored {
        recorded_at: u64,
    },
    /// Not anchored, and not the book's final word (a suspended key, an open
    /// dispute, a record not final yet): a kept proof stays.
    Open(&'static str),
    /// The book's final word (a withdrawal, another signature): a kept proof
    /// goes.
    Withdrawn(&'static str),
}

/// One learn pass: every chain read at one finalized block, one reading per
/// report (shared by every tombstone a catalog lists). It holds the store's
/// learn turn: one pass at a time.
pub struct LearnPass {
    at: At,
    time: u64,
    reads: BTreeMap<Hash32, Read>,
    /// Records whose every proof the book withdrew this pass: still served
    /// as removed until [`BulkStore::learn_finish`], so the dataset is asked
    /// about again first (review CAT-LEARN-R2 4).
    dropping: BTreeSet<Key>,
    /// Records whose newest proof went this pass (an older one took its place).
    moved: BTreeSet<Key>,
    /// Datasets a verified answer named this pass (live or removed).
    pub heard: BTreeSet<(String, String)>,
    _turn: tokio::sync::OwnedMutexGuard<()>,
}

impl LearnPass {
    /// The finalized block of the pass.
    pub fn block(&self) -> u64 {
        self.at.0
    }

    /// The datasets to ask about again before anything else this pass: those
    /// whose newest proof the book withdrew.
    pub fn reask(&self) -> Vec<(String, String)> {
        self.dropping.union(&self.moved).cloned().collect()
    }
}

/// What one answer did ([`BulkStore::learn_from`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Learn {
    /// A tombstone that holds was kept (new, or newer than the kept one):
    /// its catalog number.
    Kept(u64),
    /// The kept proof again.
    Same(u64),
    /// A tombstone older than the kept one (not read).
    Older(u64),
    /// A live leaf whose catalog reads anchored: never ends a search (an old
    /// catalog may still be anchored).
    Live(u64),
    /// Nothing kept, and why.
    Refused(String),
}

impl Learn {
    /// A proof that holds: the search for this dataset ends.
    pub fn holds(&self) -> bool {
        matches!(self, Learn::Kept(_) | Learn::Same(_))
    }
}

/// What the recheck of the kept proofs did ([`BulkStore::learn_recheck`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recheck {
    /// Records kept (a proof anchored, or not the book's final word).
    pub kept: usize,
    /// Of those, whose newest proof does not read anchored now (kept until
    /// the book's final word).
    pub open: usize,
    /// Of those, whose newest proof the book withdrew: an older one that
    /// holds took its place.
    pub moved: usize,
    /// Proofs that are not the newest (a key's first) the book withdrew.
    pub firsts_gone: usize,
    /// Records whose every proof the book withdrew: dropped at
    /// [`BulkStore::learn_finish`] unless the dataset is learned again.
    pub withdrawn: usize,
}

fn io(e: std::io::Error) -> String {
    format!("bulk learned record: {e}")
}

fn no_leaf() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "bulk_no_leaf",
        "this node holds no catalog leaf of this dataset",
    )
}

/// A leaf answer's header, leaf proof and report (the other fields are for
/// readers: never trusted).
fn decode_answer(body: &[u8]) -> Result<(CatalogHeader, LeafProof, SignedReport), String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "not JSON".to_string())?;
    let field = |k: &str| onyx_bulk::wire::hex_field(&v[k], k).map_err(|e| e.to_string());
    let header = CatalogHeader::try_from_slice(&field("header")?)
        .map_err(|_| "header does not decode".to_string())?;
    let leaf = LeafProof::try_from_slice(&field("leaf")?)
        .map_err(|_| "leaf does not decode".to_string())?;
    let report = SignedReport::try_from_slice(&field("report")?)
        .map_err(|_| "report does not decode".to_string())?;
    Ok((header, leaf, report))
}

/// The checks that need no chain (review CAT-LEARN-R1 L1): the header's
/// publisher is `publisher` (the held manifest's); the header proves the leaf
/// (path, index, count, neighbours); the leaf names `publisher` and
/// `dataset`; the report signs the header. Returns the signer.
fn offline(
    publisher: &str,
    dataset: &str,
    header: &CatalogHeader,
    leaf: &LeafProof,
    report: &SignedReport,
) -> Result<[u8; 20], String> {
    if header.publisher.0 != publisher {
        return Err("the catalog is another publisher's".into());
    }
    header.proves(leaf).map_err(|e| e.to_string())?;
    if leaf.leaf.publisher.0 != publisher || leaf.leaf.dataset != dataset {
        return Err("the leaf names another dataset".into());
    }
    cat::verify(header, report).map_err(|e| format!("the report: {e}"))
}

/// A tombstone at version 2 or later (a hand-made catalog may hold any leaf).
fn tombstone_shape(leaf: &CatalogLeaf) -> Result<(), String> {
    if !leaf.is_tombstone() {
        return Err("not a tombstone".into());
    }
    if leaf.version < 2 || leaf.prev == Hash32::ZERO {
        return Err("a tombstone at version 1".into());
    }
    Ok(())
}

fn checked_json((n, h): (u64, Hash32)) -> Value {
    json!({"number": n, "hash": h.to_string()})
}

impl BulkStore {
    /// How learned removals are used (`ONYX_STORAGE_REMOVALS`; default on).
    pub fn with_removals(mut self, mode: RemovalsMode) -> Self {
        self.removals_mode = mode;
        self
    }

    /// `ONYX_STORAGE_REMOVALS`.
    pub fn removals_mode(&self) -> RemovalsMode {
        self.removals_mode
    }

    fn learned_dir(&self) -> PathBuf {
        self.dir.join("learned")
    }

    fn learned_path(&self, publisher: &str, dataset: &str) -> PathBuf {
        // Dotted ids (a held manifest's): no `/`, no `..`.
        self.learned_dir()
            .join(publisher)
            .join(format!("{dataset}.bin"))
    }

    /// Read every learned record at start, before any route serves. Only
    /// `.bin` files in a publisher's folder are records (a `.tmp` left by a
    /// cut-short write, `withdrawn/`, `damaged/` and anything else are not).
    /// Each is checked again offline (it decodes, sits at its own path, every
    /// proof proves its tombstone and is signed); one that does not moves to
    /// `learned/<publisher>/damaged/`, loudly, and is learned again from the
    /// sources: a learned record never stops the start (review CAT-LEARN-R2 2).
    pub(crate) fn load_learned(&self) -> Result<(), String> {
        let mut st = LearnState::default();
        if let Ok(pubs) = std::fs::read_dir(self.learned_dir()) {
            for p in pubs.flatten() {
                let pdir = p.path();
                if !pdir.is_dir() {
                    eprintln!(
                        "onyx-node: bulk: learn: {} is not a publisher folder: skipped",
                        pdir.display()
                    );
                    continue;
                }
                let files = match std::fs::read_dir(&pdir) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!(
                            "onyx-node: bulk: learn: {} cannot be read ({e}): skipped",
                            pdir.display()
                        );
                        continue;
                    }
                };
                for f in files.flatten() {
                    let path = f.path();
                    if path.is_dir() || path.extension().is_none_or(|e| e != "bin") {
                        continue;
                    }
                    match self.read_learned(&path) {
                        Ok(r) => {
                            st.records
                                .insert((r.publisher.clone(), r.dataset.clone()), r);
                        }
                        Err(why) => set_aside_damaged(&pdir, &path, &why),
                    }
                }
            }
        }
        *self.learned.lock().map_err(|_| "lock")? = st;
        Ok(())
    }

    /// One record file, checked offline.
    fn read_learned(&self, path: &FsPath) -> Result<Learned, String> {
        let bytes = std::fs::read(path).map_err(io)?;
        let r = Learned::try_from_slice(&bytes).map_err(|_| "does not decode".to_string())?;
        if path != self.learned_path(&r.publisher, &r.dataset) {
            return Err("holds another dataset".into());
        }
        for p in r.proofs() {
            offline(&r.publisher, &r.dataset, &p.header, &p.leaf, &p.report)?;
            tombstone_shape(&p.leaf.leaf)?;
        }
        if r.firsts.iter().any(|f| f.number() >= r.newest.number()) {
            return Err("a first proof is not older than the newest".into());
        }
        Ok(r)
    }

    /// Write `r` to `path` (synced temporary file, renamed). Tests may make
    /// every such write fail.
    fn write_learned_file(&self, path: &FsPath, r: &Learned) -> Result<(), String> {
        #[cfg(test)]
        if self.learn_fault.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("a write fault (test)".into());
        }
        let bytes = borsh::to_vec(r).map_err(|_| "encode".to_string())?;
        write_durable(path, &bytes).map_err(io)
    }

    /// Keep `r`: its file first, outside the state's lock, then memory. A
    /// file that cannot be written is logged and written again at the next
    /// recheck; the removal is kept in memory meanwhile (fail closed).
    fn put_learned(&self, r: &Learned) {
        let key = (r.publisher.clone(), r.dataset.clone());
        let written = self.write_learned_file(&self.learned_path(&r.publisher, &r.dataset), r);
        if let Err(e) = &written {
            eprintln!(
                "onyx-node: bulk: learn: {}/{}: the record could not be written ({e}); kept in memory, written again next pass",
                key.0, key.1
            );
        }
        if let Ok(mut st) = self.learned.lock() {
            st.records.insert(key.clone(), r.clone());
            if written.is_ok() {
                st.unsaved.remove(&key);
            } else {
                st.unsaved.insert(key);
            }
        }
    }

    /// Write again the records whose file could not be written.
    fn save_unsaved(&self) {
        let todo: Vec<Learned> = self.learned.lock().map_or_else(
            |_| Vec::new(),
            |st| {
                st.unsaved
                    .iter()
                    .filter_map(|k| st.records.get(k).cloned())
                    .collect()
            },
        );
        for r in todo {
            if self
                .write_learned_file(&self.learned_path(&r.publisher, &r.dataset), &r)
                .is_ok()
            {
                if let Ok(mut st) = self.learned.lock() {
                    st.unsaved.remove(&(r.publisher.clone(), r.dataset.clone()));
                }
            }
        }
    }

    /// The book's final word on every proof of `r`: memory first, so the
    /// dataset serves again even when the file cannot move; then the record
    /// moves to `learned/<publisher>/withdrawn/` (a file left behind is read
    /// again at start and dropped again at the next recheck).
    fn drop_learned(&self, r: &Learned, why: &str) {
        let key = (r.publisher.clone(), r.dataset.clone());
        if let Ok(mut st) = self.learned.lock() {
            // Replaced meanwhile by another proof: that one stays.
            if st
                .records
                .get(&key)
                .is_none_or(|x| x.newest.header.root() != r.newest.header.root())
            {
                return;
            }
            st.records.remove(&key);
            st.unsaved.remove(&key);
            st.said.remove(&key);
        }
        eprintln!(
            "onyx-node: bulk: learn: {}/{}: dropped: the book withdrew catalog {} ({why}) and no source gave another proof; served again",
            r.publisher,
            r.dataset,
            r.newest.number()
        );
        if let Err(e) = self.keep_withdrawn(r).and_then(|()| {
            match std::fs::remove_file(self.learned_path(&r.publisher, &r.dataset)) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(io(e)),
            }
        }) {
            eprintln!(
                "onyx-node: bulk: learn: {}/{}: the dropped record could not move aside ({e}); a start reads it again and the next pass drops it again",
                r.publisher, r.dataset
            );
        }
    }

    /// A withdrawn record's evidence under `learned/<publisher>/withdrawn/`.
    fn keep_withdrawn(&self, r: &Learned) -> Result<(), String> {
        let to = self
            .learned_dir()
            .join(&r.publisher)
            .join("withdrawn")
            .join(format!(
                "{}-{}-{}.bin",
                r.dataset,
                r.newest.number(),
                r.newest.header.root().to_hex()
            ));
        self.write_learned_file(&to, r)
    }

    /// Log `line` for `key` once per change.
    fn learn_say(&self, key: &Key, line: String) {
        let Ok(mut s) = self.learned.lock() else {
            return;
        };
        if s.said.get(key) != Some(&line) {
            eprintln!("onyx-node: bulk: learn: {}/{}: {line}", key.0, key.1);
            s.said.insert(key.clone(), line);
        }
    }

    /// The learned record of a dataset, whatever the mode.
    pub(crate) fn learned_record(&self, key: &Key) -> Result<Option<Learned>, String> {
        Ok(self
            .learned
            .lock()
            .map_err(|_| "lock")?
            .records
            .get(key)
            .cloned())
    }

    /// Learned records kept, whatever the mode.
    pub fn learned_count(&self) -> usize {
        self.learned.lock().map_or(0, |s| s.records.len())
    }

    /// A learned removal of `dataset` while `ONYX_STORAGE_REMOVALS=on`: the
    /// newest proof's catalog number and the `learned` fields of the 410
    /// (`number`, `root`, `checked_at`). Never read by [`Self::removed`],
    /// the erase pass or a storage node's own fetches (review CAT-LEARN-R1
    /// L3). Fails closed on a poisoned lock.
    pub fn learned_removal(
        &self,
        publisher: &str,
        dataset: &str,
    ) -> Result<Option<(u64, Value)>, ApiError> {
        if self.removals_mode != RemovalsMode::On {
            return Ok(None);
        }
        let st = self.learned.lock().map_err(|_| internal("lock"))?;
        Ok(st
            .records
            .get(&(publisher.to_string(), dataset.to_string()))
            .map(|r| {
                (
                    r.newest.number(),
                    json!({
                        "number": r.newest.number(),
                        "root": r.newest.header.root().to_string(),
                        "checked_at": checked_json(r.checked),
                    }),
                )
            }))
    }

    /// The 410 of a learned removal, if any (rows, info, the public uploads).
    pub(crate) fn learned_gone(
        &self,
        publisher: &str,
        dataset: &str,
    ) -> Result<Option<ApiError>, ApiError> {
        Ok(self
            .learned_removal(publisher, dataset)?
            .map(|(n, v)| dataset_removed(n).with_field("learned", v)))
    }

    /// Every dataset a learned removal leaves out of the listing (none unless
    /// `ONYX_STORAGE_REMOVALS=on`).
    pub(crate) fn learned_keys(&self) -> Result<BTreeSet<Key>, ApiError> {
        if self.removals_mode != RemovalsMode::On {
            return Ok(BTreeSet::new());
        }
        Ok(self
            .learned
            .lock()
            .map_err(|_| internal("lock"))?
            .records
            .keys()
            .cloned()
            .collect())
    }

    /// `GET /v1/bulk/catalogs/{publisher}/leaf/{dataset}`: the newest held
    /// catalog's header, its signed report and the dataset's leaf with its
    /// path and neighbours, from the newest catalog's body in memory: no
    /// rebuild, no chain read, no fetch (review CAT-LEARN-R1 L2). A kept
    /// learned proof answers instead when no held catalog of the publisher is
    /// as new (anyone may hand a storage node an old anchored catalog: review
    /// CAT-LEARN-R2 1) or when the newest held catalog does not list the
    /// dataset; otherwise the held catalog answers. `404 bulk_no_leaf` when
    /// neither has the dataset, `409 bulk_publisher_forked` under a fork mark,
    /// 500 when the newest catalog is needed and not held whole.
    pub(crate) fn leaf_answer(&self, publisher: &str, dataset: &str) -> Result<Value, ApiError> {
        if let Some(f) = self.fork_marked(publisher) {
            return Err(publisher_forked(&f));
        }
        let held = {
            let cs = self.catalogs.read().map_err(|_| internal("lock"))?;
            cs.get(publisher)
                .and_then(|m| m.last_key_value())
                .map(|(n, h)| (*n, h.body.clone(), h.report.clone()))
        };
        let learned = if self.removals_mode == RemovalsMode::Off {
            None
        } else {
            self.learned_record(&(publisher.to_string(), dataset.to_string()))
                .map_err(internal)?
        };
        let from_learned =
            |r: Learned| (r.newest.header, r.newest.leaf, r.newest.report, "learned");
        let (header, leaf, report, from) = match (held, learned) {
            (Some((n, _, _)), Some(r)) if r.newest.number() > n => from_learned(r),
            (None, Some(r)) => from_learned(r),
            (None, None) => return Err(no_leaf()),
            (Some((_, body, report)), learned) => {
                let body = body
                    .ok_or_else(|| internal("the newest catalog is not held whole: try again"))?;
                match body.catalog.find(dataset) {
                    Some(i) => {
                        let leaf = body
                            .catalog
                            .leaf_proof_in(&body.tree, i)
                            .ok_or_else(|| internal("catalog leaf proof"))?;
                        (body.catalog.header.clone(), leaf, report, "catalog")
                    }
                    None => from_learned(learned.ok_or_else(no_leaf)?),
                }
            }
        };
        Ok(json!({
            "publisher": publisher,
            "dataset": dataset,
            "number": header.number,
            "root": header.root().to_string(),
            "removed": leaf.leaf.is_tombstone(),
            "header": hex0x(&borsh::to_vec(&header).unwrap_or_default()),
            "leaf": hex0x(&borsh::to_vec(&leaf).unwrap_or_default()),
            "report": hex0x(&borsh::to_vec(&report).unwrap_or_default()),
            "from": from,
        }))
    }

    /// Start a learn pass (waiting for the one running, if any): the chain's
    /// finalized block and its time.
    pub async fn learn_pass_start(&self) -> Result<LearnPass, String> {
        let turn = self.learn_one.clone().lock_owned().await;
        let (at, time) = erase::finalized_time(self.rpc.as_ref(), self.pin.chain_id).await?;
        Ok(LearnPass {
            at,
            time,
            reads: BTreeMap::new(),
            dropping: BTreeSet::new(),
            moved: BTreeSet::new(),
            heard: BTreeSet::new(),
            _turn: turn,
        })
    }

    /// [`read_anchor_at`] of `r` at the pass's block, once per report.
    async fn learn_read(
        &self,
        pass: &mut LearnPass,
        r: &SignedReport,
        signer: &[u8; 20],
    ) -> Result<Read, String> {
        let h = r.hash();
        if let Some(x) = pass.reads.get(&h) {
            return Ok(*x);
        }
        let reading = read_anchor_at(
            self.rpc.as_ref(),
            self.pin.chain_id,
            &self.pin.book,
            r,
            signer,
            pass.at,
        )
        .await?;
        let x = match reading.state {
            AnchorState::Anchored(a) => Read::Anchored {
                recorded_at: a.recorded_at,
            },
            AnchorState::Not(why) if reading.withdrawn => Read::Withdrawn(why),
            AnchorState::Not(why) => Read::Open(why),
        };
        pass.reads.insert(h, x);
        Ok(x)
    }

    /// Read every kept proof again at the pass's block (review CAT-LEARN-R1
    /// M2, R2 4), the newest and each key's first: a proof goes only on the
    /// book's final word on its own catalog (`Reading.withdrawn`); a
    /// suspension, an open dispute or a retired key keeps it, as a catalog
    /// node keeps a taken catalog. When the newest goes, the newest proof
    /// left takes its place; when none is left, the record stays served as
    /// removed until [`Self::learn_finish`], so the pass asks about the
    /// dataset again first ([`LearnPass::reask`]). Records whose file could
    /// not be written are written again; a write that fails never stops the
    /// recheck (R2 3). `Err` is a chain that could not be read: nothing is
    /// dropped.
    pub async fn learn_recheck(&self, pass: &mut LearnPass) -> Result<Recheck, String> {
        self.save_unsaved();
        let records: Vec<Learned> = self
            .learned
            .lock()
            .map_err(|_| "lock")?
            .records
            .values()
            .cloned()
            .collect();
        let mut out = Recheck::default();
        for r in records {
            let key = (r.publisher.clone(), r.dataset.clone());
            let mut gone = BTreeSet::new();
            let mut why = "";
            for p in r.proofs() {
                let read = match cat::verify(&p.header, &p.report) {
                    Ok(signer) => self.learn_read(pass, &p.report, &signer).await?,
                    Err(_) => Read::Withdrawn("the report does not verify"),
                };
                if let Read::Withdrawn(w) = read {
                    gone.insert(p.report.hash());
                    why = w;
                }
            }
            let mut next = r.clone();
            match next.without(&gone) {
                None => {
                    out.withdrawn += 1;
                    pass.dropping.insert(key.clone());
                    self.learn_say(
                        &key,
                        format!(
                            "the book withdrew catalog {} ({why}): asked again before it serves",
                            r.newest.number()
                        ),
                    );
                    continue;
                }
                Some(moved) => {
                    out.kept += 1;
                    out.firsts_gone += gone.len() - usize::from(moved);
                    if moved {
                        out.moved += 1;
                        pass.moved.insert(key.clone());
                        self.learn_say(
                            &key,
                            format!(
                                "the book withdrew catalog {} ({why}): catalog {} stands, asked again",
                                r.newest.number(),
                                next.newest.number()
                            ),
                        );
                    }
                }
            }
            let newest_read = match cat::verify(&next.newest.header, &next.newest.report) {
                Ok(signer) => self.learn_read(pass, &next.newest.report, &signer).await?,
                Err(_) => Read::Open("the report does not verify"),
            };
            match newest_read {
                Read::Anchored { .. } => next.checked = pass.at,
                Read::Open(w) | Read::Withdrawn(w) => {
                    out.open += 1;
                    self.learn_say(
                        &key,
                        format!(
                            "kept: catalog {} does not read anchored now ({w}); kept until the book's final word",
                            next.newest.number()
                        ),
                    );
                }
            }
            if gone.is_empty() {
                // Only `checked` moved: memory only.
                if let Ok(mut st) = self.learned.lock() {
                    if let Some(x) = st.records.get_mut(&key) {
                        if x.newest.header.root() == next.newest.header.root() {
                            x.checked = next.checked;
                        }
                    }
                }
            } else {
                self.put_learned(&next);
            }
        }
        Ok(out)
    }

    /// End a pass: drop every record whose proofs the book withdrew this pass
    /// and that no source replaced. Returns how many.
    pub fn learn_finish(&self, pass: &mut LearnPass) -> usize {
        let mut n = 0;
        for key in std::mem::take(&mut pass.dropping) {
            if let Ok(Some(r)) = self.learned_record(&key) {
                self.drop_learned(&r, "every kept proof withdrawn");
                n += 1;
            }
        }
        n
    }

    /// Check one leaf answer for `publisher`'s `dataset` (from `from`, for the
    /// log) and keep it when it is a tombstone that holds (ADR-0059
    /// amendment 4 item 5a slice 2a, every check of review CAT-LEARN-R1 L1):
    /// this node holds a manifest of the dataset; the offline checks
    /// (publisher, `proves`, the leaf's ids, `cat::verify`); a tombstone at
    /// version 2 or later; the one version rule (a held version `t - 1` must
    /// be the tombstone's `prev`, otherwise a fork: logged, not kept; held
    /// versions at `t` or later are the publisher's fault, logged, and do not
    /// stop it, M1); then the report read anchored at the pass's block
    /// (`read_anchor_at`, which refuses a catalog under a key compromised from
    /// before it). A record being dropped this pass counts as none: any proof
    /// that holds replaces it. `Err` is a chain that could not be read.
    pub async fn learn_from(
        &self,
        pass: &mut LearnPass,
        publisher: &str,
        dataset: &str,
        body: &[u8],
        from: &str,
    ) -> Result<Learn, String> {
        let key: Key = (publisher.to_string(), dataset.to_string());
        let refused = |why: &str| Ok(Learn::Refused(why.to_string()));
        let (header, leaf, report) = match decode_answer(body) {
            Ok(x) => x,
            Err(e) => return refused(&e),
        };
        let held: Vec<(u32, Hash32)> = {
            let ds = self.datasets.read().map_err(|_| "lock")?;
            ds.get(&key)
                .map(|d| {
                    d.versions
                        .iter()
                        .map(|h| (h.manifest.header.version, h.manifest.header.root()))
                        .collect()
                })
                .unwrap_or_default()
        };
        if held.is_empty() {
            return refused("this node holds no manifest of the dataset");
        }
        let signer = match offline(publisher, dataset, &header, &leaf, &report) {
            Ok(s) => s,
            Err(e) => return refused(&e),
        };
        let n = header.number;
        let l = &leaf.leaf;
        if !l.is_tombstone() {
            return Ok(match self.learn_read(pass, &report, &signer).await? {
                Read::Anchored { .. } => {
                    pass.heard.insert(key);
                    Learn::Live(n)
                }
                Read::Open(why) | Read::Withdrawn(why) => Learn::Refused(format!(
                    "a live leaf whose catalog does not read anchored ({why})"
                )),
            });
        }
        if let Err(e) = tombstone_shape(l) {
            return refused(&e);
        }
        // Whatever versions it holds, dataset ids are final (item 2): only
        // the version before the tombstone must be the one it ends.
        if let Some((_, root)) = held.iter().find(|(v, _)| *v == l.version - 1) {
            if *root != l.prev {
                self.learn_say(
                    &key,
                    format!(
                        "fork, not kept: catalog {n} (from {from}) ends version {} after {}, this node holds version {} as {root}",
                        l.version,
                        l.prev,
                        l.version - 1
                    ),
                );
                return refused("a fork: the tombstone's previous root is not the held version's");
            }
        }
        let dropping = pass.dropping.contains(&key);
        let kept = if dropping {
            None
        } else {
            self.learned_record(&key)?
        };
        if let Some(k) = &kept {
            let kn = k.newest.number();
            if kn > n {
                return Ok(Learn::Older(n));
            }
            if kn == n {
                if k.newest.header.root() == header.root() {
                    pass.heard.insert(key);
                    return Ok(Learn::Same(n));
                }
                self.learn_say(
                    &key,
                    format!("another catalog {n} than the kept one (from {from}): not kept"),
                );
                return refused("another catalog with the kept one's number");
            }
        }
        if let Some((v, _)) = held.iter().rev().find(|(v, _)| *v >= l.version) {
            self.learn_say(
                &key,
                format!(
                    "catalog {n} removes it at version {} and this node holds version {v}: the publisher's fault (ADR-0059 amendment 4 item 6), the removal stands",
                    l.version
                ),
            );
        }
        let recorded_at = match self.learn_read(pass, &report, &signer).await? {
            Read::Anchored { recorded_at } => recorded_at,
            Read::Open(why) | Read::Withdrawn(why) => {
                return refused(&format!("its catalog does not read anchored ({why})"))
            }
        };
        let p = Proof {
            header,
            leaf,
            report,
            recorded_at,
            seen_at: pass.time,
        };
        let r = match kept {
            Some(mut k) => {
                k.take_newer(p);
                k.checked = pass.at;
                k
            }
            None => Learned {
                publisher: publisher.to_string(),
                dataset: dataset.to_string(),
                newest: p,
                firsts: Vec::new(),
                checked: pass.at,
            },
        };
        if dropping {
            // The withdrawn record's evidence, before the new one replaces it.
            if let Ok(Some(old)) = self.learned_record(&key) {
                if let Err(e) = self.keep_withdrawn(&old) {
                    eprintln!(
                        "onyx-node: bulk: learn: {}/{}: the withdrawn record could not move aside ({e})",
                        key.0, key.1
                    );
                }
            }
            pass.dropping.remove(&key);
        }
        self.put_learned(&r);
        pass.heard.insert(key.clone());
        self.learn_say(
            &key,
            format!(
                "removed in catalog {n} (from {from}, block {}; first catalog {}){}",
                pass.at.0,
                r.first().number(),
                match self.removals_mode {
                    RemovalsMode::On => "",
                    _ => ": ONYX_STORAGE_REMOVALS is not on, nothing served changes",
                }
            ),
        );
        Ok(Learn::Kept(n))
    }
}

/// Move a damaged record to `<publisher folder>/damaged/`, loudly.
fn set_aside_damaged(pdir: &FsPath, path: &FsPath, why: &str) {
    let name = path.file_name().map(|n| n.to_owned()).unwrap_or_default();
    let to = pdir.join("damaged").join(name);
    let moved =
        std::fs::create_dir_all(pdir.join("damaged")).and_then(|()| std::fs::rename(path, &to));
    match moved {
        Ok(()) => eprintln!(
            "onyx-node: bulk: learn: DAMAGED learned record {} ({why}): moved to {}; the removal is learned again from the sources",
            path.display(),
            to.display()
        ),
        Err(e) => eprintln!(
            "onyx-node: bulk: learn: DAMAGED learned record {} ({why}), not moved ({e}): not used; the removal is learned again from the sources",
            path.display()
        ),
    }
}
