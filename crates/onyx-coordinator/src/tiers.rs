//! Storage tiers for finalized row batches (NORTH-STAR NS13, ROADMAP S11,
//! ADR-0052; draft spec docs/spec/storage-tiers.md).
//!
//! A node keeps each finalized shard's rows at one of three tiers and still
//! answers every query with rows that hash to the root on chain:
//!
//! - **hot**: the row batch in the node's store (`cas/`, as before);
//! - **warm**: the batch in a second, cheaper store, read on demand;
//! - **cold**: no rows at all. The node keeps what a rebuild needs (the
//!   shard's spec, its recipe, label set and attestations: every object other
//!   than rows stays in the hot store for good) and reruns the recipe on a
//!   paid request.
//!
//! Rules this module enforces (THREATS NL10-1..4):
//!
//! - A move never drops the last copy before the next copy is written and
//!   read back under the same key (the store rechecks the hash on read).
//! - A shard goes cold only after one rebuild from its referenced input and
//!   the kept files matched the chain root (NL10-1). A failed rebuild keeps
//!   the shard warm and is returned as an error for the alert.
//! - A pinned input other than the chain data (recipe, label set, cited
//!   attestations) must be in the kept store, or the shard does not go cold
//!   (NL10-2: a salted format-3 set is kept as its file, never its secret).
//! - Only shards over a durable chain input (a block or a block range) may
//!   go cold; a document or a level-2 observation never does.
//! - A recompute is paid and limited: [`RecomputeQueue`] refuses a request
//!   without a hold, caps each account's queue and the work in flight
//!   (NL10-3), and counts requests left unserved past a deadline for the
//!   health watch (NL10-4).
//! - A recompute whose rows do not hash to the chain root is refused and
//!   never stored (a conflict).

use crate::cas::{Cas, CasError, FsCas, Kind, MemCas};
use crate::recompute::{compute, ComputeError, RecomputeEnv};
use onyx_core::{Hash32, ShardSpec, Snapshot};
use onyx_labels::attest::AttestCache;
use onyx_labels::ATTESTATION_EVIDENCE;
use onyx_merkle::MerkleTree;
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::Mutex;

/// Where a shard's rows are kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// In the node's own store.
    Hot,
    /// In the second store.
    Warm,
    /// Not stored: rebuilt on a paid request.
    Cold,
}

impl Tier {
    fn as_str(self) -> &'static str {
        match self {
            Tier::Hot => "hot",
            Tier::Warm => "warm",
            Tier::Cold => "cold",
        }
    }

    fn parse(s: &str) -> Option<Tier> {
        match s {
            "hot" => Some(Tier::Hot),
            "warm" => Some(Tier::Warm),
            "cold" => Some(Tier::Cold),
            _ => None,
        }
    }
}

/// A store that can drop an object (moves between tiers only).
pub trait CasPrune: Cas {
    /// Remove the object; a missing object is not an error.
    fn prune(&self, kind: Kind, key: &Hash32) -> Result<(), CasError>;
}

impl CasPrune for MemCas {
    fn prune(&self, kind: Kind, key: &Hash32) -> Result<(), CasError> {
        self.remove(kind, key);
        Ok(())
    }
}

impl CasPrune for FsCas {
    fn prune(&self, kind: Kind, key: &Hash32) -> Result<(), CasError> {
        match std::fs::remove_file(self.object_path(kind, key)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CasError::Io(e.to_string())),
        }
    }
}

/// Why a move or a recompute did not happen. The shard keeps its tier.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum TierError {
    /// The root is not tracked, or not at the tier the move starts from.
    #[error("shard {0} is {1:?}, not {2:?}")]
    WrongTier(Hash32, Option<Tier>, Tier),
    /// The rows are not readable where the tier says they are.
    #[error("rows of {0} not readable at their tier")]
    Missing(Hash32),
    /// The copy written to the next tier did not read back.
    #[error("copy of {0} did not read back")]
    CopyNotReadBack(Hash32),
    /// The shard's input is not durable chain data, or it has no recipe.
    #[error("not recomputable: {0}")]
    NotRecomputable(&'static str),
    /// A pinned input other than the chain data is not in the kept store.
    #[error("kept input missing: {0}")]
    KeptInputMissing(String),
    /// The input bytes could not be fetched again.
    #[error("source bytes not available")]
    SourceUnavailable,
    /// The rebuild did not produce rows.
    #[error("rebuild failed: {0}")]
    Rebuild(ComputeError),
    /// The rebuilt rows hash to another root than the chain's (a conflict:
    /// never stored, never served).
    #[error("rebuilt root {got} differs from the chain root {chain}")]
    RootMismatch {
        /// Root of the rebuilt rows.
        got: Hash32,
        /// Finalized root on chain.
        chain: Hash32,
    },
    /// Storage failure.
    #[error("store: {0}")]
    Store(CasError),
    /// The tier file could not be read or written.
    #[error("tier file: {0}")]
    File(String),
}

/// Counts for `/v1/health` (ADR-0052).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TierCounts {
    /// Shards with rows in the hot store.
    pub hot: usize,
    /// Shards with rows in the warm store.
    pub warm: usize,
    /// Shards with no rows stored.
    pub cold: usize,
}

/// True when `spec` can be rebuilt byte for byte later: it has a recipe and
/// its input is a block or a block range of a chain, fetched again by height
/// and checked against the pinned hashes. Documents (web pages change) and
/// lanes without a recipe never go cold.
pub fn recomputable(spec: &ShardSpec) -> Result<(), TierError> {
    if spec.recipe.is_none() || spec.runtime.is_none() {
        return Err(TierError::NotRecomputable("no recipe"));
    }
    match spec.source.snapshot {
        Snapshot::Block { .. } | Snapshot::BlockRange { .. } => Ok(()),
        Snapshot::Document { .. } => Err(TierError::NotRecomputable(
            "a document input may change; only chain data is fetched again",
        )),
    }
}

/// Every pinned input of `spec` other than the chain data must be in `kept`
/// (NL10-2): the recipe, the label set file and the attestations it cites.
pub fn kept_inputs_present(spec: &ShardSpec, kept: &dyn Cas) -> Result<(), TierError> {
    if let Some(r) = spec.recipe {
        if !kept.has(Kind::Recipe, &r) {
            return Err(TierError::KeptInputMissing(format!("recipe {r}")));
        }
    }
    if let Some(pin) = &spec.label_set {
        let set = crate::cas::CasExt::label_set(kept, &pin.content_hash).ok_or_else(|| {
            TierError::KeptInputMissing(format!("label set {}", pin.content_hash))
        })?;
        for e in set
            .labels
            .iter()
            .flat_map(|l| l.evidence.iter())
            .filter(|e| e.kind == ATTESTATION_EVIDENCE)
        {
            if !kept.has(Kind::Attestation, &e.content_hash) {
                return Err(TierError::KeptInputMissing(format!(
                    "attestation {}",
                    e.content_hash
                )));
            }
        }
    }
    Ok(())
}

/// Rows of `spec` rebuilt from its input and the kept files, accepted only
/// when they hash to `chain_root`.
fn rebuild(
    spec: &ShardSpec,
    env: &RecomputeEnv<'_>,
    cache: &AttestCache,
    chain_root: &Hash32,
) -> Result<Vec<Vec<u8>>, TierError> {
    recomputable(spec)?;
    kept_inputs_present(spec, env.cas)?;
    let source = (env.source)(spec).ok_or(TierError::SourceUnavailable)?;
    let rows =
        compute(spec, source, env.reg, env.cas, env.host, cache).map_err(TierError::Rebuild)?;
    let got = MerkleTree::from_rows(&rows).root();
    if got != *chain_root {
        return Err(TierError::RootMismatch {
            got,
            chain: *chain_root,
        });
    }
    Ok(rows)
}

/// A node's row store split into a hot and a warm store, with the tier of
/// every tracked shard. It is a [`Cas`]: rows are read from the hot store,
/// then the warm one; every other kind lives in the hot store and never
/// moves, so a cold shard's kept inputs stay readable.
pub struct TieredStore<H: CasPrune, W: CasPrune> {
    hot: H,
    warm: W,
    tiers: Mutex<BTreeMap<Hash32, Tier>>,
}

impl<H: CasPrune, W: CasPrune> TieredStore<H, W> {
    /// A store over `hot` and `warm` with no tracked shard.
    pub fn new(hot: H, warm: W) -> Self {
        TieredStore {
            hot,
            warm,
            tiers: Mutex::new(BTreeMap::new()),
        }
    }

    /// The hot store (kept inputs live here).
    pub fn hot(&self) -> &H {
        &self.hot
    }

    /// Tier of `root`, if tracked.
    pub fn tier(&self, root: &Hash32) -> Option<Tier> {
        self.tiers.lock().ok()?.get(root).copied()
    }

    /// Counts per tier.
    pub fn counts(&self) -> TierCounts {
        let mut c = TierCounts::default();
        if let Ok(t) = self.tiers.lock() {
            for tier in t.values() {
                match tier {
                    Tier::Hot => c.hot += 1,
                    Tier::Warm => c.warm += 1,
                    Tier::Cold => c.cold += 1,
                }
            }
        }
        c
    }

    fn set(&self, root: Hash32, tier: Tier) {
        if let Ok(mut t) = self.tiers.lock() {
            t.insert(root, tier);
        }
    }

    fn expect(&self, root: &Hash32, want: Tier) -> Result<(), TierError> {
        match self.tier(root) {
            Some(t) if t == want => Ok(()),
            other => Err(TierError::WrongTier(*root, other, want)),
        }
    }

    /// Copy `root`'s rows from `from` to `to`, read them back, then drop them
    /// from `from`: the last copy is never dropped first.
    fn copy_then_drop(
        &self,
        root: &Hash32,
        from: &dyn CasPrune,
        to: &dyn CasPrune,
    ) -> Result<(), TierError> {
        let bytes = from
            .get(Kind::Rows, root)
            .ok_or(TierError::Missing(*root))?;
        let key = to.put(Kind::Rows, &bytes).map_err(TierError::Store)?;
        if key != *root || to.get(Kind::Rows, root).as_deref() != Some(&bytes[..]) {
            return Err(TierError::CopyNotReadBack(*root));
        }
        from.prune(Kind::Rows, root).map_err(TierError::Store)
    }

    /// hot to warm.
    pub fn to_warm(&self, root: &Hash32) -> Result<(), TierError> {
        self.expect(root, Tier::Hot)?;
        self.copy_then_drop(root, &self.hot, &self.warm)?;
        self.set(*root, Tier::Warm);
        Ok(())
    }

    /// warm to hot (a shard queried often again).
    pub fn warm_to_hot(&self, root: &Hash32) -> Result<(), TierError> {
        self.expect(root, Tier::Warm)?;
        self.copy_then_drop(root, &self.warm, &self.hot)?;
        self.set(*root, Tier::Hot);
        Ok(())
    }

    /// warm to cold: only after a rebuild from the referenced input and the
    /// kept files matched `chain_root` (the finalized root read on chain).
    /// On any error the shard stays warm and the caller raises an alert.
    pub fn to_cold(
        &self,
        root: &Hash32,
        spec: &ShardSpec,
        env: &RecomputeEnv<'_>,
        cache: &AttestCache,
        chain_root: &Hash32,
    ) -> Result<(), TierError> {
        self.expect(root, Tier::Warm)?;
        if root != chain_root {
            return Err(TierError::RootMismatch {
                got: *root,
                chain: *chain_root,
            });
        }
        rebuild(spec, env, cache, chain_root)?;
        self.warm
            .prune(Kind::Rows, root)
            .map_err(TierError::Store)?;
        self.hot.prune(Kind::Rows, root).map_err(TierError::Store)?;
        self.set(*root, Tier::Cold);
        Ok(())
    }

    /// A cold shard's rows rebuilt on request (the caller holds the paid
    /// ticket from [`RecomputeQueue`]). The rows are stored hot only when they
    /// hash to `chain_root`; a mismatch is refused and nothing is stored.
    pub fn recompute(
        &self,
        root: &Hash32,
        spec: &ShardSpec,
        env: &RecomputeEnv<'_>,
        cache: &AttestCache,
        chain_root: &Hash32,
    ) -> Result<Vec<Vec<u8>>, TierError> {
        self.expect(root, Tier::Cold)?;
        let rows = rebuild(spec, env, cache, chain_root)?;
        let bytes =
            onyx_core::to_canonical_bytes(&rows).map_err(|e| TierError::File(e.to_string()))?;
        let key = self.hot.put(Kind::Rows, &bytes).map_err(TierError::Store)?;
        if key != *chain_root || self.hot.get(Kind::Rows, &key).is_none() {
            return Err(TierError::CopyNotReadBack(*root));
        }
        self.set(*root, Tier::Hot);
        Ok(rows)
    }

    /// Write the tier of every tracked shard to `path` (one JSON object,
    /// root hex to tier), through a new file renamed into place.
    pub fn save(&self, path: &Path) -> Result<(), TierError> {
        let map: serde_json::Map<String, serde_json::Value> = self
            .tiers
            .lock()
            .map_err(|e| TierError::File(e.to_string()))?
            .iter()
            .map(|(k, t)| (hex::encode(k.0), serde_json::Value::from(t.as_str())))
            .collect();
        let text = serde_json::Value::Object(map).to_string();
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text)
            .and_then(|()| std::fs::rename(&tmp, path))
            .map_err(|e| TierError::File(e.to_string()))
    }

    /// Read the tiers saved by [`Self::save`]. A tier whose rows are not where
    /// it says (hot or warm) is refused, so a lost copy is found at start.
    pub fn load(&self, path: &Path) -> Result<(), TierError> {
        let text = std::fs::read_to_string(path).map_err(|e| TierError::File(e.to_string()))?;
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| TierError::File(e.to_string()))?;
        let obj = v
            .as_object()
            .ok_or_else(|| TierError::File("not an object".into()))?;
        let mut out = BTreeMap::new();
        for (k, t) in obj {
            let mut key = [0u8; 32];
            hex::decode_to_slice(k, &mut key).map_err(|e| TierError::File(e.to_string()))?;
            let key = Hash32(key);
            let tier = t
                .as_str()
                .and_then(Tier::parse)
                .ok_or_else(|| TierError::File(format!("bad tier for {k}")))?;
            let present = match tier {
                Tier::Hot => self.hot.has(Kind::Rows, &key),
                Tier::Warm => self.warm.has(Kind::Rows, &key),
                Tier::Cold => true,
            };
            if !present {
                return Err(TierError::Missing(key));
            }
            out.insert(key, tier);
        }
        *self
            .tiers
            .lock()
            .map_err(|e| TierError::File(e.to_string()))? = out;
        Ok(())
    }
}

impl<H: CasPrune, W: CasPrune> Cas for TieredStore<H, W> {
    fn put(&self, kind: Kind, bytes: &[u8]) -> Result<Hash32, CasError> {
        let key = self.hot.put(kind, bytes)?;
        if kind == Kind::Rows {
            if let Ok(mut t) = self.tiers.lock() {
                t.insert(key, Tier::Hot);
            }
        }
        Ok(key)
    }

    fn get(&self, kind: Kind, key: &Hash32) -> Option<Vec<u8>> {
        match kind {
            Kind::Rows => self.hot.get(kind, key).or_else(|| self.warm.get(kind, key)),
            _ => self.hot.get(kind, key),
        }
    }
}

/// The paid hold that backs a recompute request (the NL8 model: a prepaid
/// voucher held, charged only when the rows are served, released otherwise).
/// Opaque here: charging and releasing are the node's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct HoldId(pub Hash32);

/// Limits on recompute work (NL10-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecomputeLimits {
    /// Requests one account may have queued or running.
    pub per_account: usize,
    /// Recomputes running at once on this node.
    pub in_flight: usize,
    /// Requests waiting in all.
    pub queue: usize,
}

/// A recompute request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// The paying account (an address).
    pub account: [u8; 20],
    /// Root of the cold shard.
    pub root: Hash32,
    /// Its hold.
    pub hold: HoldId,
    /// When it was asked (unix seconds).
    pub asked_unix: u64,
}

/// Why a recompute request was refused. Nothing runs.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum QueueError {
    /// No paid hold backs the request (NL10-3: nobody recomputes for free).
    #[error("a recompute needs a paid hold")]
    Unpaid,
    /// The account already has `per_account` requests open.
    #[error("too many open requests for this account")]
    AccountFull,
    /// The node's queue is full.
    #[error("recompute queue full")]
    QueueFull,
    /// The same shard is already queued or running for this account.
    #[error("already requested")]
    Duplicate,
}

/// Recompute requests: paid, capped per account, a fixed number running at
/// once, oldest first.
pub struct RecomputeQueue {
    limits: RecomputeLimits,
    waiting: VecDeque<Job>,
    running: Vec<Job>,
}

impl RecomputeQueue {
    /// An empty queue with `limits`.
    pub fn new(limits: RecomputeLimits) -> Self {
        RecomputeQueue {
            limits,
            waiting: VecDeque::new(),
            running: Vec::new(),
        }
    }

    fn open_for<'a>(&'a self, account: &'a [u8; 20]) -> impl Iterator<Item = &'a Job> + 'a {
        self.waiting
            .iter()
            .chain(self.running.iter())
            .filter(move |j| &j.account == account)
    }

    /// Queue a request. `hold` is `None` when no paid hold backs it: refused,
    /// and nothing is queued.
    pub fn request(
        &mut self,
        account: [u8; 20],
        root: Hash32,
        hold: Option<HoldId>,
        now: u64,
    ) -> Result<(), QueueError> {
        let hold = hold.ok_or(QueueError::Unpaid)?;
        if self.open_for(&account).any(|j| j.root == root) {
            return Err(QueueError::Duplicate);
        }
        if self.open_for(&account).count() >= self.limits.per_account {
            return Err(QueueError::AccountFull);
        }
        if self.waiting.len() >= self.limits.queue {
            return Err(QueueError::QueueFull);
        }
        self.waiting.push_back(Job {
            account,
            root,
            hold,
            asked_unix: now,
        });
        Ok(())
    }

    /// The next job to run, if fewer than `in_flight` run now.
    pub fn start(&mut self) -> Option<Job> {
        if self.running.len() >= self.limits.in_flight {
            return None;
        }
        let j = self.waiting.pop_front()?;
        self.running.push(j.clone());
        Some(j)
    }

    /// A running job ended (served, or refused and its hold released).
    pub fn finish(&mut self, account: &[u8; 20], root: &Hash32) {
        self.running
            .retain(|j| !(&j.account == account && &j.root == root));
    }

    /// Jobs running now.
    pub fn running(&self) -> usize {
        self.running.len()
    }

    /// Requests asked more than `deadline_secs` ago and not finished: the
    /// health watch alarms on a node that never serves cold requests (NL10-4).
    pub fn overdue(&self, now: u64, deadline_secs: u64) -> usize {
        self.waiting
            .iter()
            .chain(self.running.iter())
            .filter(|j| now.saturating_sub(j.asked_unix) > deadline_secs)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cas::CasExt;
    use crate::test_fixture::{World, SOURCE};
    use onyx_core::{keccak256, LabelSetRef};

    fn store() -> TieredStore<MemCas, MemCas> {
        TieredStore::new(MemCas::new(), MemCas::new())
    }

    /// World's kept inputs copied into the tiered store's hot side, and the
    /// shard's true rows stored hot.
    fn hot_shard(w: &World, t: &TieredStore<MemCas, MemCas>, n: u8) -> (ShardSpec, Hash32) {
        let spec = w.spec(n);
        let wasm = crate::test_fixture::recipe_ok();
        t.put(Kind::Recipe, &wasm).expect("recipe");
        let src = |_: &ShardSpec| Some(SOURCE.to_vec());
        let rows = compute(
            &spec,
            (src)(&spec).expect("src"),
            &w.reg,
            &w.cas,
            &w.host,
            &AttestCache::default(),
        )
        .expect("rows");
        let root = t.put_rows(&rows).expect("put");
        (spec, root)
    }

    fn env<'a>(
        w: &'a World,
        t: &'a TieredStore<MemCas, MemCas>,
        src: &'a dyn Fn(&ShardSpec) -> Option<Vec<u8>>,
    ) -> RecomputeEnv<'a> {
        RecomputeEnv {
            reg: &w.reg,
            cas: t,
            host: &w.host,
            source: src,
        }
    }

    /// NS13 Done-when: hot to warm to cold, and a cold shard rebuilt on
    /// request answers with rows that hash to the chain root.
    #[test]
    fn hot_warm_cold_and_back_by_recompute() {
        let w = World::new();
        let t = store();
        let (spec, root) = hot_shard(&w, &t, 1);
        assert_eq!(t.tier(&root), Some(Tier::Hot));
        t.to_warm(&root).expect("warm");
        assert_eq!(t.tier(&root), Some(Tier::Warm));
        assert!(
            !t.hot().has(Kind::Rows, &root),
            "hot copy dropped after the warm one read back"
        );
        assert!(t.rows(&root).is_some(), "warm rows are still served");
        let src = |_: &ShardSpec| Some(SOURCE.to_vec());
        let e = env(&w, &t, &src);
        let cache = AttestCache::default();
        t.to_cold(&root, &spec, &e, &cache, &root).expect("cold");
        assert_eq!(t.tier(&root), Some(Tier::Cold));
        assert!(t.rows(&root).is_none(), "a cold shard keeps no rows");
        assert_eq!(
            t.counts(),
            TierCounts {
                hot: 0,
                warm: 0,
                cold: 1
            }
        );
        let rows = t
            .recompute(&root, &spec, &e, &cache, &root)
            .expect("recompute");
        assert_eq!(MerkleTree::from_rows(&rows).root(), root);
        assert_eq!(t.tier(&root), Some(Tier::Hot));
        assert_eq!(t.rows(&root), Some(rows));
        // warm back to hot keeps exactly one copy.
        t.to_warm(&root).expect("warm again");
        t.warm_to_hot(&root).expect("hot again");
        assert_eq!(t.tier(&root), Some(Tier::Hot));
        assert!(t.hot().has(Kind::Rows, &root));
    }

    /// NL10-1: a shard whose input reference does not rebuild stays warm.
    #[test]
    fn a_shard_with_a_bad_reference_stays_warm() {
        let w = World::new();
        let t = store();
        let (spec, root) = hot_shard(&w, &t, 2);
        t.to_warm(&root).expect("warm");
        let cache = AttestCache::default();
        // The source cannot be fetched again.
        let none = |_: &ShardSpec| None;
        assert_eq!(
            t.to_cold(&root, &spec, &env(&w, &t, &none), &cache, &root),
            Err(TierError::SourceUnavailable)
        );
        // Other bytes than the pinned ones (the content hash check refuses them).
        let other = |_: &ShardSpec| Some(b"other bytes".to_vec());
        assert!(matches!(
            t.to_cold(&root, &spec, &env(&w, &t, &other), &cache, &root),
            Err(TierError::Rebuild(_))
        ));
        // A spec that rebuilds to other rows (another shard's params).
        let src = |_: &ShardSpec| Some(SOURCE.to_vec());
        let mut wrong = spec.clone();
        wrong.recipe = Some(keccak256(&crate::test_fixture::recipe_loop()));
        t.put(Kind::Recipe, &crate::test_fixture::recipe_loop())
            .expect("loop recipe");
        assert!(t
            .to_cold(&root, &wrong, &env(&w, &t, &src), &cache, &root)
            .is_err());
        assert_eq!(t.tier(&root), Some(Tier::Warm));
        assert!(t.rows(&root).is_some(), "the warm copy is kept");
    }

    /// NL10-2: a pinned label set (a salted format-3 set included) that is
    /// not in the kept store keeps the shard warm.
    #[test]
    fn a_shard_without_its_kept_label_set_stays_warm() {
        let w = World::new();
        let t = store();
        let (mut spec, root) = hot_shard(&w, &t, 3);
        spec.label_set = Some(LabelSetRef {
            name: "test.onyx.entities.btc".into(),
            version: 3,
            content_hash: keccak256(b"a salted set file this node lost"),
        });
        t.to_warm(&root).expect("warm");
        let src = |_: &ShardSpec| Some(SOURCE.to_vec());
        assert!(matches!(
            t.to_cold(&root, &spec, &env(&w, &t, &src), &AttestCache::default(), &root),
            Err(TierError::KeptInputMissing(m)) if m.starts_with("label set")
        ));
        assert_eq!(t.tier(&root), Some(Tier::Warm));
    }

    /// Only chain data goes cold: a document input never does.
    #[test]
    fn a_document_input_never_goes_cold() {
        let w = World::new();
        let t = store();
        let (mut spec, root) = hot_shard(&w, &t, 4);
        spec.source.snapshot = Snapshot::Document {
            published_unix: 1,
            content_hash: keccak256(SOURCE),
        };
        t.to_warm(&root).expect("warm");
        let src = |_: &ShardSpec| Some(SOURCE.to_vec());
        assert!(matches!(
            t.to_cold(
                &root,
                &spec,
                &env(&w, &t, &src),
                &AttestCache::default(),
                &root
            ),
            Err(TierError::NotRecomputable(_))
        ));
        let mut no_recipe = spec;
        no_recipe.recipe = None;
        assert!(recomputable(&no_recipe).is_err());
    }

    /// A recompute that differs from the chain root is refused and nothing
    /// is stored; a cold move against another chain root is refused too.
    #[test]
    fn a_recompute_that_differs_from_the_chain_root_is_never_stored() {
        let w = World::new();
        let t = store();
        let (spec, root) = hot_shard(&w, &t, 5);
        t.to_warm(&root).expect("warm");
        let src = |_: &ShardSpec| Some(SOURCE.to_vec());
        let e = env(&w, &t, &src);
        let cache = AttestCache::default();
        let other_root = keccak256(b"another finalized root");
        assert!(matches!(
            t.to_cold(&root, &spec, &e, &cache, &other_root),
            Err(TierError::RootMismatch { .. })
        ));
        t.to_cold(&root, &spec, &e, &cache, &root).expect("cold");
        // The fixture recipe ignores params, so craft the conflict through the
        // chain root the caller read.
        assert!(matches!(
            t.recompute(&root, &spec, &e, &cache, &other_root),
            Err(TierError::RootMismatch { .. })
        ));
        assert_eq!(t.tier(&root), Some(Tier::Cold));
        assert!(t.rows(&root).is_none());
    }

    /// Moves only start from the tier they name.
    #[test]
    fn moves_start_only_from_their_tier() {
        let w = World::new();
        let t = store();
        let (spec, root) = hot_shard(&w, &t, 6);
        let src = |_: &ShardSpec| Some(SOURCE.to_vec());
        let e = env(&w, &t, &src);
        let cache = AttestCache::default();
        assert!(matches!(
            t.warm_to_hot(&root),
            Err(TierError::WrongTier(..))
        ));
        assert!(matches!(
            t.to_cold(&root, &spec, &e, &cache, &root),
            Err(TierError::WrongTier(..))
        ));
        assert!(matches!(
            t.recompute(&root, &spec, &e, &cache, &root),
            Err(TierError::WrongTier(..))
        ));
        let unknown = keccak256(b"untracked");
        assert!(matches!(
            t.to_warm(&unknown),
            Err(TierError::WrongTier(_, None, _))
        ));
    }

    /// The tier file round-trips, and a tier whose rows are gone is found at
    /// load.
    #[test]
    fn tiers_survive_a_restart_and_a_lost_copy_is_found() {
        let w = World::new();
        let dir = std::env::temp_dir().join(format!("onyx-tiers-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("tiers.json");
        let t = TieredStore::new(FsCas::new(dir.join("hot")), FsCas::new(dir.join("warm")));
        let spec = w.spec(7);
        t.put(Kind::Recipe, &crate::test_fixture::recipe_ok())
            .expect("recipe");
        let rows = compute(
            &spec,
            SOURCE.to_vec(),
            &w.reg,
            &w.cas,
            &w.host,
            &AttestCache::default(),
        )
        .expect("rows");
        let root = t.put_rows(&rows).expect("rows");
        t.to_warm(&root).expect("warm");
        t.save(&path).expect("save");
        let again = TieredStore::new(FsCas::new(dir.join("hot")), FsCas::new(dir.join("warm")));
        again.load(&path).expect("load");
        assert_eq!(again.tier(&root), Some(Tier::Warm));
        assert_eq!(again.rows(&root), Some(rows));
        // The warm copy is lost: load refuses the file.
        t.warm.prune(Kind::Rows, &root).expect("prune");
        let third = TieredStore::new(FsCas::new(dir.join("hot")), FsCas::new(dir.join("warm")));
        assert_eq!(third.load(&path), Err(TierError::Missing(root)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    const LIMITS: RecomputeLimits = RecomputeLimits {
        per_account: 2,
        in_flight: 1,
        queue: 3,
    };

    /// NL10-3: an unpaid cold request starts no recompute.
    #[test]
    fn an_unpaid_cold_request_starts_no_recompute() {
        let mut q = RecomputeQueue::new(LIMITS);
        assert_eq!(
            q.request([1; 20], keccak256(b"r"), None, 0),
            Err(QueueError::Unpaid)
        );
        assert_eq!(q.start(), None);
        assert_eq!(q.running(), 0);
    }

    /// NL10-3: per-account cap, queue cap, one at a time; NL10-4: overdue
    /// requests are counted.
    #[test]
    fn recomputes_are_limited_and_overdue_ones_counted() {
        let mut q = RecomputeQueue::new(LIMITS);
        let hold = Some(HoldId(keccak256(b"hold")));
        let (a, b) = ([1; 20], [2; 20]);
        q.request(a, keccak256(b"r1"), hold, 0).expect("a1");
        assert_eq!(
            q.request(a, keccak256(b"r1"), hold, 0),
            Err(QueueError::Duplicate)
        );
        q.request(a, keccak256(b"r2"), hold, 0).expect("a2");
        assert_eq!(
            q.request(a, keccak256(b"r3"), hold, 0),
            Err(QueueError::AccountFull)
        );
        q.request(b, keccak256(b"r1"), hold, 10).expect("b1");
        assert_eq!(
            q.request([3; 20], keccak256(b"r9"), hold, 10),
            Err(QueueError::QueueFull)
        );
        let first = q.start().expect("one runs");
        assert_eq!(first.root, keccak256(b"r1"));
        assert_eq!(q.start(), None, "only one in flight");
        assert_eq!(q.overdue(100, 60), 3);
        q.finish(&first.account, &first.root);
        assert_eq!(q.running(), 0);
        assert!(q.start().is_some());
        assert_eq!(
            q.overdue(65, 60),
            1,
            "the request asked at 10 s is not overdue yet"
        );
    }
}
