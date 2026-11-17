//! Successor links on a node (ADR-0059 amendment 6 draft 3, slice 2; THREATS
//! CAT-SUCC-1 to CAT-SUCC-11, build reviews SUCC-B2 to SUCC-B5).
//!
//! A publisher that moves a dataset to a new id ends the old id with a
//! tombstone and adds a row to its links dataset (`onyx.successors`, or a
//! later generation) in the same catalog. This node keeps, per publisher, an
//! index for its newest held catalog; nothing of it is stored on disk. After
//! review SUCC-B4 the index is bounded and built eagerly:
//!
//! - **Bounded input.** A links version is usable only with at most
//!   `MAX_LINK_ROWS` rows, `LINKS_MAX_BYTES` bytes and `MAX_LINK_SHARDS`
//!   shards (onyx-bulk; readers apply the same rule). The bytes are the
//!   header's claim, so every load checks the shard file's size against its
//!   entry before it reads it (SUCC-B5 H1), and a job loads each distinct
//!   shard file once (M1): a job reads at most `LINKS_MAX_BYTES` from at most
//!   `MAX_LINK_SHARDS` files.
//! - **One worker, fed by uploads.** One background task per node builds
//!   indexes, one publisher at a time, from a first-in first-out queue that
//!   holds each publisher at most once. A publisher is queued when this node
//!   takes a catalog of it (or sets one aside), or the one manifest header or
//!   shard its pending index waits for (SUCC-B5 L1); and at start, those
//!   whose catalog removes a dataset first (M2). A job reads the whole links
//!   version its newest catalog lists, once, unless the index is built for
//!   that catalog already or the catalog removes nothing (then nothing can
//!   act and nothing is read). No passes, no part reads.
//! - **Small result.** The index keeps only the links that act in its
//!   catalog (one row at most per tombstone, `Links::only_for`), never every
//!   row: no cache and no eviction.
//! - **Reads never build and never wait.** The 410, info, the listing and
//!   the successor route use the index only when it is built for the catalog
//!   they answer from; otherwise they answer `pending` (or leave the counts
//!   out). No read queues a job, and none awaits the chain: the route reads
//!   page 0 of the links from the held version (refused when the last
//!   anchor read found it no longer held: SUCC-B5 L3), and the fork mark as
//!   it stands (read again in the background).
//! - **Pending.** A manifest or shard not held leaves the index `pending`,
//!   waiting for exactly it; a shard that does not read, or whose file is
//!   not its entry's size, leaves it `pending` until the publisher's next
//!   catalog. A worker stuck on I/O stops every build but no read (accepted:
//!   THREATS SUCC-B4). Index reads leave the shard tiers as they are and take
//!   none of the readers' load permits.
//!
//! The index feeds the `successor` field of a removed dataset's 410, the
//! `GET /v1/bulk/{publisher}/{dataset}/successor` answer (every proof a
//! reader needs to follow the chain itself, `onyx-check --successor`), and
//! `replaces` and `moved` in info and the listing.

use super::*;
use onyx_bulk::catalog::AbsenceProof;
use onyx_bulk::row::{decode_row, present};
use onyx_bulk::successor::{
    self as sx, Chain, End, FollowError, GenFault, GenState, Links, Listing, MAX_LINK_GENERATIONS,
};
use onyx_bulk::ManifestHeader;
use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Weak;

/// Most replaced ids `info` names (with the full count beside them).
pub const MAX_REPLACES_SHOWN: usize = 64;

/// Where a publisher's links stand for one catalog.
#[derive(Clone, Debug)]
pub(crate) enum LinksState {
    /// No usable generation: every chain ends `no_links`.
    None,
    /// The node lacks a header or a shard of what the catalog lists, or a
    /// shard did not read. With the used generation's id when it is known
    /// (a fork of it shows at once).
    Pending(Option<String>),
    /// Every row of the used version was read: the links that act in the
    /// catalog.
    Ready {
        generation: u32,
        id: String,
        root: Hash32,
        links: Arc<Links>,
    },
}

/// A publisher's links index, for one catalog.
#[derive(Clone, Debug)]
pub(crate) struct Index {
    /// The catalog it was built for: number and root.
    catalog: (u64, Hash32),
    state: LinksState,
    /// Target id -> the old ids whose first hop is it, in id order.
    replaces: Arc<BTreeMap<String, Vec<String>>>,
    /// Removed datasets with a link that names a target.
    moved: usize,
}

/// What a pending index waits for: only exactly that queues its next job
/// (review SUCC-B5 L1).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Wait {
    /// The manifest of links version `root` of dataset `id`.
    Header { id: String, root: Hash32 },
    /// Shard `shard`, entry `next` of links version `root` (every entry
    /// before it is held).
    Shard {
        root: Hash32,
        next: usize,
        shard: Hash32,
    },
}

/// One publisher's links on this node.
#[derive(Default, Debug)]
struct PubSucc {
    index: Option<Index>,
    wait: Option<Wait>,
}

/// Every publisher's links, and who waits for which shard.
#[derive(Default)]
struct State {
    by: BTreeMap<String, PubSucc>,
    /// Shard root -> the publishers whose index waits for it (review
    /// SUCC-B5 L4: an upload looks up its root, not every publisher).
    shards: BTreeMap<Hash32, BTreeSet<String>>,
}

impl State {
    fn set_wait(&mut self, publisher: &str, wait: Option<Wait>) {
        let p = self.by.entry(publisher.to_string()).or_default();
        if let Some(Wait::Shard { shard, .. }) = &p.wait {
            if let Some(w) = self.shards.get_mut(shard) {
                w.remove(publisher);
                if w.is_empty() {
                    self.shards.remove(shard);
                }
            }
        }
        if let Some(Wait::Shard { shard, .. }) = &wait {
            self.shards
                .entry(*shard)
                .or_default()
                .insert(publisher.to_string());
        }
        p.wait = wait;
    }
}

/// The publishers to build, first in first out, each at most once (a set
/// beside the order: review SUCC-B5 L4); and the one being built.
#[derive(Default)]
struct Queue {
    order: VecDeque<String>,
    queued: BTreeSet<String>,
    busy: Option<String>,
}

/// Every publisher's links index, and the worker that builds them.
pub(crate) struct Succ {
    state: Mutex<State>,
    queue: Mutex<Queue>,
    /// Wakes the worker when a publisher is queued.
    wake: Arc<tokio::sync::Notify>,
    started: AtomicBool,
    /// Bumped at the end of every job (tests wait on it).
    done: tokio::sync::watch::Sender<u64>,
    /// Report digests whose stale "not held" verdict is being read again
    /// for page 0 now: one read per window (review SUCC-B6 M1).
    rereads: Mutex<BTreeSet<Hash32>>,
    /// Tests: shard loads tried by builds.
    #[cfg(test)]
    reads: std::sync::atomic::AtomicU64,
    /// Tests: the publishers built, in order.
    #[cfg(test)]
    jobs: Mutex<Vec<String>>,
    /// Tests: the worker takes a permit of this before each job.
    #[cfg(test)]
    gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    /// Tests: publishers whose next job panics.
    #[cfg(test)]
    panic_next: Mutex<BTreeSet<String>>,
    /// Tests: a job takes a permit of this after it read and before it
    /// stores what it found; and whether one waits there now.
    #[cfg(test)]
    store_gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    #[cfg(test)]
    at_store_gate: AtomicBool,
}

impl Default for Succ {
    fn default() -> Self {
        Succ {
            state: Mutex::new(State::default()),
            queue: Mutex::new(Queue::default()),
            wake: Arc::new(tokio::sync::Notify::new()),
            started: AtomicBool::new(false),
            done: tokio::sync::watch::Sender::new(0),
            rereads: Mutex::new(BTreeSet::new()),
            #[cfg(test)]
            reads: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            jobs: Mutex::new(Vec::new()),
            #[cfg(test)]
            gate: Mutex::new(None),
            #[cfg(test)]
            panic_next: Mutex::new(BTreeSet::new()),
            #[cfg(test)]
            store_gate: Mutex::new(None),
            #[cfg(test)]
            at_store_gate: AtomicBool::new(false),
        }
    }
}

/// An index as it stands at a read.
enum Now<'a> {
    /// Built: the links (`None`: no usable generation).
    Settled(Option<&'a Links>),
    /// This node lacks what the catalog lists, or has not built it yet.
    Pending,
    /// The links dataset is forked here.
    Forked,
}

/// The id an end names, or `start` when it names none.
fn end_id<'a>(end: &'a End, start: &'a str) -> &'a str {
    match end {
        End::NotRemoved { dataset, .. }
        | End::Live { dataset, .. }
        | End::Removed { dataset }
        | End::Retracted { dataset, .. }
        | End::NotListed { dataset }
        | End::Cycle { dataset }
        | End::TooLong { dataset } => dataset,
        End::NoLinks => start,
    }
}

fn listing(c: &Catalog, id: &str) -> Listing {
    c.find(id)
        .map_or(Listing::Absent, |i| Listing::Leaf(c.leaves[i].clone()))
}

fn pending() -> Value {
    json!({"status": "pending"})
}

fn forked_status() -> Value {
    json!({"status": "forked"})
}

/// The worker: builds the queued publishers one at a time, each job in a
/// task of its own (a panic ends that job, not the worker). It holds the
/// store only while it takes or runs a job.
async fn succ_work(store: Weak<BulkStore>, wake: Arc<tokio::sync::Notify>) {
    loop {
        let Some(s) = store.upgrade() else {
            return;
        };
        #[cfg(test)]
        s.succ_gate().await;
        let Some(publisher) = s.succ_next() else {
            drop(s);
            wake.notified().await;
            continue;
        };
        let task = {
            let (s, p) = (s.clone(), publisher.clone());
            tokio::spawn(async move { s.succ_build(&p).await })
        };
        if task.await.is_err() {
            eprintln!(
                "onyx-node: bulk: links index of {publisher}: the build stopped (ADR-0059 amendment 6)"
            );
        }
        if let Ok(mut q) = s.succ.queue.lock() {
            q.busy = None;
        }
        s.succ.done.send_modify(|n| *n = n.wrapping_add(1));
    }
}

impl BulkStore {
    /// Start the links index worker (once) and queue every publisher with a
    /// held catalog, those whose newest catalog removes a dataset first
    /// (their 410s wait for it; the others read nothing: review SUCC-B5 M2),
    /// and among those the smallest links version first (a job's bytes, not
    /// a publisher's name, set its place: review SUCC-B6 L1). Until its job,
    /// a publisher's reads answer `pending`. Needs a tokio runtime; without
    /// one it does nothing.
    pub fn succ_start(self: &Arc<Self>) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let newest: Vec<(String, Option<Arc<CatalogBody>>)> = match self.catalogs.read() {
            Ok(cs) => cs
                .iter()
                .map(|(p, m)| {
                    let body = m.last_key_value().and_then(|(_, h)| h.body.clone());
                    (p.clone(), body)
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        let mut pubs: Vec<(bool, u64, String)> = newest
            .into_iter()
            .map(|(p, body)| match body {
                Some(b) if b.removed.is_empty() => (true, 0, p),
                Some(b) => (false, self.links_bytes(&p, &b.catalog), p),
                None => (false, 0, p),
            })
            .collect();
        pubs.sort();
        for (_, _, p) in &pubs {
            self.succ_queue(p);
        }
        if !self.succ.started.swap(true, Ordering::SeqCst) {
            rt.spawn(succ_work(Arc::downgrade(self), self.succ.wake.clone()));
        }
    }

    /// Queue `publisher`'s index build, unless it is queued already.
    pub(super) fn succ_queue(&self, publisher: &str) {
        if let Ok(mut q) = self.succ.queue.lock() {
            if q.queued.insert(publisher.to_string()) {
                q.order.push_back(publisher.to_string());
            }
        }
        self.succ.wake.notify_one();
    }

    /// The next publisher to build, now busy.
    fn succ_next(&self) -> Option<String> {
        let mut q = self.succ.queue.lock().ok()?;
        let p = q.order.pop_front()?;
        q.queued.remove(&p);
        q.busy = Some(p.clone());
        Some(p)
    }

    /// Shard `root` was stored (or sent again): every publisher whose index
    /// waits for exactly it is queued. Any other shard queues nothing.
    pub(super) fn succ_shard_stored(&self, root: &Hash32) {
        let waiting: Vec<String> = match self.succ.state.lock() {
            Ok(st) => st
                .shards
                .get(root)
                .map(|w| w.iter().cloned().collect())
                .unwrap_or_default(),
            Err(_) => return,
        };
        for p in &waiting {
            self.succ_queue(p);
        }
    }

    /// Manifest `root` of `publisher`'s dataset `id` was stored: the
    /// publisher is queued only when its index waits for exactly that
    /// header (review SUCC-B5 L1).
    pub(super) fn succ_manifest_stored(&self, publisher: &str, id: &str, root: &Hash32) {
        let waits = self.succ.state.lock().is_ok_and(|st| {
            st.by
                .get(publisher)
                .and_then(|p| p.wait.as_ref())
                .is_some_and(
                    |w| matches!(w, Wait::Header { id: i, root: r } if i == id && r == root),
                )
        });
        if waits {
            self.succ_queue(publisher);
        }
    }

    /// The newest held catalog of `publisher`: number, body and signed
    /// report, read together (review SUCC-B2 L1). `None` without one.
    fn newest_catalog(
        &self,
        publisher: &str,
    ) -> Result<Option<(u64, Arc<CatalogBody>, SignedReport)>, ApiError> {
        let cs = self.catalogs.read().map_err(|_| internal("lock"))?;
        let Some((n, h)) = cs.get(publisher).and_then(|m| m.last_key_value()) else {
            return Ok(None);
        };
        let b = h
            .body
            .clone()
            .ok_or_else(|| internal("the newest catalog is not held whole: try again"))?;
        Ok(Some((*n, b, h.report.clone())))
    }

    /// `publisher`'s index when it is built for catalog `at` (pending
    /// included). Never builds, never waits.
    fn succ_index(&self, publisher: &str, at: (u64, Hash32)) -> Option<Index> {
        self.succ
            .state
            .lock()
            .ok()?
            .by
            .get(publisher)?
            .index
            .clone()
            .filter(|ix| ix.catalog == at)
    }

    /// One job: build `publisher`'s index for its newest catalog, unless it
    /// is built for it already (a pending one is built again).
    async fn succ_build(&self, publisher: &str) {
        #[cfg(test)]
        {
            if let Ok(mut j) = self.succ.jobs.lock() {
                j.push(publisher.to_string());
            }
            let boom = self
                .succ
                .panic_next
                .lock()
                .is_ok_and(|mut s| s.remove(publisher));
            assert!(!boom, "test: a links index build panics");
        }
        let newest = match self.newest_catalog(publisher) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("onyx-node: bulk: links index of {publisher}: {}", e.message);
                return;
            }
        };
        let Some((n, body, _)) = newest else {
            if let Ok(mut st) = self.succ.state.lock() {
                st.set_wait(publisher, None);
                st.by.remove(publisher);
            }
            return;
        };
        let at = (n, body.catalog.header.root());
        let wait = {
            let Ok(st) = self.succ.state.lock() else {
                return;
            };
            let p = st.by.get(publisher);
            let built = p
                .and_then(|p| p.index.as_ref())
                .is_some_and(|ix| ix.catalog == at && !matches!(ix.state, LinksState::Pending(_)));
            if built {
                return;
            }
            p.and_then(|p| p.wait.clone())
        };
        let (state, wait) = self.links_state(publisher, &body, wait).await;
        #[cfg(test)]
        self.succ_store_gate().await;
        let mut replaces: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut moved = 0;
        if let LinksState::Ready { links, .. } = &state {
            for (t, r) in links.effective(tombstones(&body)) {
                if let Some(to) = &r.to {
                    replaces
                        .entry(to.clone())
                        .or_default()
                        .push(t.dataset.clone());
                    moved += 1;
                }
            }
        }
        if let Ok(mut st) = self.succ.state.lock() {
            st.by.entry(publisher.to_string()).or_default().index = Some(Index {
                catalog: at,
                state,
                replaces: Arc::new(replaces),
                moved,
            });
            st.set_wait(publisher, wait.clone());
        }
        // Stored while the wait was being set: queue again (the upload's
        // check ran before the wait was there).
        let came = match &wait {
            Some(Wait::Shard { shard, .. }) => self.tiers.tier(shard).is_some(),
            Some(Wait::Header { id, root }) => self.links_header(publisher, id, root).is_some(),
            None => false,
        };
        if came {
            self.succ_queue(publisher);
        }
    }

    /// The held header of version `(id, root)` of `publisher` (`None` when
    /// not held).
    fn links_header(&self, publisher: &str, id: &str, root: &Hash32) -> Option<ManifestHeader> {
        let ds = self.datasets.read().ok()?;
        ds.get(&(publisher.to_string(), id.to_string()))?
            .versions
            .iter()
            .find(|h| h.manifest.header.root() == *root)
            .map(|h| h.manifest.header.clone())
    }

    /// Whether `publisher`'s dataset `id` is forked on this node now (a
    /// poisoned lock counts as forked: it fails closed).
    fn dataset_forked(&self, publisher: &str, id: &str) -> bool {
        self.datasets.read().map_or(true, |ds| {
            ds.get(&(publisher.to_string(), id.to_string()))
                .is_some_and(|d| d.forked.is_some())
        })
    }

    /// The shard entries of held version `(id, root)` when every one from
    /// entry `from` on is held (`Err`: the first that is not). `None` when
    /// the version is not held.
    fn links_shards(
        &self,
        publisher: &str,
        id: &str,
        root: &Hash32,
        from: usize,
    ) -> Option<Result<Vec<ShardEntry>, Wait>> {
        let ds = self.datasets.read().ok()?;
        let shards = &ds
            .get(&(publisher.to_string(), id.to_string()))?
            .versions
            .iter()
            .find(|h| h.manifest.header.root() == *root)?
            .manifest
            .shards;
        let missing = shards
            .iter()
            .enumerate()
            .skip(from)
            .find(|(_, e)| self.tiers.tier(&e.root).is_none());
        Some(match missing {
            Some((next, e)) => Err(Wait::Shard {
                root: *root,
                next,
                shard: e.root,
            }),
            None => Ok(shards.clone()),
        })
    }

    /// The links generation `publisher` uses in catalog `c`
    /// (`choose_generation` over its leaves and the held manifests).
    fn generation_of(&self, publisher: &str, c: &Catalog) -> Result<sx::GenChoice, GenFault<()>> {
        sx::choose_generation(|g| -> Result<GenState, ()> {
            let id = sx::generation_id(g).unwrap_or_default();
            Ok(match listing(c, &id) {
                Listing::Absent => GenState::Absent,
                Listing::Leaf(l) if l.is_tombstone() => GenState::Removed,
                Listing::Leaf(l) => {
                    let header = self.links_header(publisher, &id, &l.root).map(Box::new);
                    GenState::Live { leaf: l, header }
                }
            })
        })
    }

    /// The bytes a job of `publisher` under catalog `c` reads at most: the
    /// held header's `byte_count` of the version it uses (0 when it uses
    /// none or waits for a header: it reads nothing then).
    fn links_bytes(&self, publisher: &str, c: &Catalog) -> u64 {
        self.generation_of(publisher, c)
            .ok()
            .and_then(|ch| ch.used)
            .map_or(0, |u| u.header.byte_count)
    }

    /// Where `publisher`'s links stand under catalog `body`: the used
    /// generation ([`Self::generation_of`]) and the links of its version
    /// that act in it, read whole once every shard is held (nothing is read
    /// for a catalog without a tombstone: nothing can act); and what it
    /// waits for. The scan for missing shards goes on from `wait` when it is
    /// for the same version.
    async fn links_state(
        &self,
        publisher: &str,
        body: &CatalogBody,
        wait: Option<Wait>,
    ) -> (LinksState, Option<Wait>) {
        let c = &body.catalog;
        let used = match self.generation_of(publisher, c) {
            Ok(ch) => match ch.used {
                Some(u) => u,
                None => return (LinksState::None, None),
            },
            Err(GenFault::NoHeader(g)) => {
                let id = sx::generation_id(g).unwrap_or_default();
                let wait = c.find(&id).map(|i| Wait::Header {
                    id: id.clone(),
                    root: c.leaves[i].root,
                });
                return (LinksState::Pending(None), wait);
            }
            // A held manifest is the one its catalog leaf names and valid:
            // the others do not happen on a node.
            Err(e) => {
                eprintln!("onyx-node: bulk: links generation of {publisher}: {e:?}");
                return (LinksState::Pending(None), None);
            }
        };
        let pending = || LinksState::Pending(Some(used.id.clone()));
        let h = &used.header;
        let Ok(mut links) = Links::new(&h.schema) else {
            return (LinksState::None, None);
        };
        let ready = |links: Links| LinksState::Ready {
            generation: used.generation,
            id: used.id.clone(),
            root: used.root,
            links: Arc::new(links),
        };
        if body.removed.is_empty() {
            return (ready(links), None);
        }
        let from = match &wait {
            Some(Wait::Shard { root, next, .. }) if *root == used.root => *next,
            _ => 0,
        };
        let entries = match self.links_shards(publisher, &used.id, &used.root, from) {
            Some(Ok(e)) => e,
            Some(Err(w)) => return (pending(), Some(w)),
            None => return (pending(), None),
        };
        // Each shard file is loaded once however many entries name it
        // (review SUCC-B5 M1; a valid manifest names a root under one schema
        // only), and let go after the last of them. Every entry that names a
        // loaded file again is checked against it: one may claim fewer bytes
        // (H1).
        let last: BTreeMap<Hash32, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.root, i))
            .collect();
        let mut kept: BTreeMap<Hash32, Arc<Loaded>> = BTreeMap::new();
        for (i, e) in entries.iter().enumerate() {
            let l = match kept.remove(&e.root) {
                Some(l) => l,
                None => {
                    #[cfg(test)]
                    self.succ.reads.fetch_add(1, Ordering::SeqCst);
                    match self.load_as(e, h.schema_of(e.index), true).await {
                        Ok(l) => l,
                        Err(_) => return (pending(), None),
                    }
                }
            };
            if !l.fits(e) || !push_link_rows(h, e, &l, &mut links) {
                return (pending(), None);
            }
            if last.get(&e.root) != Some(&i) {
                kept.insert(e.root, l);
            }
        }
        let links = links.only_for(tombstones(body));
        (ready(links), None)
    }

    /// `ix` as it stands now: a links dataset forked since the build is
    /// forked (review SUCC-B2 L2).
    fn succ_state_now<'a>(&self, publisher: &str, ix: &'a Index) -> Now<'a> {
        match &ix.state {
            LinksState::None => Now::Settled(None),
            LinksState::Pending(id) => {
                if id
                    .as_ref()
                    .is_some_and(|id| self.dataset_forked(publisher, id))
                {
                    Now::Forked
                } else {
                    Now::Pending
                }
            }
            LinksState::Ready { id, links, .. } => {
                if self.dataset_forked(publisher, id) {
                    Now::Forked
                } else {
                    Now::Settled(Some(links))
                }
            }
        }
    }

    /// The chain from removed dataset `dataset` under catalog `body`.
    fn chain_of(
        links: Option<&Links>,
        body: &CatalogBody,
        dataset: &str,
    ) -> Result<Chain, ApiError> {
        let c = &body.catalog;
        sx::follow(dataset, links, |id| Ok::<_, ()>(listing(c, id)))
            .map_err(|e: FollowError<()>| internal(format!("links chain: {e:?}")))
    }

    /// Read `publisher`'s fork mark again against the chain in the
    /// background, unless it was read less than [`ANCHOR_RECHECK_SECS`]
    /// ago: a successor answer never waits on the chain (SUCC-B3 L2).
    fn fork_recheck_soon(self: &Arc<Self>, publisher: &str) {
        let now = (self.now)();
        let fresh = self
            .fork_read
            .lock()
            .ok()
            .and_then(|t| t.get(publisher).copied())
            .is_some_and(|t| now.saturating_sub(t) < ANCHOR_RECHECK_SECS);
        if fresh {
            return;
        }
        let me = self.clone();
        let publisher = publisher.to_string();
        tokio::spawn(async move {
            let _ = me.fork_stands(&publisher).await;
        });
    }

    /// The `successor` field of the 410 of `dataset`, removed in catalog `n`
    /// whose body is `body` (the one the removal was read from: SUCC-B3 L1;
    /// ADR-0059 amendment 6 item 5). `forked` under a publisher fork mark,
    /// checked first and read again against the chain in the background
    /// (SUCC-R2 L1, SUCC-B2 L2, SUCC-B3 L2), or when the links dataset is
    /// forked; `pending` while this node lacks what that catalog lists or has
    /// not built its index for it yet (never waited for: SUCC-B2 M2).
    pub(super) fn successor_json(
        self: &Arc<Self>,
        publisher: &str,
        dataset: &str,
        n: u64,
        body: &CatalogBody,
    ) -> Value {
        if self.fork_marked(publisher).is_some() {
            self.fork_recheck_soon(publisher);
            return forked_status();
        }
        let Some(ix) = self.succ_index(publisher, (n, body.catalog.header.root())) else {
            return pending();
        };
        let links = match self.succ_state_now(publisher, &ix) {
            Now::Pending => return pending(),
            Now::Forked => return forked_status(),
            Now::Settled(l) => l,
        };
        let generation = match &ix.state {
            LinksState::Ready { generation, .. } => Some(*generation),
            _ => None,
        };
        let Ok(chain) = Self::chain_of(links, body, dataset) else {
            return pending();
        };
        json!({
            "dataset": chain.hops.first().map(|h| h.to.clone()),
            "final": end_id(&chain.end, dataset),
            "status": chain.end.status(),
            "hops": chain.hops.len(),
            "row": chain.hops.first().map(|h| h.n.to_string()),
            "generation": generation,
            "proof": format!("/v1/bulk/{publisher}/{dataset}/successor"),
        })
    }

    /// The settled index of `publisher` for its newest catalog, unless the
    /// publisher or its links dataset is forked (`None`: not built yet,
    /// pending or forked).
    fn succ_settled_index(&self, publisher: &str) -> Option<Index> {
        if self.fork_marked(publisher).is_some() {
            return None;
        }
        let (n, body, _) = self.newest_catalog(publisher).ok()??;
        let ix = self.succ_index(publisher, (n, body.catalog.header.root()))?;
        matches!(self.succ_state_now(publisher, &ix), Now::Settled(_)).then_some(ix)
    }

    /// `replaces` for live dataset `dataset`: `(count, the first ids)`.
    pub(crate) fn replaces_of(
        &self,
        publisher: &str,
        dataset: &str,
    ) -> Option<(usize, Vec<String>)> {
        let ix = self.succ_settled_index(publisher)?;
        let r = ix.replaces.get(dataset)?;
        Some((
            r.len(),
            r.iter().take(MAX_REPLACES_SHOWN).cloned().collect(),
        ))
    }

    /// For the listing: per publisher with a settled index, target id -> the
    /// ids it replaces, and how many removed datasets have a link (read
    /// before the listing's locks). A publisher without one is left out: its
    /// `moved` is null and its entries show no `replaces` (review SUCC-B2
    /// L4).
    pub(crate) fn replaces_all(&self) -> BTreeMap<String, Replaces> {
        let pubs: Vec<String> = match self.catalogs.read() {
            Ok(cs) => cs.keys().cloned().collect(),
            Err(_) => return BTreeMap::new(),
        };
        pubs.into_iter()
            .filter_map(|p| {
                let ix = self.succ_settled_index(&p)?;
                Some((p, (ix.replaces, ix.moved)))
            })
            .collect()
    }

    /// Whether the last anchor read of `h`, a version of `publisher`'s
    /// dataset `id`, found its report no longer held (`serving` refuses the
    /// version then): page 0 is refused too, with no chain read (review
    /// SUCC-B5 L3). A verdict older than [`ANCHOR_RECHECK_SECS`] is read
    /// again in the background, as the rows route would read it, so a report
    /// held again serves page 0 again.
    fn found_not_held(self: &Arc<Self>, publisher: &str, id: &str, h: &Held) -> bool {
        let last = self
            .rechecked
            .lock()
            .ok()
            .and_then(|c| c.get(&h.report.payload.digest()).copied());
        let Some((t, false)) = last else {
            return false;
        };
        if (self.now)().saturating_sub(t) >= ANCHOR_RECHECK_SECS {
            self.read_again_soon(publisher, id, h, t);
        }
        true
    }

    /// Read the anchor of `h`, a version of `publisher`'s dataset `id`
    /// whose "not held" verdict of time `t` is stale, again in the
    /// background as `serving` reads it, once per window (review SUCC-B6
    /// M1): the first answer claims the read, the answers meanwhile read
    /// nothing, and a read that fails (it writes no verdict) writes "not
    /// held" anew, so it holds another window, as a fork mark does (review
    /// CAT-6 L3). The claim is kept apart from the verdicts: `serving` reads
    /// a stale one.
    fn read_again_soon(self: &Arc<Self>, publisher: &str, id: &str, h: &Held, t: u64) {
        let digest = h.report.payload.digest();
        let claimed = self.succ.rereads.lock().is_ok_and(|mut r| r.insert(digest));
        if !claimed {
            return;
        }
        let claim = ReadAgain(self.clone(), digest);
        let key = (publisher.to_string(), id.to_string());
        let root = h.manifest.header.root();
        tokio::spawn(async move {
            let s = &claim.0;
            let _ = s.serving(&key, Some(root)).await;
            if let Ok(mut c) = s.rechecked.lock() {
                if c.get(&digest) == Some(&(t, false)) {
                    c.insert(digest, ((s.now)(), false));
                }
            }
            drop(claim);
        });
    }

    /// Held version `root` of `publisher`'s dataset `id` as stored, with no
    /// chain read (review SUCC-B4 L1): a reader checks its rows against the
    /// catalog leaf that names `root`.
    fn held_at(&self, publisher: &str, id: &str, root: &Hash32) -> Option<Held> {
        let ds = self.datasets.read().ok()?;
        let h = ds
            .get(&(publisher.to_string(), id.to_string()))?
            .versions
            .iter()
            .find(|h| h.manifest.header.root() == *root)?;
        Some(Held {
            manifest: h.manifest.clone(),
            report: h.report.clone(),
            signer: h.signer,
            anchor: h.anchor.clone(),
            catalog: h.catalog.clone(),
        })
    }

    /// `GET /v1/bulk/{publisher}/{dataset}/successor` (ADR-0059 amendment 6
    /// item 5): for a removed dataset, the newest catalog's header and
    /// report; each links generation up to the used one (its leaf proof and
    /// held header, or its absence proof); every id the chain reaches, start
    /// first (a leaf proof, or an absence proof); the end; and page 0 of the
    /// used links version's rows, pinned by root. A reader checks it all
    /// itself (`onyx-check --successor`). The catalog's number, header,
    /// report and the removal are read together (review SUCC-B2 L1); the
    /// index is used only when built for that catalog, else the end is
    /// `pending`, as when page 0 cannot be read (SUCC-B3 L3). Page 0 is read
    /// from the held version, never through the chain (SUCC-B4 L1). Under a
    /// fork mark, `409 bulk_publisher_forked` at once (the mark is read
    /// again in the background: SUCC-B3 L2).
    pub async fn successor_answer(
        self: &Arc<Self>,
        publisher: &str,
        dataset: &str,
    ) -> Result<Value, ApiError> {
        if let Some(f) = self.fork_marked(publisher) {
            self.fork_recheck_soon(publisher);
            return Err(publisher_forked(&f));
        }
        let newest = self.newest_catalog(publisher)?;
        let Some((n, body, report)) = newest
            .clone()
            .filter(|(_, b, _)| b.removed.contains(dataset))
        else {
            // A storage node's learned removal names no successor (not in
            // this amendment); a dataset the catalog lists live, or held
            // here, is live (review SUCC-B2 L4).
            if let Some(e) = self.learned_gone(publisher, dataset)? {
                return Err(e);
            }
            let listed = newest
                .as_ref()
                .is_some_and(|(_, b, _)| b.catalog.find(dataset).is_some());
            let held = self
                .datasets
                .read()
                .map_err(|_| internal("lock"))?
                .contains_key(&(publisher.to_string(), dataset.to_string()));
            return Err(if listed || held {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "bulk_dataset_live",
                    "the dataset is not removed: it has no successor",
                )
            } else {
                unknown_dataset()
            });
        };
        let c = &body.catalog;
        let ix = self.succ_index(publisher, (n, c.header.root()));
        let mut now = ix
            .as_ref()
            .map_or(Now::Pending, |ix| self.succ_state_now(publisher, ix));
        let ready = match (&now, ix.as_ref().map(|ix| &ix.state)) {
            (
                Now::Settled(Some(_)),
                Some(LinksState::Ready {
                    generation,
                    id,
                    root,
                    ..
                }),
            ) => Some((*generation, id.clone(), *root)),
            _ => None,
        };
        // Page 0 first: without it a reader cannot check the links, so the
        // answer is pending (SUCC-B3 L3).
        let mut links = None;
        if let Some((_, id, root)) = &ready {
            let q = RowsQuery {
                from: None,
                to: None,
                page: 0,
                limit: sx::LINKS_PAGE_ROWS,
                desc: false,
                dated: false,
                root: Some(*root),
            };
            let page = match self.held_at(publisher, id, root) {
                Some(h) if !self.found_not_held(publisher, id, &h) => {
                    self.rows_of(publisher, id, h, &q).await.ok()
                }
                _ => None,
            };
            match page {
                Some(v) => links = Some(v),
                None => now = Now::Pending,
            }
        }
        let proof_of = |id: &str| -> Result<Value, ApiError> {
            Ok(match c.find(id) {
                Some(i) => {
                    let lp = c
                        .leaf_proof_in(&body.tree, i)
                        .ok_or_else(|| internal("leaf proof"))?;
                    json!({"dataset": id, "leaf": hex0x(&borsh::to_vec(&lp).unwrap_or_default())})
                }
                None => {
                    let ap: AbsenceProof = c
                        .absence_proof_in(&body.tree, id)
                        .ok_or_else(|| internal("absence proof"))?;
                    json!({"dataset": id, "absent": hex0x(&borsh::to_vec(&ap).unwrap_or_default())})
                }
            })
        };
        let used = match (&ready, &links) {
            (Some(r), Some(_)) => r.0,
            _ => MAX_LINK_GENERATIONS,
        };
        let mut gens = Vec::new();
        for g in 1..=used {
            let id = sx::generation_id(g).unwrap_or_default();
            let mut v = proof_of(&id)?;
            v["generation"] = json!(g);
            if let Some(i) = c.find(&id).filter(|i| !c.leaves[*i].is_tombstone()) {
                if let Some(h) = self.links_header(publisher, &id, &c.leaves[i].root) {
                    v["header"] = json!(hex0x(&borsh::to_vec(&h).unwrap_or_default()));
                }
            }
            gens.push(v);
        }
        let (end, chain) = match now {
            Now::Pending => (pending(), Vec::new()),
            Now::Forked => (forked_status(), Vec::new()),
            Now::Settled(l) => {
                let ch = Self::chain_of(l, &body, dataset)?;
                let mut ids = vec![dataset.to_string()];
                ids.extend(ch.hops.iter().map(|h| h.to.clone()));
                let proofs = ids
                    .iter()
                    .map(|id| proof_of(id))
                    .collect::<Result<Vec<_>, _>>()?;
                let hops: Vec<Value> = ch
                    .hops
                    .iter()
                    .map(|h| json!({"from": h.from, "row": h.n.to_string(), "position": h.position, "to": h.to}))
                    .collect();
                (
                    json!({"status": ch.end.status(), "dataset": end_id(&ch.end, dataset), "hops": hops}),
                    proofs,
                )
            }
        };
        Ok(json!({
            "publisher": publisher,
            "dataset": dataset,
            "catalog": {
                "number": n,
                "header": hex0x(&borsh::to_vec(&c.header).unwrap_or_default()),
                "report": hex0x(&borsh::to_vec(&report).unwrap_or_default()),
            },
            "generations": gens,
            "chain": chain,
            "end": end,
            "links": links,
        }))
    }
}

/// A claimed background read of a report's anchor
/// ([`BulkStore::read_again_soon`]), let go when dropped, after a panic too.
struct ReadAgain(Arc<BulkStore>, Hash32);

impl Drop for ReadAgain {
    fn drop(&mut self) {
        if let Ok(mut r) = self.0.succ.rereads.lock() {
            r.remove(&self.1);
        }
    }
}

/// Push every row of shard `e` of links version `h`, loaded as `l` (past
/// the loader's cache and its permits for readers, its tier left as it is:
/// review SUCC-B4 L2), shown under the newest schema; `false` when a row
/// does not read.
fn push_link_rows(h: &ManifestHeader, e: &ShardEntry, l: &Loaded, links: &mut Links) -> bool {
    let schema = h.schema_of(e.index);
    for i in 0..l.offsets.len() {
        let Ok(cells) = decode_row(schema, l.row(i)) else {
            return false;
        };
        if links.push(&present(schema, &h.schema, &cells)).is_err() {
            return false;
        }
    }
    true
}

/// The tombstones of catalog `body`.
fn tombstones(body: &CatalogBody) -> impl Iterator<Item = &CatalogLeaf> {
    let c = &body.catalog;
    body.removed
        .iter()
        .filter_map(move |d| c.find(d).map(|i| &c.leaves[i]))
}

/// Tests: the worker and the indexes, seen from outside.
#[cfg(test)]
impl BulkStore {
    /// Whether `publisher` is queued or being built.
    pub(crate) fn succ_busy(&self, publisher: &str) -> bool {
        let q = self.succ.queue.lock().expect("lock");
        q.busy.as_deref() == Some(publisher) || q.queued.contains(publisher)
    }

    /// Wait (up to a cap far above any run) until `publisher` is neither
    /// queued nor being built. Queues nothing.
    pub(crate) async fn succ_settle(&self, publisher: &str) {
        let wait = async {
            loop {
                let mut done = self.succ.done.subscribe();
                if !self.succ_busy(publisher) {
                    return;
                }
                let _ = done.changed().await;
            }
        };
        tokio::time::timeout(Duration::from_secs(60), wait)
            .await
            .expect("the links index build ends");
    }

    /// The publishers queued now, in order.
    pub(crate) fn succ_queued(&self) -> Vec<String> {
        let q = self.succ.queue.lock().expect("lock");
        q.order.iter().cloned().collect()
    }

    /// The publishers built so far, in order.
    pub(crate) fn succ_jobs(&self) -> Vec<String> {
        self.succ.jobs.lock().expect("lock").clone()
    }

    /// Shard loads tried by index builds so far.
    pub(crate) fn succ_reads(&self) -> u64 {
        self.succ.reads.load(Ordering::SeqCst)
    }

    /// Whether `publisher`'s index is built for its newest catalog (pending
    /// included).
    pub(crate) fn succ_built(&self, publisher: &str) -> bool {
        let Some((n, body, _)) = self.newest_catalog(publisher).expect("catalogs") else {
            return false;
        };
        self.succ_index(publisher, (n, body.catalog.header.root()))
            .is_some()
    }

    /// The links `publisher`'s index keeps (one per tombstone at most).
    pub(crate) fn succ_kept(&self, publisher: &str) -> usize {
        let st = self.succ.state.lock().expect("lock");
        match st.by.get(publisher).and_then(|p| p.index.as_ref()) {
            Some(Index {
                state: LinksState::Ready { links, .. },
                ..
            }) => links.kept(),
            _ => 0,
        }
    }

    /// From now on the worker waits for a permit of the semaphore returned
    /// (none at first) before it takes each job, so the queue stays as it
    /// is; [`Self::succ_release`] ends that.
    pub(crate) fn succ_hold(&self) -> Arc<tokio::sync::Semaphore> {
        let g = Arc::new(tokio::sync::Semaphore::new(0));
        *self.succ.gate.lock().expect("lock") = Some(g.clone());
        g
    }

    /// The worker no longer waits before a job.
    pub(crate) fn succ_release(&self) {
        if let Some(g) = self.succ.gate.lock().expect("lock").take() {
            g.close();
        }
    }

    async fn succ_gate(&self) {
        let g = self.succ.gate.lock().expect("lock").clone();
        if let Some(g) = g {
            if let Ok(p) = g.acquire().await {
                p.forget();
            }
        }
    }

    /// From now on a job waits, after it read and before it stores what it
    /// found, for a permit of the semaphore returned (none at first).
    pub(crate) fn succ_hold_store(&self) -> Arc<tokio::sync::Semaphore> {
        let g = Arc::new(tokio::sync::Semaphore::new(0));
        *self.succ.store_gate.lock().expect("lock") = Some(g.clone());
        g
    }

    /// Whether a job waits at that gate now.
    pub(crate) fn succ_at_store_gate(&self) -> bool {
        self.succ.at_store_gate.load(Ordering::SeqCst)
    }

    /// Jobs no longer wait before they store.
    pub(crate) fn succ_release_store(&self) {
        if let Some(g) = self.succ.store_gate.lock().expect("lock").take() {
            g.close();
        }
    }

    async fn succ_store_gate(&self) {
        let g = self.succ.store_gate.lock().expect("lock").clone();
        if let Some(g) = g {
            self.succ.at_store_gate.store(true, Ordering::SeqCst);
            if let Ok(p) = g.acquire().await {
                p.forget();
            }
            self.succ.at_store_gate.store(false, Ordering::SeqCst);
        }
    }

    /// `publisher`'s next job panics.
    pub(crate) fn succ_panic_next(&self, publisher: &str) {
        self.succ
            .panic_next
            .lock()
            .expect("lock")
            .insert(publisher.to_string());
    }
}

/// Per publisher, for the listing: target id -> the ids it replaces, and
/// how many removed datasets have a link.
pub(crate) type Replaces = (Arc<BTreeMap<String, Vec<String>>>, usize);
