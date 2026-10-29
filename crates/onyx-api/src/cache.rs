//! Bounded in-memory caches (M7-F3, F4, F5): finalized roots for a short TTL,
//! row batches and their Merkle trees by root (content-addressed, so never
//! stale), settled payment responses for idempotent retries (per client, a
//! few replays each, M7-L39-F5), settlements claimed and in flight (claimed
//! atomically, released by a drop guard, M7-L39-F1, M7-L40-F4/F5), token
//! buckets for payment attempts per client and per wider IPv6 prefix
//! (M7-L39-F4, M7-L40-F2), and a per-payer budget of payments sent to the
//! facilitator that never settled (M7-L40-F2/F3). The budget unit is taken
//! when the payment is checked and refunded when it settles (M7-L41-F5), so
//! concurrent requests cannot overspend it. Settled responses are bounded by
//! count and by bytes (M7-L41-F3).

use crate::roots::FinalRoot;
use onyx_core::Hash32;
use onyx_merkle::MerkleTree;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// A bounded map with least-recently-used eviction, an optional expiry per
/// entry, and a size budget (each entry has a weight; the sum stays under it).
struct Lru<K: Ord + Clone, V: Clone> {
    map: BTreeMap<K, (V, u64, Option<Instant>, usize)>,
    order: BTreeMap<u64, K>,
    tick: u64,
    weight: usize,
    max_entries: usize,
    max_weight: usize,
}

impl<K: Ord + Clone, V: Clone> Lru<K, V> {
    fn new(max_entries: usize, max_weight: usize) -> Self {
        Lru {
            map: BTreeMap::new(),
            order: BTreeMap::new(),
            tick: 0,
            weight: 0,
            max_entries,
            max_weight,
        }
    }

    fn remove(&mut self, k: &K) {
        if let Some((_, t, _, w)) = self.map.remove(k) {
            self.order.remove(&t);
            self.weight -= w;
        }
    }

    fn get(&mut self, k: &K, now: Instant) -> Option<V> {
        let (v, t, exp, w) = self.map.get(k)?.clone();
        if exp.is_some_and(|e| now >= e) {
            self.remove(k);
            return None;
        }
        self.order.remove(&t);
        self.tick += 1;
        self.order.insert(self.tick, k.clone());
        self.map.insert(k.clone(), (v.clone(), self.tick, exp, w));
        Some(v)
    }

    fn insert(&mut self, k: K, v: V, ttl: Option<Duration>, weight: usize, now: Instant) {
        self.remove(&k);
        self.prune_expired(now);
        if weight > self.max_weight || self.max_entries == 0 {
            return;
        }
        while self.map.len() >= self.max_entries || self.weight + weight > self.max_weight {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some((_, _, _, w)) = self.map.remove(&oldest) {
                self.weight -= w;
            }
        }
        self.tick += 1;
        self.order.insert(self.tick, k.clone());
        self.weight += weight;
        self.map
            .insert(k, (v, self.tick, ttl.map(|t| now + t), weight));
    }

    /// Drop expired entries from the least recently used end (SOAK-3): an
    /// entry no one asks for again used to stay until the count or weight
    /// budget pushed it out, so a steady paid load filled the settled
    /// responses toward their 65,536-entry / 256 MiB budget long after their
    /// 10-minute life (the L6 soak: onyx-node grew about 30 MB an hour). With
    /// one TTL per cache, the least recently used entries expire first; an
    /// expired entry behind a live one goes when it reaches the front (or on
    /// its next `get`). Amortized O(1) per insert.
    fn prune_expired(&mut self, now: Instant) {
        while let Some((_, oldest)) = self.order.first_key_value() {
            let expired = self
                .map
                .get(oldest)
                .is_none_or(|e| e.2.is_some_and(|x| now >= x));
            if !expired {
                break;
            }
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some((_, _, _, w)) = self.map.remove(&oldest) {
                self.weight -= w;
            }
        }
    }

    /// Change a live entry in place (expiry and recency unchanged). `f`
    /// returns false to drop the entry instead.
    fn update(&mut self, k: &K, now: Instant, f: impl FnOnce(&mut V) -> bool) -> Option<V> {
        if self.map.get(k)?.2.is_some_and(|e| now >= e) {
            self.remove(k);
            return None;
        }
        let entry = self.map.get_mut(k)?;
        let keep = f(&mut entry.0);
        let out = entry.0.clone();
        if !keep {
            self.remove(k);
        }
        Some(out)
    }

    fn len(&self) -> usize {
        self.map.len()
    }
}

/// Rows of a finalized root with their tree and a key index.
pub struct Loaded {
    /// Row bytes, in order.
    pub rows: Vec<Vec<u8>>,
    /// Their Merkle tree (its root equals the finalized root).
    pub tree: MerkleTree,
    /// Row key (`OutputRow.key`) -> index, built once per root (M7-L39-F3).
    pub by_key: BTreeMap<Vec<u8>, usize>,
}

impl Loaded {
    /// Rows and tree, with the key index built from the rows.
    pub fn new(rows: Vec<Vec<u8>>, tree: MerkleTree) -> Self {
        use borsh::BorshDeserialize;
        let mut by_key = BTreeMap::new();
        for (i, r) in rows.iter().enumerate() {
            if let Ok(o) = onyx_recipe_sdk::OutputRow::try_from_slice(r) {
                by_key.entry(o.key).or_insert(i);
            }
        }
        Loaded { rows, tree, by_key }
    }
}

/// A response kept for an identical paid retry.
#[derive(Clone, Debug)]
pub struct CachedResponse {
    /// JSON body.
    pub body: serde_json::Value,
    /// ETag.
    pub etag: String,
    /// Base64 `PAYMENT-RESPONSE` header.
    pub payment_response: String,
    /// The response was immutable (root-pinned URL).
    pub immutable: bool,
}

/// A settled response with the replays it has left.
#[derive(Clone, Debug)]
struct Settled {
    response: CachedResponse,
    replays_left: u32,
}

/// Token bucket in integer units (no floats, ADR-0009): one attempt costs
/// the bucket's period in milliseconds; a bucket earns `rate` units per
/// millisecond, so `rate` attempts per period, with a burst of `rate`.
#[derive(Clone)]
struct Bucket {
    units: u64,
    at: Instant,
}

/// Units of one attempt per minute (milliseconds in a minute).
const ATTEMPT: u64 = 60_000;
/// Units of one unsettled payment per hour (milliseconds in an hour).
const UNSETTLED: u64 = 3_600_000;

impl Bucket {
    /// The bucket refilled to `now`, for `rate` attempts per `period` ms.
    fn refilled(b: Option<Bucket>, rate: u64, period: u64, now: Instant) -> Bucket {
        let cap = rate.saturating_mul(period);
        let mut e = b.unwrap_or(Bucket {
            units: cap,
            at: now,
        });
        let ms = u64::try_from(now.duration_since(e.at).as_millis()).unwrap_or(u64::MAX);
        e.units = e.units.saturating_add(ms.saturating_mul(rate)).min(cap);
        e.at = now;
        e
    }
}

/// How a payment key was claimed (M7-L40-F4).
pub enum Claimed {
    /// This request owns the payment until its [`ClaimGuard`] drops.
    Mine(ClaimGuard),
    /// Another request owns it: wait for it to end, then look for its response.
    Wait(watch::Receiver<bool>),
    /// Too many payments in flight.
    Full,
}

/// Ownership of one payment key, from `authorize` to the end of settlement.
/// Dropping it, on any path (an error before settling, a refused settle, a
/// panic in the settle task), releases the key and wakes waiting retries
/// (M7-L40-F5). The payer's budget unit, taken by [`Caches::reserve_payer`]
/// before the facilitator is called, stays spent unless the payment settles
/// or the failure was the facilitator's (M7-L41-F5).
pub struct ClaimGuard {
    caches: Arc<Caches>,
    key: Hash32,
    tx: Option<watch::Sender<bool>>,
    payer: Option<[u8; 20]>,
    unsettled_per_hour: u32,
}

impl ClaimGuard {
    /// `payer`'s budget unit was reserved for this payment.
    pub fn reserved(&mut self, payer: [u8; 20], unsettled_per_hour: u32) {
        self.payer = Some(payer);
        self.unsettled_per_hour = unsettled_per_hour;
    }

    /// Give the reserved unit back: the payment settled, or it failed for a
    /// reason that is not the payer's (facilitator down or slow).
    pub fn refund(&mut self) {
        if let Some(p) = self.payer.take() {
            self.caches.refund_payer(&p, self.unsettled_per_hour);
        }
    }

    /// The payment settled: its unit is refunded.
    pub fn settled(&mut self) {
        self.refund();
    }

    /// The key it holds.
    pub fn key(&self) -> Hash32 {
        self.key
    }
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        // A poisoned lock must not leave the key claimed forever.
        self.caches
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(true);
        }
    }
}

/// All caches of one server. Created empty with [`Caches::default`].
pub struct Caches {
    roots: Mutex<Lru<(u64, Hash32), FinalRoot>>,
    trees: Mutex<Lru<Hash32, Arc<Loaded>>>,
    settled: Mutex<Lru<Hash32, Settled>>,
    pending: Mutex<BTreeMap<Hash32, watch::Receiver<bool>>>,
    buckets: Mutex<Lru<String, Bucket>>,
    unsettled: Mutex<Lru<[u8; 20], Bucket>>,
    contributors: Mutex<Lru<[u8; 20], ContribEntry>>,
    /// Root lookups that found no final root or failed, kept briefly so a
    /// flood of requests for such shards does not reach the node each time
    /// (AUD-1-F4).
    root_misses: Mutex<Lru<(u64, Hash32), RootMiss>>,
    /// One node lookup per shard at a time (AUD-1-F4): concurrent requests
    /// for the same shard wait for the first lookup and read its result.
    root_flights: Mutex<BTreeMap<(u64, Hash32), FlightLock>>,
}

/// The lock one shard's node lookup holds (AUD-1-F4).
type FlightLock = Arc<tokio::sync::Mutex<()>>;

/// A cached root lookup that found no usable root (AUD-1-F4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootMiss {
    /// The shard has no final root yet.
    NotFinal,
    /// The root source failed; the message is kept for the 503.
    Unavailable(String),
}

/// One proven contributor's read budget and cached status (REV-L1-F1): kept in
/// its own LRU, entered only after the address proved accepted work, so
/// requests signed by throwaway keys never evict a spent budget, and never
/// touch the payment limiter's buckets.
#[derive(Clone)]
struct ContribEntry {
    minute: Bucket,
    day_start: Instant,
    day_rows: u64,
    checked: Instant,
}

/// Proven contributors tracked for read budgets.
pub const MAX_CONTRIBUTORS: usize = 65_536;
/// How long a confirmed contributor status is reused without asking the
/// source again (REV-L1-F1: no RPC per read).
pub const CONTRIBUTOR_STATUS_TTL: Duration = Duration::from_secs(60);
const DAY: Duration = Duration::from_secs(86_400);

/// Default number of cached roots.
pub const ROOT_ENTRIES: usize = 4_096;
/// Default number of cached row batches.
pub const TREE_ENTRIES: usize = 256;
/// Default byte budget for cached row batches.
pub const TREE_BYTES: usize = 256 << 20;
/// Settled responses kept for retries, by default ([`Caches::new`] takes the
/// configured number; M7-L40-F5: large enough that other clients' paid traffic
/// does not evict a response owed to a client that got a 504).
pub const SETTLED_ENTRIES: usize = 65_536;
/// Bytes of settled responses kept for retries, by default (M7-L41-F3: an
/// export body can be tens of MB).
pub const SETTLED_BYTES: usize = 256 << 20;
/// How long a settled response is replayed.
pub const SETTLED_TTL: Duration = Duration::from_secs(600);
/// Replays of one settled response (a lost answer, a retry); a payment header
/// is not a bearer token for unlimited fetches (M7-L39-F5).
pub const SETTLED_REPLAYS: u32 = 3;
/// Most clients tracked by the rate limiter; the least recently seen are
/// dropped in O(log n) (M7-L39-F4).
pub const MAX_BUCKETS: usize = 65_536;
/// Settlements in flight tracked for waiting retries.
const MAX_PENDING: usize = 4_096;
/// Payers tracked by the unsettled budget.
const MAX_PAYERS: usize = 65_536;

impl Default for Caches {
    fn default() -> Self {
        Self::new(SETTLED_ENTRIES, SETTLED_BYTES)
    }
}

/// Approximate bytes of a stored response (body as JSON text plus headers).
fn response_bytes(r: &CachedResponse) -> usize {
    serde_json::to_vec(&r.body)
        .map(|b| b.len())
        .unwrap_or(usize::MAX)
        .saturating_add(r.etag.len() + r.payment_response.len() + 64)
}

impl Caches {
    /// Empty caches keeping up to `settled_entries` settled responses and
    /// `settled_bytes` bytes of them.
    pub fn new(settled_entries: usize, settled_bytes: usize) -> Self {
        Caches {
            roots: Mutex::new(Lru::new(ROOT_ENTRIES, usize::MAX)),
            trees: Mutex::new(Lru::new(TREE_ENTRIES, TREE_BYTES)),
            settled: Mutex::new(Lru::new(settled_entries, settled_bytes)),
            pending: Mutex::new(BTreeMap::new()),
            buckets: Mutex::new(Lru::new(MAX_BUCKETS, usize::MAX)),
            unsettled: Mutex::new(Lru::new(MAX_PAYERS, usize::MAX)),
            contributors: Mutex::new(Lru::new(MAX_CONTRIBUTORS, usize::MAX)),
            root_misses: Mutex::new(Lru::new(ROOT_ENTRIES, usize::MAX)),
            root_flights: Mutex::new(BTreeMap::new()),
        }
    }

    /// A cached failed root lookup (AUD-1-F4).
    pub fn root_miss(&self, round: u64, shard: &Hash32) -> Option<RootMiss> {
        self.root_misses
            .lock()
            .ok()?
            .get(&(round, *shard), Instant::now())
    }

    /// Cache a failed root lookup for `ttl` (AUD-1-F4).
    pub fn put_root_miss(&self, round: u64, shard: Hash32, miss: RootMiss, ttl: Duration) {
        if let Ok(mut c) = self.root_misses.lock() {
            c.insert((round, shard), miss, Some(ttl), 1, Instant::now());
        }
    }

    /// The lock that serializes node lookups of one shard (AUD-1-F4).
    pub fn root_flight(&self, round: u64, shard: &Hash32) -> FlightLock {
        let mut f = self
            .root_flights
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f.entry((round, *shard)).or_default().clone()
    }

    /// Drop the lookup lock of a shard once no request holds it.
    pub fn end_root_flight(&self, round: u64, shard: &Hash32) {
        let mut f = self
            .root_flights
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if f.get(&(round, *shard))
            .is_some_and(|l| Arc::strong_count(l) == 1)
        {
            f.remove(&(round, *shard));
        }
    }

    /// Lookup locks currently held or waited on (tests).
    pub fn root_flights(&self) -> usize {
        self.root_flights.lock().map(|f| f.len()).unwrap_or(0)
    }

    /// A cached finalized root.
    pub fn root(&self, round: u64, shard: &Hash32) -> Option<FinalRoot> {
        self.roots
            .lock()
            .ok()?
            .get(&(round, *shard), Instant::now())
    }

    /// Cache a finalized root for `ttl` (not-finalized answers are never cached).
    pub fn put_root(&self, round: u64, shard: Hash32, root: FinalRoot, ttl: Duration) {
        if let Ok(mut c) = self.roots.lock() {
            c.insert((round, shard), root, Some(ttl), 1, Instant::now());
        }
    }

    /// Rows and tree of `root`, if cached.
    pub fn tree(&self, root: &Hash32) -> Option<Arc<Loaded>> {
        self.trees.lock().ok()?.get(root, Instant::now())
    }

    /// Cache rows and tree of `root` (by content: never stale).
    pub fn put_tree(&self, root: Hash32, loaded: Arc<Loaded>) {
        let bytes = loaded.rows.iter().map(|r| r.len() + 64).sum();
        if let Ok(mut c) = self.trees.lock() {
            c.insert(root, loaded, None, bytes, Instant::now());
        }
    }

    /// Number of cached row batches (tests).
    pub fn trees_cached(&self) -> usize {
        self.trees.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Replay the settled response for payment `key` (one replay used), or
    /// `None` when there is none or its replays are spent.
    pub fn replay_settled(&self, key: &Hash32) -> Option<CachedResponse> {
        let mut c = self.settled.lock().ok()?;
        let mut granted = false;
        c.update(key, Instant::now(), |s| {
            granted = s.replays_left > 0;
            s.replays_left = s.replays_left.saturating_sub(1);
            granted && s.replays_left > 0
        })
        .filter(|_| granted)
        .map(|s| s.response)
    }

    /// Remember a settled response for identical retries from the same client.
    /// Weighted by its size, so a few huge exports cannot exhaust memory
    /// (M7-L41-F3); one heavier than the whole budget is not kept.
    pub fn put_settled(&self, key: Hash32, r: CachedResponse) {
        let weight = response_bytes(&r);
        if let Ok(mut c) = self.settled.lock() {
            c.insert(
                key,
                Settled {
                    response: r,
                    replays_left: SETTLED_REPLAYS,
                },
                Some(SETTLED_TTL),
                weight,
                Instant::now(),
            );
        }
    }

    /// Number of settled responses kept (tests).
    pub fn settled_kept(&self) -> usize {
        self.settled.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Claim payment `key` atomically (M7-L40-F4): exactly one request owns
    /// a payment at a time; identical concurrent requests wait for it.
    pub fn claim(self: &Arc<Self>, key: Hash32) -> Claimed {
        let Ok(mut p) = self.pending.lock() else {
            return Claimed::Full;
        };
        if let Some(rx) = p.get(&key) {
            return Claimed::Wait(rx.clone());
        }
        if p.len() >= MAX_PENDING {
            return Claimed::Full;
        }
        let (tx, rx) = watch::channel(false);
        p.insert(key, rx);
        Claimed::Mine(ClaimGuard {
            caches: self.clone(),
            key,
            tx: Some(tx),
            payer: None,
            unsettled_per_hour: 0,
        })
    }

    /// Payment keys claimed now (tests of the release order, M7L3-R2-2).
    #[cfg(test)]
    pub(crate) fn claimed(&self) -> usize {
        self.pending.lock().map(|p| p.len()).unwrap_or(0)
    }

    /// A payment of `key` still claimed, to wait on.
    pub fn settlement_in_flight(&self, key: &Hash32) -> Option<watch::Receiver<bool>> {
        self.pending.lock().ok()?.get(key).cloned()
    }

    /// Take one attempt from every scope's bucket, or from none (M7-L40-F2):
    /// `scopes` are (key, attempts per minute), e.g. an IPv6 /64, its /56 and
    /// its /48, so rotating /64s inside one allocation shares the wider
    /// buckets. False when any is empty: the caller answers 429.
    pub fn take_attempts(&self, scopes: &[(String, u32)]) -> bool {
        let Ok(mut b) = self.buckets.lock() else {
            return false;
        };
        let now = Instant::now();
        let mut next = Vec::with_capacity(scopes.len());
        for (key, per_minute) in scopes {
            let e = Bucket::refilled(
                b.get(key, now),
                u64::from((*per_minute).max(1)),
                ATTEMPT,
                now,
            );
            next.push((key, e));
        }
        let ok = next.iter().all(|(_, e)| e.units >= ATTEMPT);
        for (key, mut e) in next {
            if ok {
                e.units -= ATTEMPT;
            }
            b.insert(key.clone(), e, None, 1, now);
        }
        ok
    }

    /// Whether `payer` has budget left (tests and diagnostics; the payment
    /// path uses [`Caches::reserve_payer`]).
    pub fn payer_allowed(&self, payer: &[u8; 20], per_hour: u32) -> bool {
        let Ok(mut b) = self.unsettled.lock() else {
            return false;
        };
        let now = Instant::now();
        let e = Bucket::refilled(
            b.get(payer, now),
            u64::from(per_hour.max(1)),
            UNSETTLED,
            now,
        );
        e.units >= UNSETTLED
    }

    /// Take one unit of `payer`'s budget of payments that end without
    /// settling (a 404 after verify, a refused settle, an abandoned request),
    /// under one lock, before the facilitator is called (M7-L41-F5): N
    /// concurrent requests get at most the budget. False when it is empty.
    pub fn reserve_payer(&self, payer: &[u8; 20], per_hour: u32) -> bool {
        let Ok(mut b) = self.unsettled.lock() else {
            return false;
        };
        let now = Instant::now();
        let mut e = Bucket::refilled(
            b.get(payer, now),
            u64::from(per_hour.max(1)),
            UNSETTLED,
            now,
        );
        let ok = e.units >= UNSETTLED;
        if ok {
            e.units -= UNSETTLED;
        }
        b.insert(*payer, e, None, 1, now);
        ok
    }

    /// Give back one unit of `payer`'s budget (the payment settled, was
    /// refused at verify, or the facilitator failed), never above the cap.
    /// A payer back at its full budget is dropped from the store: a missing
    /// entry means a full budget, so nothing is lost, and keys that never
    /// end unsettled (fresh unfunded keys, refused at verify) do not hold
    /// slots that would evict drained payers and reset them (M7-L42-F3).
    pub fn refund_payer(&self, payer: &[u8; 20], per_hour: u32) {
        if let Ok(mut b) = self.unsettled.lock() {
            let now = Instant::now();
            let rate = u64::from(per_hour.max(1));
            let cap = rate.saturating_mul(UNSETTLED);
            let mut e = Bucket::refilled(b.get(payer, now), rate, UNSETTLED, now);
            e.units = e.units.saturating_add(UNSETTLED).min(cap);
            if e.units >= cap {
                b.remove(payer);
            } else {
                b.insert(*payer, e, None, 1, now);
            }
        }
    }

    /// Number of payers the unsettled budget tracks (tests).
    pub fn payers_tracked(&self) -> usize {
        self.unsettled.lock().map(|b| b.len()).unwrap_or(0)
    }

    /// Take one payment attempt from `client`'s own bucket (`per_minute`
    /// refill, same burst). Bounded: the least recently seen clients are
    /// dropped, in O(log n).
    pub fn take_attempt(&self, client: &str, per_minute: u32) -> bool {
        self.take_attempts(&[(client.to_string(), per_minute)])
    }

    /// Number of clients the rate limiter tracks (tests).
    pub fn buckets_tracked(&self) -> usize {
        self.buckets.lock().map(|b| b.len()).unwrap_or(0)
    }

    /// Whether `who` was confirmed a contributor less than
    /// [`CONTRIBUTOR_STATUS_TTL`] ago.
    pub fn contributor_confirmed(&self, who: &[u8; 20]) -> bool {
        let now = Instant::now();
        self.contributors
            .lock()
            .ok()
            .and_then(|mut m| m.get(who, now))
            .is_some_and(|e| now.duration_since(e.checked) < CONTRIBUTOR_STATUS_TTL)
    }

    /// Take one read of `rows` rows from a proven contributor's budget
    /// (REV-L1-F1, F5): `per_minute` reads per minute (same burst) and
    /// `rows_per_day` rows per day (a fixed 24 h window from the first
    /// counted read, REV-L2-F4). `confirmed` records that the source just
    /// confirmed the address. False (429) when either is spent.
    pub fn take_contributor_read(
        &self,
        who: [u8; 20],
        rows: u64,
        per_minute: u32,
        rows_per_day: u64,
        confirmed: bool,
    ) -> bool {
        let Ok(mut m) = self.contributors.lock() else {
            return false;
        };
        let now = Instant::now();
        let mut e = m.get(&who, now).unwrap_or(ContribEntry {
            minute: Bucket::refilled(None, u64::from(per_minute.max(1)), ATTEMPT, now),
            day_start: now,
            day_rows: 0,
            checked: now,
        });
        if confirmed {
            e.checked = now;
        }
        e.minute = Bucket::refilled(Some(e.minute), u64::from(per_minute.max(1)), ATTEMPT, now);
        if now.duration_since(e.day_start) >= DAY {
            e.day_start = now;
            e.day_rows = 0;
        }
        let ok = e.minute.units >= ATTEMPT && e.day_rows.saturating_add(rows) <= rows_per_day;
        if ok {
            e.minute.units -= ATTEMPT;
            e.day_rows += rows;
        }
        m.insert(who, e, None, 1, now);
        ok
    }

    /// Number of contributors with a tracked budget (tests).
    pub fn contributors_tracked(&self) -> usize {
        self.contributors.lock().map(|m| m.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_evicts_oldest_by_count_and_weight_and_expires() {
        let now = Instant::now();
        let mut c: Lru<u32, u32> = Lru::new(2, 10);
        c.insert(1, 1, None, 4, now);
        c.insert(2, 2, None, 4, now);
        assert_eq!(c.get(&1, now), Some(1)); // 1 is now the newest
        c.insert(3, 3, None, 4, now); // evicts 2 (count)
        assert_eq!(c.get(&2, now), None);
        assert_eq!(c.get(&1, now), Some(1));
        c.insert(4, 4, None, 9, now); // weight: evicts everything else
        assert_eq!((c.get(&1, now), c.get(&3, now)), (None, None));
        c.insert(5, 5, Some(Duration::from_secs(1)), 1, now);
        assert_eq!(c.get(&5, now + Duration::from_secs(2)), None);
        c.insert(6, 6, None, 11, now); // heavier than the budget: not kept
        assert_eq!(c.get(&6, now), None);
    }

    /// SOAK-3 (L6 soak): expired entries are dropped when new ones come, not
    /// kept until the count or weight budget pushes them out. Before, 1,000
    /// responses past their life were all still held (and their memory).
    #[test]
    fn soak_3_expired_entries_are_dropped_on_insert() {
        let now = Instant::now();
        let ttl = Some(Duration::from_secs(600));
        let mut c: Lru<u32, u32> = Lru::new(65_536, usize::MAX);
        for i in 0..1_000 {
            c.insert(i, i, ttl, 10, now);
        }
        assert_eq!(c.len(), 1_000);
        // One read keeps an entry's recency fresh, not its life.
        assert_eq!(c.get(&5, now + Duration::from_secs(300)), Some(5));
        let later = now + Duration::from_secs(601);
        c.insert(1_000, 1_000, ttl, 10, later);
        // The read entry expired too (its life runs from its insert).
        assert_eq!(c.len(), 1, "only the new entry is left");
        assert_eq!(c.weight, 10);
        assert_eq!(c.get(&5, later), None);
        // A live entry in front stops the sweep; the expired one behind it
        // goes once it reaches the front.
        c.insert(2_000, 1, ttl, 10, later + Duration::from_secs(300));
        assert_eq!(c.get(&1_000, later + Duration::from_secs(300)), Some(1_000));
        let t = later + Duration::from_secs(650);
        c.insert(2_001, 1, ttl, 10, t);
        assert_eq!(
            c.len(),
            3,
            "1,000 expired behind the live 2,000: kept for now"
        );
        assert_eq!(c.get(&1_000, t), None, "and dropped on its read");
        assert_eq!(c.len(), 2);
        // Entries without a TTL are never pruned by time.
        let mut d: Lru<u32, u32> = Lru::new(10, usize::MAX);
        d.insert(1, 1, None, 1, now);
        d.insert(2, 2, None, 1, now + Duration::from_secs(10_000));
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn settled_responses_replay_a_few_times_then_stop() {
        let c = Caches::default();
        let k = Hash32([7; 32]);
        assert!(c.replay_settled(&k).is_none());
        c.put_settled(
            k,
            CachedResponse {
                body: serde_json::json!({"a": 1}),
                etag: "e".into(),
                payment_response: "p".into(),
                immutable: false,
            },
        );
        for _ in 0..SETTLED_REPLAYS {
            assert!(c.replay_settled(&k).is_some());
        }
        assert!(c.replay_settled(&k).is_none());
    }

    /// M7-L39-F4: the limiter tracks at most MAX_BUCKETS clients, dropping
    /// the least recently seen; a dropped client starts with a full bucket.
    #[test]
    fn rate_limiter_is_bounded_and_drops_the_oldest_client() {
        let c = Caches::default();
        assert!(c.take_attempt("first", 1));
        assert!(!c.take_attempt("first", 1));
        for i in 0..MAX_BUCKETS {
            c.take_attempt(&format!("c{i}"), 1);
        }
        assert_eq!(c.buckets_tracked(), MAX_BUCKETS);
        // "first" was the oldest: dropped, so it is new again.
        assert!(c.take_attempt("first", 1));
        assert_eq!(c.buckets_tracked(), MAX_BUCKETS);
    }

    #[test]
    fn bucket_allows_a_burst_then_refuses() {
        let c = Caches::default();
        assert!((0..3).all(|_| c.take_attempt("1.2.3.4", 3)));
        assert!(!c.take_attempt("1.2.3.4", 3));
        assert!(c.take_attempt("5.6.7.8", 3));
    }

    /// M7-L40-F4: one owner per payment key; a second claim waits.
    #[test]
    fn claims_are_exclusive_until_released() {
        let c = Arc::new(Caches::new(8, SETTLED_BYTES));
        let k = Hash32([7; 32]);
        let Claimed::Mine(g) = c.claim(k) else {
            panic!("first claim owns the key");
        };
        let Claimed::Wait(mut rx) = c.claim(k) else {
            panic!("second claim waits");
        };
        assert!(!*rx.borrow_and_update());
        drop(g);
        assert!(*rx.borrow());
        assert!(c.settlement_in_flight(&k).is_none());
        assert!(matches!(c.claim(k), Claimed::Mine(_)));
    }

    /// M7-L40-F5: a settle task that panics still releases the key and
    /// wakes the waiters; the unsettled payment is charged to its payer.
    #[tokio::test]
    async fn a_panicking_settle_task_releases_its_claim() {
        let c = Arc::new(Caches::new(8, SETTLED_BYTES));
        let k = Hash32([9; 32]);
        let payer = [0xab; 20];
        let Claimed::Mine(mut g) = c.claim(k) else {
            panic!("claim");
        };
        assert!(c.reserve_payer(&payer, 1));
        g.reserved(payer, 1);
        let Claimed::Wait(mut rx) = c.claim(k) else {
            panic!("wait");
        };
        let task = tokio::spawn(async move {
            let _owned = g;
            panic!("settle task panics");
        });
        assert!(task.await.expect_err("panicked").is_panic());
        rx.changed().await.expect("woken");
        assert!(*rx.borrow());
        assert!(c.settlement_in_flight(&k).is_none());
        assert!(
            !c.payer_allowed(&payer, 1),
            "the unsettled payment is charged"
        );
    }

    /// A settled claim does not charge the payer.
    #[test]
    fn a_settled_claim_is_not_charged() {
        let c = Arc::new(Caches::new(8, SETTLED_BYTES));
        let payer = [0xcd; 20];
        let Claimed::Mine(mut g) = c.claim(Hash32([1; 32])) else {
            panic!("claim");
        };
        assert!(c.reserve_payer(&payer, 1));
        g.reserved(payer, 1);
        assert!(!c.payer_allowed(&payer, 1), "reserved while in flight");
        g.settled();
        drop(g);
        assert!(c.payer_allowed(&payer, 1));
    }

    /// M7-L41-F5: concurrent payments from one payer cannot take more than
    /// its budget, however they interleave.
    #[test]
    fn concurrent_reservations_never_exceed_the_budget() {
        let c = Arc::new(Caches::default());
        let payer = [0xee; 20];
        let budget = 5u32;
        let handles: Vec<_> = (0..64)
            .map(|_| {
                let c = c.clone();
                std::thread::spawn(move || c.reserve_payer(&payer, budget))
            })
            .collect();
        let granted = handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .filter(|ok| *ok)
            .count();
        assert_eq!(granted, budget as usize);
        // A settled payment gives its unit back.
        c.refund_payer(&payer, budget);
        assert!(c.reserve_payer(&payer, budget));
        assert!(!c.reserve_payer(&payer, budget));
    }

    /// M7-L41-F3: settled responses are bounded by bytes, not only by count.
    #[test]
    fn settled_responses_are_bounded_by_bytes() {
        let c = Caches::new(1_000, 10_000);
        let big = |n: u8| CachedResponse {
            body: serde_json::json!({ "rows": vec!["ab".repeat(1_000); 2], "n": n }),
            etag: "e".into(),
            payment_response: "p".into(),
            immutable: true,
        };
        c.put_settled(Hash32([1; 32]), big(1));
        c.put_settled(Hash32([2; 32]), big(2));
        assert_eq!(c.settled_kept(), 2);
        // A third ~4 KB body does not fit beside two others in 10 KB: the
        // oldest is evicted by weight, although the count cap is 1,000.
        c.put_settled(Hash32([3; 32]), big(3));
        assert_eq!(c.settled_kept(), 2);
        assert!(c.replay_settled(&Hash32([1; 32])).is_none());
        assert!(c.replay_settled(&Hash32([3; 32])).is_some());
        // One body heavier than the whole budget is not kept at all.
        let tiny = Caches::new(1_000, 100);
        tiny.put_settled(Hash32([4; 32]), big(4));
        assert_eq!(tiny.settled_kept(), 0);
    }

    /// M7-L40-F2: all scopes pay for an attempt, or none does.
    #[test]
    fn scoped_attempts_are_all_or_nothing() {
        let c = Caches::default();
        let wide = ("wide".to_string(), 2);
        assert!(c.take_attempts(&[("a".into(), 5), wide.clone()]));
        assert!(c.take_attempts(&[("b".into(), 5), wide.clone()]));
        // The shared scope is empty: refused, and "c" keeps its attempts.
        assert!(!c.take_attempts(&[("c".into(), 5), wide.clone()]));
        for _ in 0..5 {
            assert!(c.take_attempt("c", 5));
        }
    }
    /// M7-L42-F3: fresh keys that are refused at verify (reserve, then
    /// refund) never hold a slot, so 70,000 of them cannot evict a drained
    /// payer and reset its budget.
    #[test]
    fn fresh_refunded_keys_do_not_reset_a_drained_payer() {
        let c = Caches::default();
        let abuser = [0xee; 20];
        assert!(c.reserve_payer(&abuser, 1));
        assert!(!c.payer_allowed(&abuser, 1), "drained");
        for i in 0..70_000u32 {
            let mut k = [0u8; 20];
            k[..4].copy_from_slice(&i.to_be_bytes());
            assert!(c.reserve_payer(&k, 1));
            c.refund_payer(&k, 1);
        }
        assert_eq!(c.payers_tracked(), 1, "only the drained payer is kept");
        assert!(!c.payer_allowed(&abuser, 1), "still drained");
    }
}
