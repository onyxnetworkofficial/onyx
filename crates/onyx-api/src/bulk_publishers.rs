//! Limits per publisher (ADR-0066 item 10; THREATS PUBON-6, PUBON-18).
//!
//! A node may be given a limits file (`ONYX_BULK_PUBLISHERS`): one entry per
//! publisher id with its bytes, its datasets and a state. With it:
//!
//! - a publisher gets its entry's numbers instead of the node's one quota
//!   and one dataset cap ([`BulkLimits::quota_bytes`],
//!   [`BulkLimits::max_datasets`]);
//! - a publisher without an entry is refused on every upload
//!   (`bulk_publisher_not_listed`): registration in the book is not a grant
//!   of disk;
//! - a paused publisher is served and stores nothing new: no new version,
//!   no new shard (`bulk_publisher_paused`). It still takes a catalog in
//!   which every leaf is unchanged or a tombstone, so it can take data
//!   down, and such a catalog that removes a dataset is not held to the
//!   publisher's own bytes and datasets (review LIM-R1 M1);
//! - what a paused publisher holds is still read against the book: a held
//!   manifest or catalog sent again answers 200, a held root is re-anchored,
//!   and a second anchored manifest or catalog marks the fork (LIM-R1 M3);
//! - the grants plus the reserve never exceed the node's total
//!   ([`PublisherLimits::fits`], checked by the node at start), and neither
//!   does what the store holds: each publisher counts with the larger of
//!   its grant and its held bytes, one without an entry with what it holds
//!   ([`BulkStore::with_publishers`], LIM-R1 M2). So a publisher inside its
//!   grant is not pushed into `bulk_node_full`.
//!
//! Without the file nothing here applies. The file is read once, at start:
//! a change needs a restart.

use super::*;
use serde::Deserialize;

/// Largest limits file read.
pub const MAX_PUBLISHERS_FILE_BYTES: usize = 1 << 20;

/// Largest `quota_gib` and `reserve_gib` (the range of `ONYX_BULK_MAX_GIB`).
pub const MAX_GRANT_GIB: u64 = 1 << 20;

/// One publisher's entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublisherGrant {
    /// Bytes over its datasets' latest versions and its stored catalogs.
    pub quota_bytes: u64,
    /// Datasets (and leaves of one catalog).
    pub max_datasets: usize,
    /// Served, but no new data is taken.
    pub paused: bool,
}

/// The limits file: who this node takes data from, and how much.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublisherLimits {
    /// Bytes of the node's total no grant may use.
    pub reserve_bytes: u64,
    /// Entries by publisher id.
    pub grants: BTreeMap<String, PublisherGrant>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDoc {
    reserve_gib: u64,
    publishers: Vec<FileEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
    id: String,
    quota_gib: u64,
    datasets: u64,
    state: String,
}

impl PublisherLimits {
    /// Read the file's JSON:
    /// `{"reserve_gib": 2, "publishers": [{"id": "...", "quota_gib": 1,
    /// "datasets": 16, "state": "open"}]}`. Every field is required and no
    /// other is taken; an id appears once; `quota_gib` is 1 to
    /// [`MAX_GRANT_GIB`], `datasets` 1 to the catalog's leaf cap, `state`
    /// `open` or `paused`.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > MAX_PUBLISHERS_FILE_BYTES {
            return Err(format!(
                "the file is over {MAX_PUBLISHERS_FILE_BYTES} bytes"
            ));
        }
        let doc: FileDoc = serde_json::from_str(text).map_err(|e| format!("not valid: {e}"))?;
        if doc.reserve_gib > MAX_GRANT_GIB {
            return Err(format!("reserve_gib must be 0..={MAX_GRANT_GIB}"));
        }
        let max_datasets = cat::MAX_CATALOG_LEAVES as u64;
        let mut grants = BTreeMap::new();
        for (i, e) in doc.publishers.into_iter().enumerate() {
            if !onyx_core::ids::is_dotted_id(&e.id) {
                return Err(format!("entry {i}: id is not a publisher id"));
            }
            let id = e.id;
            if !(1..=MAX_GRANT_GIB).contains(&e.quota_gib) {
                return Err(format!(
                    "publisher {id}: quota_gib must be 1..={MAX_GRANT_GIB}"
                ));
            }
            if !(1..=max_datasets).contains(&e.datasets) {
                return Err(format!(
                    "publisher {id}: datasets must be 1..={max_datasets}"
                ));
            }
            let paused = match e.state.as_str() {
                "open" => false,
                "paused" => true,
                _ => return Err(format!("publisher {id}: state must be open or paused")),
            };
            let grant = PublisherGrant {
                quota_bytes: e.quota_gib << 30,
                max_datasets: e.datasets as usize,
                paused,
            };
            if grants.insert(id.clone(), grant).is_some() {
                return Err(format!("publisher {id} has two entries"));
            }
        }
        Ok(PublisherLimits {
            reserve_bytes: doc.reserve_gib << 30,
            grants,
        })
    }

    /// Every grant (paused ones too: their data stays) plus the reserve is
    /// within `max_total_bytes`, the node's total.
    pub fn fits(&self, max_total_bytes: u64) -> Result<(), String> {
        let granted = self
            .grants
            .values()
            .fold(0u64, |a, g| a.saturating_add(g.quota_bytes));
        if granted.saturating_add(self.reserve_bytes) > max_total_bytes {
            return Err(format!(
                "the grants of {} publisher(s) ({} bytes) plus the reserve ({} bytes) exceed the node's total ({} bytes, ONYX_BULK_MAX_GIB): lower a grant or raise the total",
                self.grants.len(),
                granted,
                self.reserve_bytes,
                max_total_bytes
            ));
        }
        Ok(())
    }
}

fn not_listed() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "bulk_publisher_not_listed",
        "this node takes no data from this publisher: its operator has not listed it",
    )
}

fn paused(what: &str) -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "bulk_publisher_paused",
        format!("this publisher is paused on this node: {what}"),
    )
}

/// Refuse to store something new for a paused publisher: a new version, a
/// version in a withdrawn one's place, a shard the node does not hold.
pub(super) fn pause_gate(grant: &PublisherGrant) -> Result<(), ApiError> {
    if grant.paused {
        return Err(paused(
            "it takes no new version and no new shard, only a catalog that removes datasets",
        ));
    }
    Ok(())
}

impl BulkStore {
    /// Limits per publisher (`ONYX_BULK_PUBLISHERS`), checked against what
    /// this store holds (review LIM-R1 M2): every publisher counts with the
    /// larger of its grant and its held bytes (a grant lowered below what is
    /// held deletes nothing), a publisher without an entry with what it
    /// holds, and the sum plus the reserve must fit the store's total.
    /// Refused otherwise, naming each publisher that holds more than the
    /// file gives it. The caller has checked [`PublisherLimits::fits`].
    pub fn with_publishers(mut self, limits: PublisherLimits) -> Result<Self, String> {
        let held = self.held_by_publisher();
        let mut sum = limits.reserve_bytes;
        let mut over = Vec::new();
        for (p, g) in &limits.grants {
            let h = held.get(p).copied().unwrap_or(0);
            if h > g.quota_bytes {
                over.push(format!(
                    "{p} holds {h} bytes over its grant of {}",
                    g.quota_bytes
                ));
            }
            sum = sum.saturating_add(h.max(g.quota_bytes));
        }
        for (p, h) in &held {
            if !limits.grants.contains_key(p) && *h > 0 {
                over.push(format!("{p} holds {h} bytes and has no entry"));
                sum = sum.saturating_add(*h);
            }
        }
        if sum > self.limits.max_total_bytes {
            return Err(format!(
                "the limits file does not fit what this node holds: the grants, the bytes held beyond them and the reserve come to {sum} bytes, the node's total is {} ({}): give those publishers an entry that covers what they hold, lower another grant or the reserve, or raise the total",
                self.limits.max_total_bytes,
                if over.is_empty() {
                    "no publisher holds more than its grant".to_string()
                } else {
                    over.join("; ")
                }
            ));
        }
        self.granted = Some(limits);
        Ok(self)
    }

    /// What the node should say at start about its limits file: a closed
    /// node, and each publisher that holds bytes without an entry or over
    /// its grant (names and bytes of what this node itself holds).
    pub fn publisher_notes(&self) -> Vec<String> {
        let Some(l) = &self.granted else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if l.grants.is_empty() {
            out.push(
                "the limits file lists no publisher: every bulk upload is refused".to_string(),
            );
        }
        for (p, h) in self.held_by_publisher() {
            match l.grants.get(&p) {
                None if h > 0 => out.push(format!(
                    "publisher {p} holds {h} bytes and has no entry: it is served, takes no upload and cannot remove a dataset"
                )),
                Some(g) if h > g.quota_bytes => out.push(format!(
                    "publisher {p} holds {h} bytes, over its grant of {}: it takes nothing that adds bytes",
                    g.quota_bytes
                )),
                _ => {}
            }
        }
        out
    }

    /// Bytes each publisher holds: its datasets' latest versions (an erased
    /// dataset's are gone) and its stored catalogs, as the quota counts them.
    fn held_by_publisher(&self) -> BTreeMap<String, u64> {
        let erased = self.erased_datasets();
        let mut held: BTreeMap<String, u64> = BTreeMap::new();
        if let Ok(ds) = self.datasets.read() {
            for (k, d) in ds.iter() {
                let b = if erased.contains(k) {
                    0
                } else {
                    d.latest().map_or(0, |h| h.manifest.header.byte_count)
                };
                let e = held.entry(k.0.clone()).or_default();
                *e = e.saturating_add(b);
            }
        }
        if let Ok(c) = self.catalog_bytes.lock() {
            for (p, b) in c.iter() {
                let e = held.entry(p.clone()).or_default();
                *e = e.saturating_add(*b);
            }
        }
        held
    }

    /// What `publisher` may hold: its entry, or without a limits file the
    /// node's one quota and cap. Refused when the file has no entry for it.
    pub(super) fn grant(&self, publisher: &str) -> Result<PublisherGrant, ApiError> {
        match &self.granted {
            None => Ok(PublisherGrant {
                quota_bytes: self.limits.quota_bytes,
                max_datasets: self.limits.max_datasets,
                paused: false,
            }),
            Some(l) => l.grants.get(publisher).copied().ok_or_else(not_listed),
        }
    }

    /// The refusals of a manifest that need no lock and no chain read: a
    /// publisher without an entry, and a paused publisher's manifest for a
    /// version the node does not hold. A manifest for a held version goes
    /// on: it may be the same one again, a re-anchor, or evidence of a fork.
    pub(super) fn refuse_new_data(&self, h: &onyx_bulk::ManifestHeader) -> Result<(), ApiError> {
        let grant = self.grant(&h.publisher.0)?;
        if !grant.paused {
            return Ok(());
        }
        let held = self
            .datasets
            .read()
            .map_err(|_| internal("lock"))?
            .get(&(h.publisher.0.clone(), h.dataset.clone()))
            .map_or(0, |d| d.versions.len() as u32);
        if h.version > held {
            pause_gate(&grant)?;
        }
        Ok(())
    }

    /// Whether catalog `c`, the next one of its publisher, may be stored
    /// under `grant`, and whether it is a takedown. A paused publisher's
    /// catalog must only take data down (THREATS PUBON-18). A takedown
    /// (every leaf unchanged or a tombstone, at least one tombstone new) is
    /// not held to the publisher's dataset cap, nor to its bytes by the
    /// caller: a grant lowered below what is held must not stop a removal
    /// (review LIM-R1 M1).
    pub(super) fn catalog_gate(
        &self,
        grant: &PublisherGrant,
        c: &Catalog,
    ) -> Result<bool, ApiError> {
        let (down_only, removes) = self.takes_down(c)?;
        if grant.paused && !down_only {
            return Err(paused(
                "a catalog is taken only when every leaf is unchanged or a tombstone",
            ));
        }
        let takedown = down_only && removes;
        if !takedown && c.leaves.len() > grant.max_datasets {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "bulk_quota_exceeded",
                format!(
                    "a publisher may hold at most {} datasets on this node",
                    grant.max_datasets
                ),
            ));
        }
        Ok(takedown)
    }

    /// Catalog `c` against the held catalog before it: whether every leaf
    /// is a tombstone or equal to its leaf there, and whether a tombstone
    /// is new.
    fn takes_down(&self, c: &Catalog) -> Result<(bool, bool), ApiError> {
        let publisher = &c.header.publisher.0;
        let before = match c.header.number.checked_sub(1) {
            Some(k) => self
                .catalog_body(publisher, k)
                .map_err(|e| internal(format!("catalog: {e}")))?
                .map(|b| b.0),
            None => None,
        };
        let unchanged = |l: &CatalogLeaf| {
            before.as_ref().is_some_and(|b| {
                b.catalog
                    .find(&l.dataset)
                    .and_then(|i| b.catalog.leaves.get(i))
                    == Some(l)
            })
        };
        let down_only = c.leaves.iter().all(|l| l.is_tombstone() || unchanged(l));
        let removes = c.leaves.iter().any(|l| l.is_tombstone() && !unchanged(l));
        Ok((down_only, removes))
    }
}
