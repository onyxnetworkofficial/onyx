//! Reserve histories (ADR-0034): the on-chain balance of a published address
//! set, as a chain of `btc-entity-reserve` shards over a committed base.
//!
//! The base comes from a UTXO scan (`bitcoin-cli scantxoutset`) of every
//! label-set address at the node's tip, committed per address
//! ([`ScanSnapshot`], `fixtures/reserves/`). A scan only reads the tip, so the
//! base at an earlier height is the scan minus the flows in between
//! ([`derive_base`]); the chain of shards from that base must then land on the
//! scan again, exactly ([`check_chain`] plus the end check in
//! `examples/export-reserves.rs`). A later scan reconciles with an earlier one
//! the same way ([`reconcile`]): `scan(T0) + flows(T0+1..=T1) == scan(T1)` per
//! address. That is what makes the base checkable by anyone with a node; a
//! re-scan on the same node only shows the node is consistent with itself
//! (THREATS RES-1). [`check_params_chain`] binds each shard's params to the
//! committed chain (RES-7).
//!
//! Trust (docs/spec/recipes.md "Reserve recipe"): the balance of the address
//! set is a rerun (level 1) given the base; the base is one node's read that
//! any other node can reconcile at any later tip; which company the addresses belong to
//! stays the label set's level (4 for the published lists, 3 for OKX's
//! signed subset as a source).

use borsh::BorshSerialize;
use onyx_core::{keccak256, Hash32, LabelSetRef};
use onyx_labels::dated::DatedLabelSet;
use onyx_labels::LabelSet;
use onyx_reserve_schema::{EntityBase, EntityReserveRow, ReserveParams};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::BTreeMap;

/// Errors of the reserve helpers.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReserveError {
    /// The snapshot document is malformed (field named).
    #[error("snapshot: {0}")]
    Snapshot(String),
    /// The snapshot was taken for another label set or address list (RES-2).
    #[error("snapshot does not match the label set: {0}")]
    AddressList(String),
    /// The snapshot's block is not the chain's block at that height (RES-4).
    #[error("snapshot block {0} is not the chain's block at that height")]
    NotOnChain(u64),
    /// Rolling the scan back gave a negative balance: scan and flows disagree.
    #[error("{0}: scan minus flows is negative")]
    Negative(String),
    /// A link of the shard chain is broken (field named).
    #[error("chain: {0}")]
    Chain(String),
    /// A header chain file is malformed, fails proof of work or does not link.
    #[error("headers: {0}")]
    Headers(String),
    /// No header evidence for this height (RES-4).
    #[error("no header for block {0}")]
    NoHeaders(u64),
    /// The block named at this height is not the header chain's: a stale
    /// branch, or a block that was never mined (RES-4).
    #[error("block {0} is not the header chain's block at that height (a stale branch)")]
    StaleBranch(u64),
    /// Fewer confirmations than [`MIN_CONFIRMATIONS`] (RES-4).
    #[error("block {height} has {have} confirmations in the header chain; {need} needed")]
    Unconfirmed {
        /// The block's height.
        height: u64,
        /// Confirmations the header chain shows (the block itself counts).
        have: u64,
        /// Confirmations required.
        need: u64,
    },
    /// Dated membership (RES-2): the set, a window or the flows a change needs.
    #[error("membership: {0}")]
    Membership(String),
}

/// One address of a scan: the UTXOs it held at the scan's block. Only
/// addresses with at least one UTXO are listed; every other scanned address
/// held nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize)]
pub struct ScanRow {
    /// `btc:<address>`, as in the label set.
    pub address: String,
    /// Entity the label set gives the address.
    pub entity: String,
    /// Unspent outputs paying the address.
    pub utxos: u64,
    /// Their total, satoshis (decimal string in JSON).
    #[serde(with = "dec")]
    pub sats: u64,
}

/// A committed UTXO scan of a label set's addresses at one block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanSnapshot {
    /// Document version (1).
    pub version: u32,
    /// How the scan was taken (command and node version), for humans.
    pub method: String,
    /// Height the scan read (the node's tip then).
    pub height: u64,
    /// That block's hash, display order (as `scantxoutset` reports it).
    pub block_hash: String,
    /// Label set name.
    pub label_set_name: String,
    /// Label set version.
    pub label_set_version: u32,
    /// Label set content hash, `0x`-hex.
    pub label_set_hash: String,
    /// Number of addresses scanned (every label of the set).
    pub addresses_scanned: u64,
    /// Rows with a balance, strictly ascending by address.
    pub rows: Vec<ScanRow>,
}

/// The hashed part of a snapshot (Borsh, ADR-0002).
#[derive(BorshSerialize)]
struct ScanCommit<'a> {
    domain: &'a str,
    height: u64,
    block_hash: &'a str,
    label_set_name: &'a str,
    label_set_version: u32,
    label_set_hash: &'a str,
    addresses_scanned: u64,
    rows: &'a [ScanRow],
}

impl ScanSnapshot {
    /// Keccak of the snapshot's Borsh commit: the `base_ref` of the first
    /// shard of a chain built on it. The `method` note is not hashed.
    pub fn hash(&self) -> Hash32 {
        let c = ScanCommit {
            domain: "onyx-reserve-scan-v1",
            height: self.height,
            block_hash: &self.block_hash,
            label_set_name: &self.label_set_name,
            label_set_version: self.label_set_version,
            label_set_hash: &self.label_set_hash,
            addresses_scanned: self.addresses_scanned,
            rows: &self.rows,
        };
        keccak256(&borsh::to_vec(&c).unwrap_or_default())
    }

    /// Check the snapshot against the label set it claims (RES-2): same name,
    /// version and content hash; every label scanned; every row an address of
    /// the set with the set's entity; rows strictly ascending, none empty.
    pub fn check_labels(&self, set: &LabelSet) -> Result<(), ReserveError> {
        let e = |m: &str| Err(ReserveError::AddressList(m.into()));
        if self.version != 1 {
            return Err(ReserveError::Snapshot("version".into()));
        }
        if self.label_set_name != set.name
            || self.label_set_version != set.version
            || self.label_set_hash != format!("0x{}", hex::encode(set.content_hash().0))
        {
            return e("label set name, version or content hash");
        }
        if self.addresses_scanned != set.labels.len() as u64 {
            return e("not every label was scanned");
        }
        let owner: BTreeMap<&str, &str> = set
            .labels
            .iter()
            .map(|l| (l.address.as_str(), l.entity.as_str()))
            .collect();
        for w in self.rows.windows(2) {
            if w[0].address >= w[1].address {
                return Err(ReserveError::Snapshot("rows not strictly ascending".into()));
            }
        }
        for r in &self.rows {
            if r.utxos == 0 || r.sats == 0 {
                return Err(ReserveError::Snapshot(format!("{}: empty row", r.address)));
            }
            match owner.get(r.address.as_str()) {
                Some(ent) if *ent == r.entity => {}
                Some(_) => return e(&format!("{}: another entity", r.address)),
                None => return e(&format!("{}: not in the label set", r.address)),
            }
        }
        Ok(())
    }

    /// Check the snapshot's block against the chain's block at its height
    /// (display hex), e.g. from a header chain checked for proof of work (RES-4).
    pub fn check_block(&self, chain_hash_display: &str) -> Result<(), ReserveError> {
        if self.block_hash != chain_hash_display {
            return Err(ReserveError::NotOnChain(self.height));
        }
        Ok(())
    }

    /// Check the snapshot against a dated label set (ADR-0037, RES-2): same
    /// name, version and content hash; every address of the set scanned
    /// (also rotated-out ones, so their balance at a membership change can be
    /// rolled back); every row an address of the set filed under its entity
    /// at the scan's height ([`DatedLabelSet::scan_entity`]); rows strictly
    /// ascending, none empty.
    pub fn check_dated(&self, set: &DatedLabelSet) -> Result<(), ReserveError> {
        let e = |m: &str| Err(ReserveError::AddressList(m.into()));
        if self.version != 1 {
            return Err(ReserveError::Snapshot("version".into()));
        }
        set.check_structure()
            .map_err(|x| ReserveError::Membership(x.to_string()))?;
        if self.label_set_name != set.name
            || self.label_set_version != set.version
            || self.label_set_hash != format!("0x{}", hex::encode(set.content_hash().0))
        {
            return e("dated label set name, version or content hash");
        }
        // Review D1-R1 F3: heights after the publication may still change in
        // the next version (only heights up to `published` are frozen).
        if self.height > set.published {
            return Err(ReserveError::Membership(format!(
                "the scan at block {} is after the set's publication at {}: a later version could still change those heights",
                self.height, set.published
            )));
        }
        let owner = set.scan_owners(self.height);
        if self.addresses_scanned != owner.len() as u64 {
            return e("not every address of the dated set was scanned");
        }
        for w in self.rows.windows(2) {
            if w[0].address >= w[1].address {
                return Err(ReserveError::Snapshot("rows not strictly ascending".into()));
            }
        }
        for r in &self.rows {
            if r.utxos == 0 || r.sats == 0 {
                return Err(ReserveError::Snapshot(format!("{}: empty row", r.address)));
            }
            match owner.get(r.address.as_str()) {
                Some(ent) if *ent == r.entity => {}
                Some(_) => return e(&format!("{}: another entity", r.address)),
                None => return e(&format!("{}: not in the label set", r.address)),
            }
        }
        Ok(())
    }

    /// Total per entity over the addresses that are members at height `h`
    /// (ADR-0037): every entity with a member at `h`, zero without a row, in
    /// entity order. For an undated set read with
    /// [`DatedLabelSet::from_undated`] this is [`Self::entity_totals`].
    pub fn totals_at(&self, set: &DatedLabelSet, h: u64) -> Result<Vec<EntityBase>, ReserveError> {
        let owner: BTreeMap<&str, &str> = set
            .members_at(h)
            .map(|m| (m.label.address.as_str(), m.label.entity.as_str()))
            .collect();
        let mut t: BTreeMap<String, u64> = owner.values().map(|e| ((*e).to_string(), 0)).collect();
        for r in &self.rows {
            if let Some(e) = owner.get(r.address.as_str()) {
                let v = t
                    .get_mut(*e)
                    .ok_or_else(|| ReserveError::AddressList((*e).to_string()))?;
                *v = v
                    .checked_add(r.sats)
                    .ok_or_else(|| ReserveError::Snapshot("overflow".into()))?;
            }
        }
        Ok(t.into_iter()
            .map(|(entity, balance_sats)| EntityBase {
                entity,
                balance_sats,
            })
            .collect())
    }

    /// Total per entity of the set, zero for an entity with no row, in entity order.
    pub fn entity_totals(&self, set: &LabelSet) -> Result<Vec<EntityBase>, ReserveError> {
        let mut t: BTreeMap<String, u64> =
            set.labels.iter().map(|l| (l.entity.clone(), 0)).collect();
        for r in &self.rows {
            let v = t
                .get_mut(&r.entity)
                .ok_or_else(|| ReserveError::AddressList(r.entity.clone()))?;
            *v = v
                .checked_add(r.sats)
                .ok_or_else(|| ReserveError::Snapshot("overflow".into()))?;
        }
        Ok(t.into_iter()
            .map(|(entity, balance_sats)| EntityBase {
                entity,
                balance_sats,
            })
            .collect())
    }
}

/// Base at an earlier height: `totals` at the scan minus each entity's net
/// flow over the blocks after the base up to the scan (`net`, received minus
/// sent; an entity without flow is 0). Negative means scan and flows
/// disagree, and nothing is derived.
pub fn derive_base(
    totals: &[EntityBase],
    net: &BTreeMap<String, i128>,
) -> Result<Vec<EntityBase>, ReserveError> {
    if let Some(k) = net.keys().find(|k| !totals.iter().any(|t| &t.entity == *k)) {
        return Err(ReserveError::AddressList(format!(
            "{k}: flow for an unscanned entity"
        )));
    }
    totals
        .iter()
        .map(|t| {
            let n = net.get(&t.entity).copied().unwrap_or(0);
            let b = i128::from(t.balance_sats) - n;
            Ok(EntityBase {
                entity: t.entity.clone(),
                balance_sats: u64::try_from(b)
                    .map_err(|_| ReserveError::Negative(t.entity.clone()))?,
            })
        })
        .collect()
}

/// Base at an earlier height, address by address: each scanned address's
/// balance minus its own net flow (`flows`: `btc:<address>` to
/// `(received, sent)` over the blocks after the base up to the scan) must not
/// be negative (R-RES-NEG-2), then the addresses add up per entity. Stronger
/// than [`derive_base`], which only sees entity totals: an address short by N
/// and another of the same entity long by N pass there. Checked at the base
/// height only; the recipe checks entity balances at every transaction
/// (R-RES-NEG-1).
pub fn derive_base_by_address(
    scan: &ScanSnapshot,
    set: &LabelSet,
    flows: &BTreeMap<String, (u128, u128)>,
) -> Result<Vec<EntityBase>, ReserveError> {
    let owner: BTreeMap<&str, &str> = set
        .labels
        .iter()
        .map(|l| (l.address.as_str(), l.entity.as_str()))
        .collect();
    let held: BTreeMap<&str, u64> = scan
        .rows
        .iter()
        .map(|r| (r.address.as_str(), r.sats))
        .collect();
    let mut totals: BTreeMap<String, i128> =
        set.labels.iter().map(|l| (l.entity.clone(), 0)).collect();
    let mut all: Vec<&str> = held.keys().copied().collect();
    all.extend(flows.keys().map(String::as_str));
    all.sort_unstable();
    all.dedup();
    for a in all {
        let e = owner
            .get(a)
            .ok_or_else(|| ReserveError::AddressList(format!("{a}: not in the label set")))?;
        let (rx, tx) = flows.get(a).copied().unwrap_or((0, 0));
        let rx = i128::try_from(rx).map_err(|_| ReserveError::Snapshot("overflow".into()))?;
        let tx = i128::try_from(tx).map_err(|_| ReserveError::Snapshot("overflow".into()))?;
        let b = i128::from(held.get(a).copied().unwrap_or(0)) - rx + tx;
        if b < 0 {
            return Err(ReserveError::Negative(a.to_string()));
        }
        let t = totals
            .get_mut(*e)
            .ok_or_else(|| ReserveError::AddressList((*e).to_string()))?;
        *t += b;
    }
    totals
        .into_iter()
        .map(|(entity, b)| {
            Ok(EntityBase {
                balance_sats: u64::try_from(b)
                    .map_err(|_| ReserveError::Snapshot("overflow".into()))?,
                entity,
            })
        })
        .collect()
}

/// One block's flows: its height and `(received, sent)` per `btc:<address>`.
pub type BlockFlows = (u64, BTreeMap<String, (u128, u128)>);

/// The per-address check at every height (THREATS RES-9): walking back from
/// the scan block by block (`blocks` in ascending height, each block's
/// `(received, sent)` per `btc:<address>`), every address's balance after
/// every block, its scan minus its own flows after that block, must not be
/// negative, and every flow must be at an address of the set. Catches what
/// the base-height check ([`derive_base_by_address`]) misses: an address the
/// scan left out that sent coins and got them back inside the window.
pub fn check_running_balances(
    scan: &ScanSnapshot,
    set: &LabelSet,
    blocks: &[BlockFlows],
) -> Result<(), ReserveError> {
    let owner: std::collections::BTreeSet<&str> =
        set.labels.iter().map(|l| l.address.as_str()).collect();
    let mut bal: BTreeMap<String, i128> = scan
        .rows
        .iter()
        .map(|r| (r.address.clone(), i128::from(r.sats)))
        .collect();
    for (h, flows) in blocks.iter().rev() {
        for (a, (rx, tx)) in flows {
            if !owner.contains(a.as_str()) {
                return Err(ReserveError::AddressList(format!(
                    "{a}: not in the label set"
                )));
            }
            let rx = i128::try_from(*rx).map_err(|_| ReserveError::Snapshot("overflow".into()))?;
            let tx = i128::try_from(*tx).map_err(|_| ReserveError::Snapshot("overflow".into()))?;
            let b = bal.entry(a.clone()).or_default();
            *b = *b - rx + tx;
            if *b < 0 {
                return Err(ReserveError::Negative(format!(
                    "{a} after block {}",
                    h.saturating_sub(1)
                )));
            }
        }
    }
    Ok(())
}

/// Reconcile two scans of the same label set (RES-1): per address,
/// `earlier + received - sent == later`, where `flows` holds each address's
/// `(received, sent)` over the blocks after the earlier scan up to the later
/// one. Returns the addresses that disagree (empty: the scans reconcile).
pub fn reconcile(
    earlier: &ScanSnapshot,
    later: &ScanSnapshot,
    flows: &BTreeMap<String, (u128, u128)>,
) -> Result<Vec<String>, ReserveError> {
    if earlier.label_set_hash != later.label_set_hash || earlier.height >= later.height {
        return Err(ReserveError::Snapshot(
            "scans of different sets or out of order".into(),
        ));
    }
    let a: BTreeMap<&str, u64> = earlier
        .rows
        .iter()
        .map(|r| (r.address.as_str(), r.sats))
        .collect();
    let b: BTreeMap<&str, u64> = later
        .rows
        .iter()
        .map(|r| (r.address.as_str(), r.sats))
        .collect();
    let mut all: Vec<&str> = a.keys().chain(b.keys()).copied().collect();
    all.extend(flows.keys().map(String::as_str));
    all.sort_unstable();
    all.dedup();
    Ok(all
        .into_iter()
        .filter(|addr| {
            let (rx, tx) = flows.get(*addr).copied().unwrap_or((0, 0));
            let want = i128::from(*a.get(addr).unwrap_or(&0)) + rx as i128 - tx as i128;
            want != i128::from(*b.get(addr).unwrap_or(&0))
        })
        .map(str::to_string)
        .collect())
}

/// One shard of a reserve history: its range, Merkle root and decoded rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReserveShard {
    /// First height.
    pub from: u64,
    /// Last height (inclusive).
    pub to: u64,
    /// Merkle root of the shard's leaves.
    pub root: Hash32,
    /// The decoded rows (one per entity, ascending).
    pub rows: Vec<EntityReserveRow>,
}

/// Check a chain of reserve shards (R-RES-CHAIN-1): the first shard starts
/// right after `base_height` on `base` with `base_ref = snapshot`; every next
/// shard starts right after the previous one, on its end balances, with
/// `base_ref` = the previous shard's root; every row names its range. Returns
/// the end balances.
pub fn check_chain(
    base_height: u64,
    base: &[EntityBase],
    snapshot: &Hash32,
    shards: &[ReserveShard],
) -> Result<Vec<EntityBase>, ReserveError> {
    let mut height = base_height;
    let mut state: Vec<EntityBase> = base.to_vec();
    let mut link = *snapshot;
    for s in shards {
        let at = |m: &str| ReserveError::Chain(format!("{}-{}: {m}", s.from, s.to));
        if height.checked_add(1) != Some(s.from) || s.to < s.from {
            return Err(at("does not start after the previous shard"));
        }
        if s.rows.len() != state.len() {
            return Err(at("not one row per entity"));
        }
        for (r, b) in s.rows.iter().zip(&state) {
            if r.entity != b.entity || r.base_sats != b.balance_sats {
                return Err(at(&format!("{}: base is not the previous end", r.entity)));
            }
            if r.base_height != height || r.height != s.to || r.base_ref != link.0 {
                return Err(at(&format!("{}: range or base_ref", r.entity)));
            }
            let end = i128::from(r.base_sats) + r.received_sats as i128 - r.sent_sats as i128;
            if end != i128::from(r.balance_sats) {
                return Err(at(&format!("{}: balance does not add up", r.entity)));
            }
        }
        state = s
            .rows
            .iter()
            .map(|r| EntityBase {
                entity: r.entity.clone(),
                balance_sats: r.balance_sats,
            })
            .collect();
        height = s.to;
        link = s.root;
    }
    Ok(state)
}

/// Bind each shard's params to the committed chain (R-RES-PARAMS-1, THREATS
/// RES-7): the first shard's `ReserveParams` must be the committed start
/// (`base_height`, `base_ref` = the snapshot hash, `base`), every next one the
/// previous finalized shard's end (its last height, its root, its rows'
/// `balance_sats`), and the shards must chain ([`check_chain`]). A rerun
/// cannot see a forged base (the recipe is a pure function of its params, so
/// miner and validators agree on any base); this check, from the committed
/// snapshot, is what refuses it. `params[i]` is the decoded `params` of the
/// spec that produced `shards[i]`. Returns the end balances.
pub fn check_params_chain(
    base_height: u64,
    base: &[EntityBase],
    snapshot: &Hash32,
    params: &[ReserveParams],
    shards: &[ReserveShard],
) -> Result<Vec<EntityBase>, ReserveError> {
    if params.len() != shards.len() {
        return Err(ReserveError::Chain("one params per shard".into()));
    }
    let end = check_chain(base_height, base, snapshot, shards)?;
    let (mut height, mut state, mut link) = (base_height, base.to_vec(), *snapshot);
    for (p, s) in params.iter().zip(shards) {
        if p.base_height != height || p.base_ref != link.0 || p.base != state {
            return Err(ReserveError::Chain(format!(
                "{}-{}: params are not the previous finalized end",
                s.from, s.to
            )));
        }
        state = s
            .rows
            .iter()
            .map(|r| EntityBase {
                entity: r.entity.clone(),
                balance_sats: r.balance_sats,
            })
            .collect();
        height = s.to;
        link = s.root;
    }
    Ok(end)
}

/// Keccak of a Borsh-encoded base list (used in the commitment).
pub fn base_hash(base: &[EntityBase]) -> Hash32 {
    keccak256(&borsh::to_vec(&base.to_vec()).unwrap_or_default())
}

/// The hash a website pins for a reserve history: domain, recipe id, label set
/// content hash, snapshot hash, scan height and block hash (internal order),
/// base height, the hashes of the base and of the scan's entity totals, then
/// each shard as `from (u64 BE) || to (u64 BE) || root`.
#[allow(clippy::too_many_arguments)]
pub fn reserve_commitment(
    recipe: &Hash32,
    label_set: &Hash32,
    snapshot: &Hash32,
    scan_height: u64,
    scan_block: &[u8; 32],
    base_height: u64,
    base: &[EntityBase],
    scan_totals: &[EntityBase],
    shards: &[ReserveShard],
) -> Hash32 {
    let mut b = Vec::with_capacity(16 + 32 * 6 + 16 + shards.len() * 48);
    b.extend_from_slice(b"onyx-reserves-v1");
    b.extend_from_slice(&recipe.0);
    b.extend_from_slice(&label_set.0);
    b.extend_from_slice(&snapshot.0);
    b.extend_from_slice(&scan_height.to_be_bytes());
    b.extend_from_slice(scan_block);
    b.extend_from_slice(&base_height.to_be_bytes());
    b.extend_from_slice(&base_hash(base).0);
    b.extend_from_slice(&base_hash(scan_totals).0);
    for s in shards {
        b.extend_from_slice(&s.from.to_be_bytes());
        b.extend_from_slice(&s.to.to_be_bytes());
        b.extend_from_slice(&s.root.0);
    }
    keccak256(&b)
}

// ------------------------------------------------------- confirmations (RES-4)

/// Confirmations a block needs before a scan of it, a flows file ending at
/// it, or a reserve shard ending at it is used (ADR-0037, THREATS RES-4).
/// Counted as Bitcoin Core counts them: the block itself plus every block on
/// top, so 6 is the block and five more. Bitcoin whitepaper section 11
/// (https://bitcoin.org/bitcoin.pdf): an attacker with 10% of the hash power
/// catches up from 5 blocks behind with probability 0.0009137, under 0.1%.
pub const MIN_CONFIRMATIONS: u64 = 6;

/// A run of Bitcoin headers checked for proof of work against a pinned floor
/// and for parent links (RES-4): the evidence that a scan, a flows file or a
/// shard is on the chain, and how deep. Hashes are display hex, by height.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderChain {
    from: u64,
    hashes: Vec<String>,
}

/// A header chain file (`fixtures/reserves/headers-<from>-<to>.json`,
/// `scripts/build-reserve-headers.py`).
#[derive(Deserialize)]
struct HeaderFile {
    version: u32,
    from: u64,
    headers: Vec<String>,
}

impl HeaderChain {
    /// Check raw 80-byte headers for heights `from..`: each meets `min_bits`
    /// and its own target (`node::check_raw_header`), keeps the bits of the
    /// header before it unless a difficulty period starts at its height (and
    /// then moves the target at most 4x; `node::permitted_bits_transition`,
    /// RES-20), and names the one before as its parent.
    pub fn from_raw(from: u64, headers: &[Vec<u8>], min_bits: u32) -> Result<Self, ReserveError> {
        if headers.is_empty() {
            return Err(ReserveError::Headers("no header".into()));
        }
        let mut hashes = Vec::with_capacity(headers.len());
        let mut prev: Option<[u8; 32]> = None;
        let mut prev_bits: Option<u32> = None;
        for (i, raw) in headers.iter().enumerate() {
            let h = from
                .checked_add(i as u64)
                .ok_or_else(|| ReserveError::Headers("height overflow".into()))?;
            let (hash, parent) = crate::node::check_raw_header(raw, min_bits)
                .map_err(|e| ReserveError::Headers(format!("{h}: {e}")))?;
            // 80 bytes (checked above); nBits at bytes 72..76, little endian.
            let bits = u32::from_le_bytes([raw[72], raw[73], raw[74], raw[75]]);
            if prev_bits.is_some_and(|p| !crate::node::permitted_bits_transition(h, p, bits)) {
                return Err(ReserveError::Headers(format!(
                    "{h}: difficulty bits {bits:#010x} break the retarget rule (RES-20)"
                )));
            }
            prev_bits = Some(bits);
            if prev.is_some_and(|p| p != parent) {
                return Err(ReserveError::Headers(format!(
                    "{h}: does not link to the header before"
                )));
            }
            prev = Some(hash);
            let mut display = hash;
            display.reverse();
            hashes.push(hex::encode(display));
        }
        Ok(HeaderChain { from, hashes })
    }

    /// Parse and check a header chain file (version 1).
    pub fn from_json(text: &str, min_bits: u32) -> Result<Self, ReserveError> {
        let f: HeaderFile =
            serde_json::from_str(text).map_err(|e| ReserveError::Headers(e.to_string()))?;
        if f.version != 1 {
            return Err(ReserveError::Headers("version".into()));
        }
        let raw = f
            .headers
            .iter()
            .map(|h| hex::decode(h).map_err(|_| ReserveError::Headers("hex".into())))
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_raw(f.from, &raw, min_bits)
    }

    /// First height.
    pub fn first(&self) -> u64 {
        self.from
    }

    /// Last height.
    pub fn tip(&self) -> u64 {
        self.from + self.hashes.len() as u64 - 1
    }

    /// The chain's block at `h` (display hex), if it has one.
    pub fn hash_at(&self, h: u64) -> Option<&str> {
        h.checked_sub(self.from)
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| self.hashes.get(i))
            .map(String::as_str)
    }

    /// The same chain cut at `tip` (tests and what-if checks).
    pub fn truncated(&self, tip: u64) -> Option<HeaderChain> {
        let n = usize::try_from(tip.checked_sub(self.from)?.checked_add(1)?).ok()?;
        (n <= self.hashes.len()).then(|| HeaderChain {
            from: self.from,
            hashes: self.hashes[..n].to_vec(),
        })
    }

    /// Confirmations of block `hash` (display hex) at height `h`: the chain's
    /// tip minus `h`, plus one. Refused when the chain has another block at
    /// `h` (a stale branch) or no header there.
    pub fn confirmations(&self, h: u64, hash: &str) -> Result<u64, ReserveError> {
        let want = self.hash_at(h).ok_or(ReserveError::NoHeaders(h))?;
        if !want.eq_ignore_ascii_case(hash) {
            return Err(ReserveError::StaleBranch(h));
        }
        Ok(self.tip() - h + 1)
    }

    /// [`Self::confirmations`] at least `need`.
    pub fn require_confirmed(&self, h: u64, hash: &str, need: u64) -> Result<(), ReserveError> {
        let have = self.confirmations(h, hash)?;
        if have < need {
            return Err(ReserveError::Unconfirmed {
                height: h,
                have,
                need,
            });
        }
        Ok(())
    }
}

// ------------------------------------------------------------ serving check

/// Bases a cross-check may have (THREATS RES-1, ADR-0034 Amendment 1).
pub const CROSS_CHECK_BASES: [&str; 3] = [
    "published-por-file",
    "issuer-wallet-list",
    "second-node-rescan",
];

/// One independent check of an entity's addresses in a committed scan
/// (THREATS RES-1): a source that is not Onyx's own node (the company's
/// proof-of-reserves file, the issuer's wallet list) agreed with the scan,
/// address by address. Recorded by `scripts/reserve-live-check.py`; the
/// figures are in docs/ops/indexer-integration.md.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossCheckRecord {
    /// Entity whose addresses were compared.
    pub entity: String,
    /// One of [`CROSS_CHECK_BASES`] except `second-node-rescan`.
    pub basis: String,
    /// What was compared (file name, page and its date).
    pub source: String,
    /// Where the source is published.
    pub url: String,
    /// Addresses compared.
    pub addresses_compared: u64,
    /// Of those, the ones that agreed to the satoshi.
    pub addresses_exact: u64,
    /// The source's own total over the compared addresses, in satoshis (decimal string).
    /// Shown on the website; the serving check does not read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theirs_sats: Option<String>,
    /// Onyx's total over the same addresses, in satoshis (decimal string).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onyx_sats: Option<String>,
    /// What the compared addresses cover of the entity's labelled ones.
    pub coverage: String,
    /// When the check was recorded (UTC date).
    pub recorded: String,
    /// The command that ran it.
    pub method: String,
}

/// The committed cross-checks of one scan (`fixtures/reserves/cross-checks.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossChecks {
    /// Document version (1).
    pub version: u32,
    /// [`ScanSnapshot::hash`] of the scan they checked, `0x`-hex.
    pub scan_hash: String,
    /// Content hash of the label set, `0x`-hex.
    pub label_set_hash: String,
    /// One record per checked entity.
    pub checks: Vec<CrossCheckRecord>,
}

/// Why an entity's reserve is served: the basis of its cross-check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CrossCheck {
    /// One of [`CROSS_CHECK_BASES`].
    pub basis: String,
    /// What it was checked against.
    pub source: String,
}

/// A re-scan of the same label set on a second node, configured by the
/// operator, with each address's `(received, sent)` over the blocks after the
/// committed scan up to this one (empty at the same height). The server
/// cannot tell two nodes apart: that this is another node is the operator's
/// statement (ADR-0034 Amendment 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecondScan {
    /// The second node's scan.
    pub scan: ScanSnapshot,
    /// Flows per `btc:<address>` between the two scans.
    pub flows: BTreeMap<String, (u128, u128)>,
}

/// The label set a served chain is anchored on (ADR-0037).
#[derive(Clone, Copy, Debug)]
pub enum AnchorLabels<'a> {
    /// An undated set: every label counts at every height (the RES-2
    /// residual: a rotated-out address keeps counting).
    Undated(&'a LabelSet),
    /// A dated set (the caller has checked its version history with
    /// `onyx_labels::dated::check_history`, as the reserve gate does): a
    /// shard over blocks `f..=t` must run the window
    /// [`DatedLabelSet::window`]`(f, t)`, and a membership change moves the
    /// changing addresses' balances at the change, rolled back from the scan
    /// with `flows` (a flows file over the blocks after the earliest change
    /// up to the scan, or from the scan to a later change; `None`: changes
    /// only at the scan's own height are possible).
    Dated {
        /// The dated set the scan names.
        set: &'a DatedLabelSet,
        /// Per-address flows for the membership changes.
        flows: Option<&'a FlowsFile>,
    },
}

impl<'a> AnchorLabels<'a> {
    /// The scan is a scan of this set.
    pub fn check_scan(&self, scan: &ScanSnapshot) -> Result<(), ReserveError> {
        match self {
            AnchorLabels::Undated(set) => scan.check_labels(set),
            AnchorLabels::Dated { set, .. } => scan.check_dated(set),
        }
    }

    /// The dated reading ([`DatedLabelSet::from_undated`] for an undated set).
    pub fn dated(&self) -> Cow<'a, DatedLabelSet> {
        match self {
            AnchorLabels::Undated(set) => Cow::Owned(DatedLabelSet::from_undated(set)),
            AnchorLabels::Dated { set, .. } => Cow::Borrowed(*set),
        }
    }

    fn flows(&self) -> Option<&'a FlowsFile> {
        match self {
            AnchorLabels::Undated(_) => None,
            AnchorLabels::Dated { flows, .. } => *flows,
        }
    }
}

/// What a served reserve chain is anchored on: the committed scan, its label
/// set, the header chain that confirms it (RES-4), the pinned start height
/// (the first shard's base height), the pinned recipe, and the cross-checks.
#[derive(Clone, Copy, Debug)]
pub struct ServingAnchor<'a> {
    /// The committed scan the chain must land on.
    pub scan: &'a ScanSnapshot,
    /// Its label set, undated or dated.
    pub labels: AnchorLabels<'a>,
    /// Proof-of-work-checked headers from the first shard's first block to
    /// at least [`MIN_CONFIRMATIONS`] - 1 blocks past the scan.
    pub headers: &'a HeaderChain,
    /// Base height of the first shard of the served chain.
    pub start_height: u64,
    /// The `btc-entity-reserve` build the shards must run.
    pub recipe: Hash32,
    /// Committed independent checks.
    pub cross_checks: &'a CrossChecks,
    /// Operator-configured second-node re-scans.
    pub second_scans: &'a [SecondScan],
}

/// One shard the chain finalized, as the API holds it: its binding inputs,
/// range (from the pinned source snapshot), params (from the store, by the
/// params hash the shard id commits to) and the leaves that hash to the
/// finalized root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedShard {
    /// Round.
    pub round: u64,
    /// Shard id.
    pub shard: Hash32,
    /// Finalized root.
    pub root: Hash32,
    /// Recipe id of the binding.
    pub recipe: Hash32,
    /// `LabelSetRef::hash` of the binding.
    pub label_set_hash: Hash32,
    /// Schema id of the binding.
    pub schema: Hash32,
    /// First height of the source snapshot.
    pub from: u64,
    /// Last height of the source snapshot.
    pub to: u64,
    /// Tip hash of the source snapshot (the block at `to`, display byte
    /// order as `BlockRange::tip_hash` holds it).
    pub tip: Hash32,
    /// The spec's params bytes, when the store holds them.
    pub params: Option<Vec<u8>>,
    /// Leaves (Borsh `OutputRow`s) in tree order.
    pub leaves: Vec<Vec<u8>>,
}

/// A row the serving check lets through, with its entity's cross-check
/// (`None`: not independently cross-checked; the API refuses it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServedRow {
    /// Leaf index.
    pub index: u64,
    /// Entity (the row key).
    pub entity: String,
    /// The entity's cross-check, if any.
    pub cross_check: Option<CrossCheck>,
}

/// The serving check's answer for every shard it was given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServingVerdict {
    /// The start base derived from the scan and the finalized flows, when the
    /// chain reached the scan.
    pub start_base: Option<Vec<EntityBase>>,
    /// Each entity's cross-check.
    pub cross_checks: BTreeMap<String, Option<CrossCheck>>,
    /// Per input shard, in input order: its rows, or why it is refused.
    pub shards: Vec<(u64, Hash32, Result<Vec<ServedRow>, String>)>,
}

/// Per-address `(received, sent)` from a flows file
/// (`scripts/stream-reserve-flows.py`, one block per line:
/// `{"height", "hash", "prev", "flows": {address: [received, sent]}}`),
/// keyed `btc:<address>`. Heights must be contiguous and each block must
/// name the one before as `prev`; returns the flows with the first block's
/// `prev` and the last block's height and hash.
pub fn flows_from_jsonl(text: &str) -> Result<FlowsFile, ReserveError> {
    let bad = |m: &str| ReserveError::Snapshot(format!("flows: {m}"));
    let mut out = FlowsFile::default();
    for (i, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let mut block = BTreeMap::new();
        let b: serde_json::Value = serde_json::from_str(line).map_err(|_| bad("not JSON"))?;
        let h = b["height"].as_u64().ok_or_else(|| bad("height"))?;
        let prev = b["prev"].as_str().ok_or_else(|| bad("prev"))?;
        if i == 0 {
            out.parent = prev.to_string();
        } else if h != out.height + 1 || prev != out.hash {
            return Err(bad("blocks are not contiguous"));
        }
        out.height = h;
        out.hash = b["hash"].as_str().ok_or_else(|| bad("hash"))?.to_string();
        for (a, v) in b["flows"].as_object().ok_or_else(|| bad("flows"))? {
            let rx = v[0].as_u64().ok_or_else(|| bad("received"))?;
            let tx = v[1].as_u64().ok_or_else(|| bad("sent"))?;
            let e = out.flows.entry(format!("btc:{a}")).or_default();
            e.0 += u128::from(rx);
            e.1 += u128::from(tx);
            block.insert(format!("btc:{a}"), (u128::from(rx), u128::from(tx)));
        }
        out.blocks.push(FlowBlock {
            height: h,
            hash: out.hash.clone(),
            flows: block,
        });
    }
    Ok(out)
}

/// A parsed flows file ([`flows_from_jsonl`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlowsFile {
    /// Display hash of the block before the first.
    pub parent: String,
    /// Last height.
    pub height: u64,
    /// Display hash of the last block.
    pub hash: String,
    /// Per `btc:<address>`: (received, sent).
    pub flows: BTreeMap<String, (u128, u128)>,
    /// The same, block by block, in height order (ADR-0037).
    pub blocks: Vec<FlowBlock>,
}

/// One block of a flows file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlowBlock {
    /// Height.
    pub height: u64,
    /// Display hash.
    pub hash: String,
    /// Per `btc:<address>`: (received, sent) in this block.
    pub flows: BTreeMap<String, (u128, u128)>,
}

impl FlowsFile {
    /// The flows file is on the header chain (RES-4): every block is the
    /// chain's block at its height, the first names the chain's block before
    /// it as its parent (when the chain has it), and the last has at least
    /// `need` confirmations. A file over a stale branch, or not yet deep
    /// enough, is refused.
    pub fn check_on_chain(&self, chain: &HeaderChain, need: u64) -> Result<(), ReserveError> {
        let last = self
            .blocks
            .last()
            .ok_or_else(|| ReserveError::Snapshot("flows: no block".into()))?;
        for b in &self.blocks {
            chain.confirmations(b.height, &b.hash)?;
        }
        if let Some(first) = self.blocks.first() {
            let before = first.height.saturating_sub(1);
            if let Some(p) = chain.hash_at(before) {
                if !p.eq_ignore_ascii_case(&self.parent) {
                    return Err(ReserveError::StaleBranch(before));
                }
            }
        }
        chain.require_confirmed(last.height, &last.hash, need)
    }

    /// The summary fields agree with the blocks (review D22-R1-3:
    /// [`Self::hash`] commits the blocks only): `height` and `hash` are the
    /// last block's, and `flows` is the blocks' sum per address.
    pub fn check_consistent(&self) -> Result<(), ReserveError> {
        let bad = |m: &str| Err(ReserveError::Snapshot(format!("flows: {m}")));
        let Some(last) = self.blocks.last() else {
            return bad("no block");
        };
        if (last.height, last.hash.as_str()) != (self.height, self.hash.as_str()) {
            return bad("the summary is not the last block");
        }
        // Review D22-R2-2: one block per height, in order (as
        // `flows_from_jsonl` reads them): a repeated block would count its
        // flows twice under a hash the client trusts.
        for w in self.blocks.windows(2) {
            if w[0].height.checked_add(1) != Some(w[1].height) {
                return bad("blocks are not contiguous");
            }
        }
        let mut sum: BTreeMap<String, (u128, u128)> = BTreeMap::new();
        for b in &self.blocks {
            for (a, (rx, tx)) in &b.flows {
                let e = sum.entry(a.clone()).or_default();
                e.0 += rx;
                e.1 += tx;
            }
        }
        if sum != self.flows {
            return bad("the per-address totals are not the blocks' sum");
        }
        Ok(())
    }

    /// Keccak of the file's Borsh commit (ADR-0002, ADR-0003): the first
    /// block's parent, then every block's height, hash and per-address
    /// `(received, sent)` in address order. What a client trusts before it
    /// applies a membership change from the file (ROADMAP D22,
    /// [`ReservePin::from_dated_scan`]): the per-address flows are the file's
    /// word (THREATS RES-13), so a file taken from the operator would let the
    /// operator move a change's delta.
    pub fn hash(&self) -> Hash32 {
        /// One block: height, hash, `(address, received, sent)` in address order.
        type Block<'a> = (u64, &'a str, Vec<(&'a str, u128, u128)>);
        #[derive(BorshSerialize)]
        struct Commit<'a> {
            domain: &'a str,
            parent: &'a str,
            blocks: Vec<Block<'a>>,
        }
        let c = Commit {
            domain: "onyx-reserve-flows-v1",
            parent: &self.parent,
            blocks: self
                .blocks
                .iter()
                .map(|b| {
                    (
                        b.height,
                        b.hash.as_str(),
                        b.flows
                            .iter()
                            .map(|(a, (rx, tx))| (a.as_str(), *rx, *tx))
                            .collect(),
                    )
                })
                .collect(),
        };
        keccak256(&borsh::to_vec(&c).unwrap_or_default())
    }
}

/// Same-height comparison or [`reconcile`]: the addresses where `later`
/// disagrees with `earlier` plus the flows in between.
fn reconcile_at_or_after(
    earlier: &ScanSnapshot,
    later: &ScanSnapshot,
    flows: &BTreeMap<String, (u128, u128)>,
) -> Result<Vec<String>, ReserveError> {
    if later.height > earlier.height {
        return reconcile(earlier, later, flows);
    }
    if later.height < earlier.height
        || later.block_hash != earlier.block_hash
        || later.label_set_hash != earlier.label_set_hash
        || !flows.is_empty()
    {
        return Err(ReserveError::Snapshot(
            "a second scan must be of the same set at the same block or later".into(),
        ));
    }
    let a: BTreeMap<&str, u64> = earlier
        .rows
        .iter()
        .map(|r| (r.address.as_str(), r.sats))
        .collect();
    let b: BTreeMap<&str, u64> = later
        .rows
        .iter()
        .map(|r| (r.address.as_str(), r.sats))
        .collect();
    let mut all: Vec<&str> = a.keys().chain(b.keys()).copied().collect();
    all.sort_unstable();
    all.dedup();
    Ok(all
        .into_iter()
        .filter(|k| a.get(k) != b.get(k))
        .map(str::to_string)
        .collect())
}

/// Each entity of the set and its cross-check (THREATS RES-1): a committed
/// record of an independent source for THIS scan and label set (every
/// compared address exact), or a second-node re-scan that reconciles with the
/// scan at every address of the entity. Records for another scan or set, or
/// with any mismatch, do not count. A second scan whose `method` note is the
/// committed scan's (a copy of the same file) does not count either, nor does
/// one whose block is not on the header chain with [`MIN_CONFIRMATIONS`]
/// (RES-4).
pub fn cross_checked_entities(
    a: &ServingAnchor<'_>,
) -> Result<BTreeMap<String, Option<CrossCheck>>, ReserveError> {
    let d = a.labels.dated();
    let mut out: BTreeMap<String, Option<CrossCheck>> = d
        .members
        .iter()
        .map(|m| (m.label.entity.clone(), None))
        .collect();
    let c = a.cross_checks;
    if c.version == 1
        && c.scan_hash == a.scan.hash().to_string()
        && c.label_set_hash == a.scan.label_set_hash
    {
        for r in &c.checks {
            let ok = CROSS_CHECK_BASES[..2].contains(&r.basis.as_str())
                && r.addresses_compared > 0
                && r.addresses_exact == r.addresses_compared;
            if let (true, Some(slot)) = (ok, out.get_mut(&r.entity)) {
                *slot = Some(CrossCheck {
                    basis: r.basis.clone(),
                    source: r.source.clone(),
                });
            }
        }
    }
    // Review D1-R1 F5: a disagreeing address counts against every entity it
    // ever belongs to (it may move between the two scans).
    let mut owners: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for m in &d.members {
        owners
            .entry(m.label.address.as_str())
            .or_default()
            .push(m.label.entity.as_str());
    }
    for s in a.second_scans {
        if s.scan.method == a.scan.method || a.labels.check_scan(&s.scan).is_err() {
            continue;
        }
        // RES-4: a re-scan counts only once its block is on the header chain
        // with enough confirmations.
        if a.headers
            .require_confirmed(s.scan.height, &s.scan.block_hash, MIN_CONFIRMATIONS)
            .is_err()
        {
            continue;
        }
        let Ok(bad) = reconcile_at_or_after(a.scan, &s.scan, &s.flows) else {
            continue;
        };
        let bad_entities: Vec<&str> = bad
            .iter()
            .flat_map(|addr| owners.get(addr.as_str()).cloned().unwrap_or_default())
            .collect();
        for (e, slot) in out.iter_mut() {
            if slot.is_none() && !bad_entities.contains(&e.as_str()) {
                *slot = Some(CrossCheck {
                    basis: CROSS_CHECK_BASES[2].into(),
                    source: format!(
                        "a re-scan at block {} ({}) reconciles at every address",
                        s.scan.height, s.scan.method
                    ),
                });
            }
        }
    }
    Ok(out)
}

/// One membership epoch of a dated set: the members between two changes, as
/// the plain window a shard of it runs (ADR-0037).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Epoch {
    /// `LabelSetRef::hash` of the window: what a shard's binding must name.
    pub set_hash: Hash32,
    /// Its entities, ascending: a shard's leaf order (one row per entity).
    pub entities: Vec<String>,
    /// Address to entity.
    pub owner: BTreeMap<String, String>,
}

impl Epoch {
    /// `entity`'s leaf index in a shard of this window, if it has an address in it.
    pub fn index_of(&self, entity: &str) -> Option<u64> {
        self.entities
            .iter()
            .position(|e| e == entity)
            .map(|i| i as u64)
    }
}

/// The epochs of a dated set, built on first use (an undated set read with
/// [`DatedLabelSet::from_undated`] has one). Shared by the serving check, the
/// client check and the client's list check (ROADMAP D22).
pub struct Epochs<'d> {
    set: &'d DatedLabelSet,
    boundaries: Vec<u64>,
    built: BTreeMap<usize, Epoch>,
}

impl<'d> Epochs<'d> {
    /// The epochs of `set`, none built yet.
    pub fn new(set: &'d DatedLabelSet) -> Self {
        Epochs {
            set,
            boundaries: set.boundaries(),
            built: BTreeMap::new(),
        }
    }

    /// The epoch of blocks `from..=to`, or why the range is not one window.
    pub fn of(&mut self, from: u64, to: u64) -> Result<usize, String> {
        let why = |e: onyx_labels::dated::DatedError| {
            format!("{e}: a shard must start at every membership change (ADR-0037)")
        };
        if from > to {
            return Err(why(onyx_labels::dated::DatedError::EmptyWindow(from, to)));
        }
        if let Some(at) = self.boundaries.iter().find(|b| **b > from && **b <= to) {
            return Err(why(onyx_labels::dated::DatedError::ChangesInside {
                from,
                to,
                at: *at,
            }));
        }
        let idx = self.boundaries.partition_point(|b| *b <= from);
        if let std::collections::btree_map::Entry::Vacant(slot) = self.built.entry(idx) {
            let w = self.set.window(from, to).map_err(why)?;
            let mut entities: Vec<String> = w.labels.iter().map(|l| l.entity.clone()).collect();
            entities.sort_unstable();
            entities.dedup();
            let set_hash = LabelSetRef {
                name: w.name.clone(),
                version: w.version,
                content_hash: w.content_hash(),
            }
            .hash();
            let owner = w
                .labels
                .into_iter()
                .map(|l| (l.address, l.entity))
                .collect();
            slot.insert(Epoch {
                set_hash,
                entities,
                owner,
            });
        }
        Ok(idx)
    }

    /// An epoch [`Self::of`] returned.
    pub fn get(&self, idx: usize) -> Option<&Epoch> {
        self.built.get(&idx)
    }

    /// The window of blocks `from..=to`: [`Self::of`] then [`Self::get`].
    pub fn window(&mut self, from: u64, to: u64) -> Result<&Epoch, String> {
        let i = self.of(from, to)?;
        self.built.get(&i).ok_or_else(|| "epoch".to_string())
    }
}

/// Per-address balances from the scan and a flows file (ADR-0037): at a
/// height before the scan, the scan minus the flows after it; after the scan,
/// the scan plus the flows up to it.
struct Balances<'a> {
    scan_height: u64,
    held: BTreeMap<&'a str, u64>,
    blocks: &'a [FlowBlock],
}

impl<'a> Balances<'a> {
    /// Every flow at an address of the set; every address's balance never
    /// negative after any block of the file (as R-RES-NEG-3); the blocks
    /// contiguous through the scan's height or right after it.
    fn new(
        scan: &'a ScanSnapshot,
        set: &DatedLabelSet,
        flows: Option<&'a FlowsFile>,
    ) -> Result<Self, ReserveError> {
        let blocks: &[FlowBlock] = flows.map_or(&[], |f| f.blocks.as_slice());
        let b = Balances {
            scan_height: scan.height,
            held: scan
                .rows
                .iter()
                .map(|r| (r.address.as_str(), r.sats))
                .collect(),
            blocks,
        };
        if blocks.is_empty() {
            return Ok(b);
        }
        let first = blocks[0].height;
        let last = blocks[blocks.len() - 1].height;
        let m = |x: String| Err(ReserveError::Membership(x));
        if first > scan.height.saturating_add(1) || last < scan.height {
            return m(format!(
                "the flows file ({first}-{last}) must reach the scan's block {} or start right after it",
                scan.height
            ));
        }
        let known: std::collections::BTreeSet<&str> = set.addresses().into_iter().collect();
        let mut bal: BTreeMap<&str, i128> =
            b.held.iter().map(|(a, v)| (*a, i128::from(*v))).collect();
        // Back from the scan: the balance after block h-1 is the one after h
        // minus h's net flow.
        for blk in blocks.iter().rev().filter(|x| x.height <= scan.height) {
            for (a, (rx, tx)) in &blk.flows {
                if !known.contains(a.as_str()) {
                    return m(format!("{a}: a flow at an address outside the set"));
                }
                let v = bal.entry(a.as_str()).or_default();
                *v = *v - *rx as i128 + *tx as i128;
                if *v < 0 {
                    return Err(ReserveError::Negative(format!(
                        "{a} after block {}",
                        blk.height.saturating_sub(1)
                    )));
                }
            }
        }
        let mut bal: BTreeMap<&str, i128> =
            b.held.iter().map(|(a, v)| (*a, i128::from(*v))).collect();
        for blk in blocks.iter().filter(|x| x.height > scan.height) {
            for (a, (rx, tx)) in &blk.flows {
                if !known.contains(a.as_str()) {
                    return m(format!("{a}: a flow at an address outside the set"));
                }
                let v = bal.entry(a.as_str()).or_default();
                *v = *v + *rx as i128 - *tx as i128;
                if *v < 0 {
                    return Err(ReserveError::Negative(format!(
                        "{a} after block {}",
                        blk.height
                    )));
                }
            }
        }
        Ok(b)
    }

    /// True if the file holds every block of `from..=to`.
    fn covers(&self, from: u64, to: u64) -> bool {
        match (self.blocks.first(), self.blocks.last()) {
            (Some(f), Some(l)) => f.height <= from && to <= l.height,
            _ => false,
        }
    }

    /// `address`'s balance after block `h`.
    fn at(&self, address: &str, h: u64) -> Result<u64, ReserveError> {
        let mut v = i128::from(self.held.get(address).copied().unwrap_or(0));
        let s = self.scan_height;
        let (lo, hi) = if h < s {
            (h.saturating_add(1), s)
        } else {
            (s.saturating_add(1), h)
        };
        if h != s && !self.covers(lo, hi) {
            return Err(ReserveError::Membership(format!(
                "no flows file covers blocks {lo}-{hi}, needed for {address}'s balance after block {h}"
            )));
        }
        for blk in self
            .blocks
            .iter()
            .filter(|b| b.height >= lo && b.height <= hi)
        {
            if let Some((rx, tx)) = blk.flows.get(address) {
                let net = *rx as i128 - *tx as i128;
                v += if h < s { -net } else { net };
            }
        }
        u64::try_from(v).map_err(|_| ReserveError::Negative(format!("{address} after block {h}")))
    }

    /// Per entity of `owner`, `(received, sent)` over `from..=to`.
    fn entity_flows(
        &self,
        owner: &BTreeMap<String, String>,
        from: u64,
        to: u64,
    ) -> BTreeMap<String, (u128, u128)> {
        let mut out: BTreeMap<String, (u128, u128)> = BTreeMap::new();
        for blk in self
            .blocks
            .iter()
            .filter(|b| b.height >= from && b.height <= to)
        {
            for (a, (rx, tx)) in &blk.flows {
                if let Some(e) = owner.get(a) {
                    let x = out.entry(e.clone()).or_default();
                    x.0 += rx;
                    x.1 += tx;
                }
            }
        }
        out
    }
}

/// What a membership change between two epochs moves, per entity: the
/// balances, after block `h`, of the addresses that join (+) or leave (-)
/// each entity (an address that moves between entities does both).
fn change_at(
    prev: &Epoch,
    next: &Epoch,
    h: u64,
    bal: &Balances<'_>,
) -> Result<BTreeMap<String, i128>, ReserveError> {
    let mut out: BTreeMap<String, i128> = BTreeMap::new();
    let mut all: Vec<&String> = prev.owner.keys().chain(next.owner.keys()).collect();
    all.sort_unstable();
    all.dedup();
    for a in all {
        let (p, n) = (prev.owner.get(a), next.owner.get(a));
        if p == n {
            continue;
        }
        let b = i128::from(bal.at(a, h)?);
        if let Some(e) = p {
            *out.entry(e.clone()).or_default() -= b;
        }
        if let Some(e) = n {
            *out.entry(e.clone()).or_default() += b;
        }
    }
    Ok(out)
}

/// The next shard's base: the previous end plus the change, on the next
/// epoch's entities. An entity that leaves must leave nothing behind.
fn rebase(
    state: &[EntityBase],
    change: &BTreeMap<String, i128>,
    entities: &[String],
) -> Result<Vec<EntityBase>, String> {
    let mut v: BTreeMap<&str, i128> = state
        .iter()
        .map(|b| (b.entity.as_str(), i128::from(b.balance_sats)))
        .collect();
    for (e, d) in change {
        *v.entry(e.as_str()).or_default() += d;
    }
    for (e, x) in &v {
        if *x != 0 && !entities.iter().any(|n| n == e) {
            return Err(format!(
                "{e} keeps {x} sats after its last address left: its rows disagree with its addresses' balances"
            ));
        }
    }
    entities
        .iter()
        .map(|e| {
            let x = v.get(e.as_str()).copied().unwrap_or(0);
            Ok(EntityBase {
                entity: e.clone(),
                balance_sats: u64::try_from(x)
                    .map_err(|_| format!("{e}: negative base after the membership change"))?,
            })
        })
        .collect()
}

/// A shard's params and rows, decoded and checked against the anchor.
struct Decoded {
    params: ReserveParams,
    rows: Vec<EntityReserveRow>,
    epoch: usize,
}

fn decode_shard(
    a: &ServingAnchor<'_>,
    epochs: &mut Epochs<'_>,
    s: &FinalizedShard,
) -> Result<Decoded, String> {
    use borsh::BorshDeserialize;
    if s.recipe != a.recipe {
        return Err("another recipe than the pinned btc-entity-reserve build".into());
    }
    if s.to < s.from || s.from <= a.start_height {
        return Err(format!(
            "range {}-{} is not after the served chain's start {}",
            s.from, s.to, a.start_height
        ));
    }
    // RES-2 (ADR-0037): the shard runs exactly the members valid at every
    // height of its range, all of them frozen by the set's publication
    // (review D1-R1 F3).
    if s.to > epochs.set.published {
        return Err(format!(
            "blocks {}-{} reach past the label set's publication at {}: a later version could still change them",
            s.from, s.to, epochs.set.published
        ));
    }
    let epoch = epochs.of(s.from, s.to)?;
    let ep = epochs.get(epoch).ok_or("epoch")?;
    if s.label_set_hash != ep.set_hash {
        return Err("another label set than the committed scan's (for these blocks)".into());
    }
    if s.schema != keccak256(onyx_reserve_schema::SCHEMA_ENTITY_RESERVE) {
        return Err("not a reserve row schema".into());
    }
    // RES-4: the range ends on the header chain's block, deep enough. Review
    // RESAPI-R1 F2: the landing must read the scan's block, not another
    // block at the same height.
    let tip = hex::encode(s.tip.0);
    if s.to == a.scan.height && !tip.eq_ignore_ascii_case(&a.scan.block_hash) {
        return Err(format!(
            "blocks {}-{} end on another block than the committed scan's ({})",
            s.from, s.to, a.scan.block_hash
        ));
    }
    match a.headers.require_confirmed(s.to, &tip, MIN_CONFIRMATIONS) {
        Ok(()) => {}
        Err(ReserveError::StaleBranch(h)) => {
            return Err(format!(
                "blocks {}-{} end on block {tip}, not the header chain's block at {h}: a stale branch (RES-4)",
                s.from, s.to
            ))
        }
        Err(e) => return Err(format!("blocks {}-{}: {e} (RES-4)", s.from, s.to)),
    }
    let params = s
        .params
        .as_deref()
        .ok_or("the params are not in the store")
        .and_then(|p| ReserveParams::try_from_slice(p).map_err(|_| "params do not decode"))?;
    let rows = s
        .leaves
        .iter()
        .map(|l| {
            let o = onyx_recipe_sdk::OutputRow::try_from_slice(l).map_err(|_| "leaf")?;
            let r = EntityReserveRow::try_from_slice(&o.value).map_err(|_| "row")?;
            if o.key != r.entity.as_bytes() {
                return Err("row key is not its entity");
            }
            Ok(r)
        })
        .collect::<Result<Vec<_>, &str>>()
        .map_err(|e| format!("rows do not decode: {e}"))?;
    if !rows.iter().map(|r| &r.entity).eq(ep.entities.iter()) {
        return Err("not one row per entity of the label set".into());
    }
    if rows
        .iter()
        .any(|r| r.base_height + 1 != s.from || r.height != s.to)
    {
        return Err("rows name another range than the source snapshot".into());
    }
    Ok(Decoded {
        params,
        rows,
        epoch,
    })
}

/// The serving check of `btc.entity-reserve` (THREATS RES-7, ADR-0034
/// Amendment 1, ADR-0037). Non-circular: the expected params of every shard
/// are derived from the committed scan and the FINALIZED rows (and, at a
/// membership change, the changing addresses' balances from the scan and a
/// flows file), never from a spec under test or a base file.
///
/// 1. The scan must be a scan of its label set (RES-2), and its block on the
///    header chain with [`MIN_CONFIRMATIONS`] (RES-4); a flows file too.
/// 2. Each shard runs the window of the set for its blocks (a dated set: a
///    shard across a membership change is refused) and ends on the header
///    chain's block with [`MIN_CONFIRMATIONS`] (a stale branch is refused,
///    before the landing too).
/// 3. From the pinned start height, finalized ranges must tile the blocks up
///    to the scan's height without gap or overlap (ranges ending after the
///    scan may follow); the chain must reach the scan, or nothing is served.
/// 4. The start base is the scan's entity totals over the members at the
///    scan's height, minus every finalized range's net flow (received minus
///    sent, which do not depend on the base) up to the scan, minus what each
///    membership change moved; all shards of one range must agree on their
///    flows, and a flows file must agree with them. A negative base means
///    scan and flows disagree: nothing served.
/// 5. Walking forward, a shard is served only if its params are exactly the
///    expected ones (start: that derived base, base reference = the scan's
///    hash; later: the previous served shard's end rows plus the membership
///    change, last height and root) and its rows chain
///    ([`check_params_chain`] over each run of shards between changes). A
///    shard on any other base is refused even though it reran.
/// 6. The end at the scan's height equals the scan's totals (the landing),
///    and a shard ending at the scan's height must end on the scan's block.
///
/// Rows come back with their entity's cross-check (RES-1,
/// [`cross_checked_entities`]); the API refuses rows without one.
pub fn serving_check(
    a: &ServingAnchor<'_>,
    shards: &[FinalizedShard],
) -> Result<ServingVerdict, ReserveError> {
    a.labels.check_scan(a.scan)?;
    if a.start_height >= a.scan.height {
        return Err(ReserveError::Chain(
            "the start height is not before the scan".into(),
        ));
    }
    a.headers
        .require_confirmed(a.scan.height, &a.scan.block_hash, MIN_CONFIRMATIONS)?;
    if let Some(f) = a.labels.flows() {
        f.check_on_chain(a.headers, MIN_CONFIRMATIONS)?;
    }
    let d = a.labels.dated();
    let bal = Balances::new(a.scan, &d, a.labels.flows())?;
    let totals = a.scan.totals_at(&d, a.scan.height)?;
    let cross = cross_checked_entities(a)?;
    let mut epochs = Epochs::new(&d);
    let mut verdict: Vec<Result<(), String>> = Vec::with_capacity(shards.len());
    let mut decoded: Vec<Option<Decoded>> = Vec::with_capacity(shards.len());
    for s in shards {
        match decode_shard(a, &mut epochs, s) {
            Ok(d) => {
                verdict.push(Ok(()));
                decoded.push(Some(d));
            }
            Err(e) => {
                verdict.push(Err(e));
                decoded.push(None);
            }
        }
    }
    // Ranges of the decodable shards, and the chain of ranges from the start.
    let mut by_range: BTreeMap<(u64, u64), Vec<usize>> = BTreeMap::new();
    for (i, d) in decoded.iter().enumerate() {
        if d.is_some() {
            by_range
                .entry((shards[i].from, shards[i].to))
                .or_default()
                .push(i);
        }
    }
    let mut chain: Vec<(u64, u64)> = Vec::new();
    let mut cur = a.start_height;
    loop {
        let next: Vec<(u64, u64)> = by_range
            .keys()
            .filter(|(f, _)| *f == cur + 1)
            .copied()
            .collect();
        match next.as_slice() {
            [one] => {
                chain.push(*one);
                cur = one.1;
            }
            _ => break,
        }
    }
    let refuse_all = |verdict: &mut Vec<Result<(), String>>, why: &str| {
        for v in verdict.iter_mut().filter(|v| v.is_ok()) {
            *v = Err(why.to_string());
        }
    };
    let finish = |verdict: Vec<Result<(), String>>,
                  decoded: &[Option<Decoded>],
                  start_base: Option<Vec<EntityBase>>| {
        let out = shards
            .iter()
            .zip(verdict)
            .zip(decoded)
            .map(|((s, v), d)| {
                let rows = v.and_then(|()| {
                    let d = d.as_ref().ok_or("not decoded")?;
                    Ok(d.rows
                        .iter()
                        .enumerate()
                        .map(|(i, r)| ServedRow {
                            index: i as u64,
                            entity: r.entity.clone(),
                            cross_check: cross.get(&r.entity).cloned().flatten(),
                        })
                        .collect())
                });
                (s.round, s.shard, rows)
            })
            .collect();
        ServingVerdict {
            start_base,
            cross_checks: cross.clone(),
            shards: out,
        }
    };
    // Shards whose range is not on the chain are refused whatever happens.
    let on_chain: std::collections::BTreeSet<(u64, u64)> = chain.iter().copied().collect();
    for (r, idx) in &by_range {
        if !on_chain.contains(r) {
            for &i in idx {
                verdict[i] = Err(format!(
                    "range {}-{} is not on the chain of finalized ranges from block {} (a gap, an overlap or two ranges at one start)",
                    r.0, r.1, a.start_height
                ));
            }
        }
    }
    let Some(landing) = chain.iter().position(|r| r.1 == a.scan.height) else {
        refuse_all(
            &mut verdict,
            &format!(
                "the finalized chain from block {} does not reach the committed scan at block {}",
                a.start_height, a.scan.height
            ),
        );
        return Ok(finish(verdict, &decoded, None));
    };
    let epoch_of = |r: &(u64, u64)| decoded[by_range[r][0]].as_ref().map(|d| d.epoch);
    // Flows per range: all finalized shards of one range must agree, and a
    // flows file holding the range must agree with them (ADR-0037).
    let mut net: BTreeMap<String, i128> = BTreeMap::new();
    // Review D1-R1 F7: a disagreement after the scan stops the chain there,
    // so no later range is served on flows the file check never saw.
    let mut stop_at: Option<(usize, String)> = None;
    for (k, r) in chain.iter().enumerate() {
        let idx = &by_range[r];
        let flows = |i: usize| -> Vec<(String, u128, u128, u64)> {
            decoded[i].as_ref().map_or_else(Vec::new, |d| {
                d.rows
                    .iter()
                    .map(|x| (x.entity.clone(), x.received_sats, x.sent_sats, x.tx_count))
                    .collect()
            })
        };
        let f0 = flows(idx[0]);
        if idx.iter().any(|&i| flows(i) != f0) {
            if k > landing {
                stop_at = Some((
                    k,
                    format!(
                        "finalized shards of blocks {}-{} disagree on the flows: the chain stops there",
                        r.0, r.1
                    ),
                ));
                break;
            }
            refuse_all(
                &mut verdict,
                &format!(
                    "finalized shards of blocks {}-{} disagree on the flows",
                    r.0, r.1
                ),
            );
            return Ok(finish(verdict, &decoded, None));
        }
        if bal.covers(r.0, r.1) {
            let owner = epoch_of(r)
                .and_then(|e| epochs.get(e))
                .map(|e| &e.owner)
                .ok_or(ReserveError::Chain("epoch".into()))?;
            let file = bal.entity_flows(owner, r.0, r.1);
            let disagree = f0
                .iter()
                .any(|(e, rx, tx, _)| file.get(e).copied().unwrap_or((0, 0)) != (*rx, *tx));
            if disagree {
                refuse_all(
                    &mut verdict,
                    &format!(
                        "the flows file disagrees with the finalized flows of blocks {}-{}",
                        r.0, r.1
                    ),
                );
                return Ok(finish(verdict, &decoded, None));
            }
        }
        if k <= landing {
            for (e, rx, tx, _) in f0 {
                *net.entry(e).or_default() += rx as i128 - tx as i128;
            }
        }
    }
    // What each membership change up to the scan moved (RES-2, ADR-0037).
    let change = |k: usize| -> Result<BTreeMap<String, i128>, String> {
        let (p, n) = (epoch_of(&chain[k - 1]), epoch_of(&chain[k]));
        if p == n {
            return Ok(BTreeMap::new());
        }
        let (Some(p), Some(n)) = (p.and_then(|e| epochs.get(e)), n.and_then(|e| epochs.get(e)))
        else {
            return Err("epoch".into());
        };
        change_at(p, n, chain[k - 1].1, &bal).map_err(|e| {
            format!(
                "the membership change at block {} cannot be applied: {e}",
                chain[k].0
            )
        })
    };
    let mut moved: BTreeMap<String, i128> = BTreeMap::new();
    // Each change up to the scan, computed once (review D1-R1 F8).
    let mut changes: BTreeMap<usize, BTreeMap<String, i128>> = BTreeMap::new();
    for k in 1..=landing {
        match change(k) {
            Ok(c) => {
                for (e, x) in &c {
                    *moved.entry(e.clone()).or_default() += x;
                }
                changes.insert(k, c);
            }
            Err(why) => {
                refuse_all(&mut verdict, &why);
                return Ok(finish(verdict, &decoded, None));
            }
        }
    }
    let first_entities = epoch_of(&chain[0])
        .and_then(|e| epochs.get(e))
        .map(|e| e.entities.clone())
        .ok_or(ReserveError::Chain("epoch".into()))?;
    let start_base = if moved.is_empty() {
        derive_base(&totals, &net)
    } else {
        let mut net = net.clone();
        for (e, x) in &moved {
            *net.entry(e.clone()).or_default() += x;
        }
        let all: Vec<EntityBase> = net
            .keys()
            .filter(|e| !totals.iter().any(|t| &t.entity == *e))
            .map(|e| EntityBase {
                entity: e.clone(),
                balance_sats: 0,
            })
            .chain(totals.iter().cloned())
            .collect();
        derive_base(&all, &net).and_then(|b| {
            let mut out = Vec::new();
            for x in b {
                if first_entities.contains(&x.entity) {
                    out.push(x);
                } else if x.balance_sats != 0 {
                    return Err(ReserveError::Membership(format!(
                        "{} has {} sats at the start but no address there",
                        x.entity, x.balance_sats
                    )));
                }
            }
            out.sort_by(|x, y| x.entity.cmp(&y.entity));
            Ok(out)
        })
    };
    let start_base = match start_base {
        Ok(b) if b.iter().map(|x| &x.entity).eq(first_entities.iter()) => b,
        Ok(_) => {
            refuse_all(
                &mut verdict,
                "the scan minus the finalized flows is not a base: entities differ from the first shard's",
            );
            return Ok(finish(verdict, &decoded, None));
        }
        Err(e) => {
            refuse_all(
                &mut verdict,
                &format!("the scan minus the finalized flows is not a base: {e}"),
            );
            return Ok(finish(verdict, &decoded, None));
        }
    };
    // Forward: pick, in each range, the shards whose params are the expected ones.
    let (mut height, mut state, mut link) = (a.start_height, start_base.clone(), a.scan.hash());
    // Runs of picked shards between membership changes: (base height, base, link, picked).
    let mut runs: Vec<(u64, Vec<EntityBase>, Hash32, Vec<usize>)> = Vec::new();
    let mut broken: Option<String> = None;
    for (k, r) in chain.iter().enumerate() {
        let idx = &by_range[r];
        if let Some((at, why)) = &stop_at {
            if k >= *at && broken.is_none() {
                broken = Some(why.clone());
            }
        }
        if let Some(why) = &broken {
            for &i in idx {
                verdict[i] = Err(why.clone());
            }
            continue;
        }
        let entities = epoch_of(r)
            .and_then(|e| epochs.get(e))
            .map(|e| e.entities.clone())
            .ok_or(ReserveError::Chain("epoch".into()))?;
        let changed = k > 0 && epoch_of(r) != epoch_of(&chain[k - 1]);
        if changed {
            let c = match changes.get(&k) {
                Some(c) => Ok(c.clone()),
                None => change(k),
            };
            match c.and_then(|c| rebase(&state, &c, &entities)) {
                Ok(b) => state = b,
                Err(why) => {
                    for &i in idx {
                        verdict[i] = Err(why.clone());
                    }
                    if k <= landing {
                        refuse_all(&mut verdict, &format!("{why}, before the committed scan"));
                        return Ok(finish(verdict, &decoded, None));
                    }
                    broken = Some(why);
                    continue;
                }
            }
        }
        if k == 0 || changed {
            runs.push((height, state.clone(), link, Vec::new()));
        }
        let want = ReserveParams {
            base_height: height,
            base_ref: link.0,
            base: state.clone(),
        };
        let mut good: Vec<usize> = Vec::new();
        for &i in idx {
            let d = decoded[i]
                .as_ref()
                .ok_or(ReserveError::Chain("decode".into()))?;
            let one = ReserveShard {
                from: r.0,
                to: r.1,
                root: shards[i].root,
                rows: d.rows.clone(),
            };
            if d.params == want && check_chain(height, &state, &link, &[one]).is_ok() {
                good.push(i);
            } else {
                verdict[i] = Err(format!(
                    "params of blocks {}-{} are not the previous finalized end (derived from the committed scan and the finalized flows)",
                    r.0, r.1
                ));
            }
        }
        let roots: std::collections::BTreeSet<Hash32> =
            good.iter().map(|&i| shards[i].root).collect();
        if roots.len() != 1 {
            let why = format!(
                "no single served shard for blocks {}-{}: the chain stops there",
                r.0, r.1
            );
            for &i in &good {
                verdict[i] = Err(why.clone());
            }
            if k <= landing {
                refuse_all(&mut verdict, &format!("{why}, before the committed scan"));
                return Ok(finish(verdict, &decoded, None));
            }
            broken = Some(why);
            continue;
        }
        let first = good[0];
        if let Some(run) = runs.last_mut() {
            run.3.push(first);
        }
        let d = decoded[first]
            .as_ref()
            .ok_or(ReserveError::Chain("decode".into()))?;
        state = d
            .rows
            .iter()
            .map(|x| EntityBase {
                entity: x.entity.clone(),
                balance_sats: x.balance_sats,
            })
            .collect();
        height = r.1;
        link = shards[first].root;
        if k == landing && state != totals {
            refuse_all(
                &mut verdict,
                "the chain does not land on the committed scan",
            );
            return Ok(finish(verdict, &decoded, None));
        }
    }
    // The existing params-chain check over each run of selected shards, from
    // its derived start (R-RES-PARAMS-1): it must agree with the walk above.
    for (base_height, base, link, picked) in &runs {
        let params: Vec<ReserveParams> = picked
            .iter()
            .filter_map(|&i| decoded[i].as_ref().map(|d| d.params.clone()))
            .collect();
        let chosen: Vec<ReserveShard> = picked
            .iter()
            .filter_map(|&i| {
                decoded[i].as_ref().map(|d| ReserveShard {
                    from: shards[i].from,
                    to: shards[i].to,
                    root: shards[i].root,
                    rows: d.rows.clone(),
                })
            })
            .collect();
        if let Err(e) = check_params_chain(*base_height, base, link, &params, &chosen) {
            refuse_all(&mut verdict, &format!("params chain: {e}"));
            return Ok(finish(verdict, &decoded, None));
        }
    }
    Ok(finish(verdict, &decoded, Some(start_base)))
}

// ------------------------------------------------------------ client check

/// What a client trusts before it trusts a reserve row (ROADMAP D3 and D22,
/// THREATS RES-10, ADR-0034 Amendments 2 and 3): the committed scan (its
/// hash, height, block, rows and per-entity totals over the members at its
/// height), the dated label set the shards' windows come from (an undated
/// set read with [`DatedLabelSet::from_undated`]: one window), the flows file
/// a membership change needs, and the reserve build. Never taken from the
/// server: a client builds it from files it holds ([`ReservePin::from_scan`],
/// [`ReservePin::from_dated_scan`]) and compares the scan's hash (and the
/// flows file's) with hashes it trusts. A chain built on any other scan,
/// label set, flows or recipe is refused, however well its roots verify.
/// The scan need not be the one the chain started on: any later scan the
/// client trusts anchors the links between it and an answer (D22).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReservePin {
    /// [`ScanSnapshot::hash`].
    pub scan_hash: Hash32,
    /// Height the scan read: the link ending there is the landing.
    pub scan_height: u64,
    /// The scan's block, display hex: the landing's tip hash.
    pub scan_block: String,
    /// The `btc-entity-reserve` build every link must run.
    pub recipe: Hash32,
    /// The scan's total per entity over the members at its height
    /// ([`ScanSnapshot::totals_at`]), in entity order (the landing shard's
    /// leaf order).
    pub totals: Vec<EntityBase>,
    /// The scan itself: per-address balances for membership changes.
    pub scan: ScanSnapshot,
    /// The label set the scan names, dated (ADR-0037).
    pub labels: DatedLabelSet,
    /// The flows file membership changes are applied with (ADR-0037 item 5;
    /// `None`: no change away from the scan's own height can be applied).
    pub flows: Option<FlowsFile>,
}

impl ReservePin {
    /// The pin of `scan` over the undated `set`, recomputed from the scan's
    /// rows (its hash and its entity totals), after
    /// [`ScanSnapshot::check_labels`]: a scan of another list, version or
    /// content hash is refused here, and so is a scan whose hash is not
    /// `trusted` (the hash the client trusts, e.g. the website's
    /// `RESERVES_SNAPSHOT`; review D3-R1-4: a scan file taken from the
    /// operator would anchor the operator's own chain). `recipe` is the
    /// client's own build of `btc-entity-reserve` (`recipes/out/`).
    pub fn from_scan(
        scan: &ScanSnapshot,
        set: &LabelSet,
        recipe: Hash32,
        trusted: &Hash32,
    ) -> Result<Self, ReserveError> {
        scan.check_labels(set)?;
        Self::pinned(
            scan,
            DatedLabelSet::from_undated(set),
            None,
            recipe,
            trusted,
        )
    }

    /// The pin of `scan` over the dated `set` (ROADMAP D22, ADR-0037):
    /// [`ScanSnapshot::check_dated`]; `history` is every earlier version of
    /// the set, oldest first, and the whole history must pass
    /// `onyx_labels::dated::check_history` (no version back-dates a
    /// membership; a version above 1 without its history is refused, as the
    /// reserve gate refuses it); the scan's hash must be `trusted`; a flows
    /// file must hash to `trusted_flows` ([`FlowsFile::hash`]), hold only
    /// addresses of the set and take none below zero, and reach the scan's
    /// block or start right after it. Totals are the scan's over the members
    /// at its height ([`ScanSnapshot::totals_at`]).
    pub fn from_dated_scan(
        scan: &ScanSnapshot,
        set: &DatedLabelSet,
        history: &[DatedLabelSet],
        flows: Option<FlowsFile>,
        recipe: Hash32,
        trusted: &Hash32,
        trusted_flows: Option<&Hash32>,
    ) -> Result<Self, ReserveError> {
        let mut all = history.to_vec();
        all.push(set.clone());
        onyx_labels::dated::check_history(&all)
            .map_err(|e| ReserveError::Membership(format!("dated label set history: {e}")))?;
        scan.check_dated(set)?;
        match (&flows, trusted_flows) {
            (None, _) => {}
            (Some(f), Some(t)) if f.hash() == *t => {}
            (Some(_), _) => {
                return Err(ReserveError::Snapshot(
                    "the flows file's hash is not the one the client trusts".into(),
                ))
            }
        }
        if let Some(f) = &flows {
            f.check_consistent()?;
        }
        Balances::new(scan, set, flows.as_ref())?;
        Self::pinned(scan, set.clone(), flows, recipe, trusted)
    }

    fn pinned(
        scan: &ScanSnapshot,
        labels: DatedLabelSet,
        flows: Option<FlowsFile>,
        recipe: Hash32,
        trusted: &Hash32,
    ) -> Result<Self, ReserveError> {
        if scan.hash() != *trusted {
            return Err(ReserveError::Snapshot(
                "the scan's hash is not the one the client trusts".into(),
            ));
        }
        Ok(ReservePin {
            scan_hash: scan.hash(),
            scan_height: scan.height,
            scan_block: scan.block_hash.clone(),
            recipe,
            totals: scan.totals_at(&labels, scan.height)?,
            scan: scan.clone(),
            labels,
            flows,
        })
    }

    /// The pin against the client's own header chain (RES-4, ROADMAP D22):
    /// the scan's block is the chain's block at its height with
    /// [`MIN_CONFIRMATIONS`] (a stale pin, a scan of a block that a reorg
    /// replaced or one not yet deep enough, is refused), and so is every
    /// block of the flows file.
    pub fn check_on(&self, headers: &HeaderChain) -> Result<(), ReserveError> {
        headers.require_confirmed(self.scan_height, &self.scan_block, MIN_CONFIRMATIONS)?;
        if let Some(f) = &self.flows {
            f.check_on_chain(headers, MIN_CONFIRMATIONS)?;
        }
        Ok(())
    }

    /// The leaf index of `entity` in the landing's shard (the members at the
    /// scan's height), if it has an address there. Other shards: their
    /// window's [`Epoch::index_of`] ([`Self::windows`]).
    pub fn index_of(&self, entity: &str) -> Option<u64> {
        self.totals
            .iter()
            .position(|t| t.entity == entity)
            .map(|i| i as u64)
    }

    /// The scan's total for `entity`.
    pub fn total(&self, entity: &str) -> Option<u64> {
        self.totals
            .iter()
            .find(|t| t.entity == entity)
            .map(|t| t.balance_sats)
    }

    /// True if the pinned set names `entity` at any height.
    pub fn names(&self, entity: &str) -> bool {
        self.labels.members.iter().any(|m| m.label.entity == entity)
    }

    /// The pinned set's windows, for a link's window hash and leaf index.
    pub fn windows(&self) -> Epochs<'_> {
        Epochs::new(&self.labels)
    }

    /// What [`derive_entity_span`] anchors on.
    pub fn anchor(&self) -> SpanAnchor<'_> {
        SpanAnchor {
            scan_height: self.scan_height,
            totals: &self.totals,
            dated: Some(DatedAnchor {
                set: &self.labels,
                scan: &self.scan,
                flows: self.flows.as_ref(),
            }),
        }
    }
}

/// One link of a reserve chain as a client proved it: a shard whose root the
/// client read as final on its own node, the binding facts it recomputed
/// from the answer's inputs (recipe, label set, schema, source snapshot), and
/// the one leaf it checked under that root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReserveLink {
    /// Round.
    pub round: u64,
    /// Shard id.
    pub shard: Hash32,
    /// Finalized root (the leaf is proven under it).
    pub root: Hash32,
    /// Recipe id of the binding.
    pub recipe: Hash32,
    /// `LabelSetRef::hash` of the binding.
    pub label_set_hash: Hash32,
    /// Row schema id of the binding.
    pub schema: Hash32,
    /// First height of the source snapshot.
    pub from: u64,
    /// Last height of the source snapshot.
    pub to: u64,
    /// Tip hash of the source snapshot (display byte order, as
    /// `BlockRange::tip_hash` holds it).
    pub tip: Hash32,
    /// Leaf index of the proven row.
    pub index: u64,
    /// The proven leaf (Borsh `OutputRow`).
    pub leaf: Vec<u8>,
}

/// One entity's row in one shard of a chain, for [`derive_entity_span`].
#[derive(Clone, Copy, Debug)]
pub struct EntityLink<'a> {
    /// First height of the shard.
    pub from: u64,
    /// Last height of the shard.
    pub to: u64,
    /// The shard's root (the next link's `base_ref`).
    pub root: Hash32,
    /// The entity's row in it.
    pub row: &'a EntityReserveRow,
}

/// What a span of one entity's links is anchored on (ROADMAP D22).
#[derive(Clone, Copy, Debug)]
pub struct SpanAnchor<'a> {
    /// Height of the trusted scan: the link ending there is the landing.
    pub scan_height: u64,
    /// The scan's totals over the members at its height.
    pub totals: &'a [EntityBase],
    /// The dated set, scan rows and flows file (membership changes and the
    /// entry and exit anchors). `None`: an undated set whose labels the
    /// caller does not hold (the browser's week export): one window, every
    /// entity in every link, the landing the only anchor.
    pub dated: Option<DatedAnchor<'a>>,
}

/// The dated part of a [`SpanAnchor`].
#[derive(Clone, Copy, Debug)]
pub struct DatedAnchor<'a> {
    /// The dated set (an undated one via [`DatedLabelSet::from_undated`]).
    pub set: &'a DatedLabelSet,
    /// The trusted scan (per-address balances).
    pub scan: &'a ScanSnapshot,
    /// The trusted flows file, if any.
    pub flows: Option<&'a FlowsFile>,
}

/// Which known balance a span is anchored on (ROADMAP D22).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanEnd {
    /// The link at this index ends at the scan's height on the scan's
    /// block: its balance is the scan's total.
    Landing(usize),
    /// The span ends before the scan where the entity's last address
    /// leaves: the last link's balance is the leaving addresses' balances
    /// (scan rolled back with the flows file).
    Exit,
    /// The span starts after the scan where the entity's first address
    /// joins: the first link's base is the joining addresses' balances
    /// (scan rolled forward with the flows file).
    Entry,
}

/// What [`derive_entity_span`] derived for one entity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityChain {
    /// Base height of the first link.
    pub start_height: u64,
    /// The first link's base, derived from the anchor and the links' flows
    /// (never read from a row or a params blob).
    pub start_base_sats: u64,
    /// What the span is anchored on.
    pub anchor: SpanEnd,
}

/// The serving check for one entity's rows over a span of consecutive links
/// (THREATS RES-10, ROADMAP D3 and D22; the client side of
/// [`serving_check`]), shared by the API client ([`client_check`]) and the
/// browser (`apps/web/verify` `reserves_check`).
///
/// The links must be one entity's rows in consecutive shards, oldest first,
/// each naming its shard's range; with a dated set each runs one window
/// (none across a membership change, none past the set's publication) in
/// which the entity has an address. A row's flows are reruns (level 1);
/// only its base is a param, so one known balance fixes every base of the
/// span. The anchor, in this order: the link ending at the scan's height
/// (its balance is the scan's total); else, for a span before the scan whose
/// entity has no address right after it, the leaving addresses' balances at
/// its end; else, for a span after the scan whose entity has no address
/// right before it, the joining addresses' balances at its start (both from
/// the scan and the trusted flows file, as the serving check's rebase).
/// The first base is derived from the anchor and the flows; then, walking
/// forward, every row's copied params must be exactly the expected ones:
/// `base_sats` the derived base (first) or the previous end plus the
/// entity's membership-change delta (the balances after the previous link
/// of the addresses that join it minus those that leave it, ADR-0037 item
/// 5), `base_ref` the previous link's root, and the balance adds up. The
/// span need not start at the chain's start (the first `base_ref` is not
/// read): the anchor binds the values, so a client pays only for the links
/// between its pinned scan and the answer.
pub fn derive_entity_span<'a>(
    a: &SpanAnchor<'a>,
    entity: &str,
    links: &[EntityLink<'_>],
) -> Result<EntityChain, ReserveError> {
    let bad = |m: String| Err(ReserveError::Chain(m));
    let Some(first) = links.first() else {
        return bad("no links".into());
    };
    let last = links[links.len() - 1];
    // Shape first: one entity's rows over consecutive shards, each naming
    // its shard's range.
    let mut height = first.row.base_height;
    for l in links {
        let at = |m: &str| ReserveError::Chain(format!("{}-{}: {entity}: {m}", l.from, l.to));
        if l.row.entity != entity {
            return Err(at("another entity's row"));
        }
        if l.to < l.from || height.checked_add(1) != Some(l.from) {
            return Err(at("does not start after the previous link"));
        }
        if l.row.base_height != height || l.row.height != l.to {
            return Err(at("the row names another range than its shard"));
        }
        height = l.to;
    }
    // Flows as signed amounts, refused past the money supply (a row's u128
    // must not wrap into a negative i128 and cancel another).
    let flow = |r: &EntityReserveRow| -> Option<i128> {
        const CAP: u128 = 21_000_000 * 100_000_000;
        (r.received_sats <= CAP && r.sent_sats <= CAP)
            .then(|| r.received_sats as i128 - r.sent_sats as i128)
    };
    for l in links {
        if flow(l.row).is_none() {
            return bad(format!(
                "{}-{}: {entity}: flows above the money supply",
                l.from, l.to
            ));
        }
    }
    let flows: Vec<i128> = links
        .iter()
        .map(|l| flow(l.row).unwrap_or_default())
        .collect();
    // Windows (ADR-0037): each link one window holding the entity, and the
    // entity's delta at every membership change between two links.
    let mut delta = vec![0i128; links.len()];
    let mut changed = vec![false; links.len()];
    let mut epochs = a.dated.map(|d| Epochs::new(d.set));
    let mut balances: Option<Balances<'_>> = None;
    let balances_of = |d: &DatedAnchor<'a>| Balances::new(d.scan, d.set, d.flows);
    if let (Some(e), Some(d)) = (epochs.as_mut(), a.dated.as_ref()) {
        let mut idx = Vec::with_capacity(links.len());
        for l in links {
            let at = |m: &str| ReserveError::Chain(format!("{}-{}: {entity}: {m}", l.from, l.to));
            if l.to > d.set.published {
                return Err(at(&format!(
                    "reaches past the label set's publication at {}",
                    d.set.published
                )));
            }
            let i = e.of(l.from, l.to).map_err(|m| at(&m))?;
            if e.get(i).and_then(|w| w.index_of(entity)).is_none() {
                return Err(at("the entity has no address in these blocks"));
            }
            idx.push(i);
        }
        // Review D22-R2-1: the trusted flows file must agree with the
        // finalized flows (reruns) of every link it covers, as the serving
        // check requires: a file under a trusted hash that disagrees would
        // otherwise move a change's delta or an exit or entry anchor, and a
        // chain whose bases follow it would be accepted. What remains is the
        // file's split between one entity's addresses (RES-13).
        if d.flows.is_some() {
            if balances.is_none() {
                balances = Some(balances_of(d)?);
            }
            let b = balances
                .as_ref()
                .ok_or(ReserveError::Chain("balances".into()))?;
            for (k, l) in links.iter().enumerate() {
                if !b.covers(l.from, l.to) {
                    continue;
                }
                let w = e.get(idx[k]).ok_or(ReserveError::Chain("epoch".into()))?;
                let file = b
                    .entity_flows(&w.owner, l.from, l.to)
                    .get(entity)
                    .copied()
                    .unwrap_or((0, 0));
                if file != (l.row.received_sats, l.row.sent_sats) {
                    return Err(ReserveError::Chain(format!(
                        "{}-{}: {entity}: the trusted flows file disagrees with the finalized flows",
                        l.from, l.to
                    )));
                }
            }
        }
        for k in 1..links.len() {
            if idx[k] == idx[k - 1] {
                continue;
            }
            let (Some(p), Some(n)) = (e.get(idx[k - 1]), e.get(idx[k])) else {
                return bad("epoch".into());
            };
            if balances.is_none() {
                balances = Some(balances_of(d)?);
            }
            let b = balances
                .as_ref()
                .ok_or(ReserveError::Chain("balances".into()))?;
            let c = change_at(p, n, links[k - 1].to, b).map_err(|e| {
                ReserveError::Chain(format!(
                    "{entity}: the membership change at block {} cannot be applied: {e}",
                    links[k].from
                ))
            })?;
            delta[k] = c.get(entity).copied().unwrap_or(0);
            changed[k] = true;
        }
    }
    // An entity's addresses' summed balance after block `h` in the window of
    // blocks `from..=to` (scan and flows file).
    let mut held_at = |from: u64, to: u64, h: u64| -> Result<i128, ReserveError> {
        let (Some(e), Some(d)) = (epochs.as_mut(), a.dated.as_ref()) else {
            return Err(ReserveError::Chain("no dated set".into()));
        };
        if balances.is_none() {
            balances = Some(balances_of(d)?);
        }
        let b = balances
            .as_ref()
            .ok_or(ReserveError::Chain("balances".into()))?;
        let w = e
            .window(from, to)
            .map_err(|m| ReserveError::Chain(format!("{from}-{to}: {m}")))?;
        let mut v: i128 = 0;
        for (addr, ent) in &w.owner {
            if ent == entity {
                v += i128::from(b.at(addr, h).map_err(|x| {
                    ReserveError::Chain(format!("{entity}: anchor after block {h}: {x}"))
                })?);
            }
        }
        Ok(v)
    };
    let member_at = |h: u64| {
        a.dated
            .is_some_and(|d| d.set.members_at(h).any(|m| m.label.entity == entity))
    };
    let landing = links.iter().position(|l| l.to == a.scan_height);
    let net = |upto: usize| -> i128 {
        flows[..=upto].iter().sum::<i128>() + delta[1..=upto].iter().sum::<i128>()
    };
    // The balance the anchor fixes: at the landing's end, or the last
    // link's end (exit), or the first link's base (entry, derived directly).
    let (anchor, derived, want_end) = if let Some(j) = landing {
        let Some(total) = a.totals.iter().find(|t| t.entity == entity) else {
            return bad(format!(
                "{entity}: no address at the committed scan's height {}",
                a.scan_height
            ));
        };
        let t = i128::from(total.balance_sats);
        (SpanEnd::Landing(j), t - net(j), Some((j, t)))
    } else if a.dated.is_some() && last.to < a.scan_height && !member_at(last.to + 1) {
        let leave = held_at(last.from, last.to, last.to)?;
        let n = links.len() - 1;
        (SpanEnd::Exit, leave - net(n), Some((n, leave)))
    } else if a.dated.is_some()
        && first.from > a.scan_height
        && first.from > 0
        && !member_at(first.from - 1)
    {
        (
            SpanEnd::Entry,
            held_at(first.from, first.to, first.from - 1)?,
            None,
        )
    } else {
        return bad(format!(
            "no link ends at the committed scan's height {}, and the links neither end where {entity}'s last address leaves nor start where its first joins",
            a.scan_height
        ));
    };
    let Ok(start_base_sats) = u64::try_from(derived) else {
        return bad(format!(
            "{entity}: the scan's total minus the flows is not a base ({derived})"
        ));
    };
    let start_height = first.row.base_height;
    let mut state = i128::from(start_base_sats);
    for (k, l) in links.iter().enumerate() {
        let at = |m: &str| ReserveError::Chain(format!("{}-{}: {entity}: {m}", l.from, l.to));
        let r = l.row;
        if k > 0 && r.base_ref != links[k - 1].root.0 {
            return Err(at("base_ref is not the previous link's root"));
        }
        let want = state + delta[k];
        if i128::from(r.base_sats) != want {
            return Err(at(if k == 0 {
                "base is not the one derived from the committed scan and the finalized flows"
            } else if changed[k] {
                "base is not the previous link's end balance plus the membership change"
            } else {
                "base is not the previous link's end balance"
            }));
        }
        let end = i128::from(r.base_sats) + flows[k];
        if end != i128::from(r.balance_sats) {
            return Err(at("balance does not add up"));
        }
        state = end;
        if let Some((j, v)) = want_end {
            if j == k && state != v {
                return Err(at(if anchor == SpanEnd::Exit {
                    "the end balance is not its leaving addresses' balances"
                } else {
                    "the chain does not land on the committed scan's total"
                }));
            }
        }
    }
    Ok(EntityChain {
        start_height,
        start_base_sats,
        anchor,
    })
}

/// What a client's reserve check found (ROADMAP D3, D22).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientVerdict {
    /// The entity checked.
    pub entity: String,
    /// The derived start ([`derive_entity_span`]).
    pub chain: EntityChain,
    /// The entity's row in every link, in link order.
    pub rows: Vec<EntityReserveRow>,
}

/// The client's reserve serving check (ROADMAP D3 and D22, THREATS RES-10):
/// the operator's `servingCheck` is a claim; this check needs only the
/// client's [`ReservePin`], its own header chain, and links it proved
/// itself (each root read as final on its own node, each binding recomputed,
/// each leaf checked under its root; `onyx_reserve_serve::client` does that
/// from API answers). The pin's scan and flows file must be on `headers`
/// with [`MIN_CONFIRMATIONS`] ([`ReservePin::check_on`]). Every link must be
/// bound to the pinned recipe and reserve schema and to its window's label
/// set (the pinned set's window of its blocks, ADR-0037), reach no further
/// than the set's publication, end on the header chain's block at its
/// height with [`MIN_CONFIRMATIONS`] (a stale branch anywhere in the span is
/// refused), carry the entity's row at its window's leaf index, and the
/// link ending at the scan's height must end on the scan's block
/// (RESAPI-R1-2); then [`derive_entity_span`] derives the bases from the
/// anchor, the flows and the membership changes and refuses any row on
/// another base.
pub fn client_check(
    pin: &ReservePin,
    entity: &str,
    links: &[ReserveLink],
    headers: &HeaderChain,
) -> Result<ClientVerdict, ReserveError> {
    use borsh::BorshDeserialize;
    let bad = |l: &ReserveLink, m: &str| {
        ReserveError::Chain(format!("round {} {}-{}: {m}", l.round, l.from, l.to))
    };
    if !pin.names(entity) {
        return Err(ReserveError::AddressList(format!(
            "{entity}: not an entity of the pinned label set"
        )));
    }
    pin.check_on(headers)?;
    let schema = keccak256(onyx_reserve_schema::SCHEMA_ENTITY_RESERVE);
    let mut windows = pin.windows();
    let mut rows = Vec::with_capacity(links.len());
    for l in links {
        if l.recipe != pin.recipe {
            return Err(bad(
                l,
                "another recipe than the pinned btc-entity-reserve build",
            ));
        }
        if l.schema != schema {
            return Err(bad(l, "not a reserve row schema"));
        }
        if l.to < l.from {
            return Err(bad(l, "the range ends before it starts"));
        }
        if l.to > pin.labels.published {
            return Err(bad(
                l,
                &format!(
                    "reaches past the label set's publication at {}",
                    pin.labels.published
                ),
            ));
        }
        let w = windows.window(l.from, l.to).map_err(|m| bad(l, &m))?;
        if l.label_set_hash != w.set_hash {
            return Err(bad(
                l,
                "another label set than the pinned scan's set has for these blocks (window hash)",
            ));
        }
        let tip = hex::encode(l.tip.0);
        if l.to == pin.scan_height && !tip.eq_ignore_ascii_case(&pin.scan_block) {
            return Err(bad(l, "ends on another block than the committed scan's"));
        }
        match headers.require_confirmed(l.to, &tip, MIN_CONFIRMATIONS) {
            Ok(()) => {}
            Err(ReserveError::StaleBranch(h)) => {
                return Err(bad(
                    l,
                    &format!(
                        "ends on block {tip}, not the header chain's block at {h}: a stale branch (RES-4)"
                    ),
                ))
            }
            Err(e) => return Err(bad(l, &format!("{e} (RES-4)"))),
        }
        let Some(index) = w.index_of(entity) else {
            return Err(bad(l, "the entity has no address in these blocks"));
        };
        if l.index != index {
            return Err(bad(l, "not the entity's leaf index"));
        }
        let o = onyx_recipe_sdk::OutputRow::try_from_slice(&l.leaf)
            .map_err(|_| bad(l, "leaf does not decode"))?;
        let r = EntityReserveRow::try_from_slice(&o.value)
            .map_err(|_| bad(l, "row does not decode"))?;
        if o.key != entity.as_bytes() || r.entity != entity {
            return Err(bad(l, "not the entity's row"));
        }
        rows.push(r);
    }
    let el: Vec<EntityLink<'_>> = links
        .iter()
        .zip(&rows)
        .map(|(l, row)| EntityLink {
            from: l.from,
            to: l.to,
            root: l.root,
            row,
        })
        .collect();
    let chain = derive_entity_span(&pin.anchor(), entity, &el)?;
    Ok(ClientVerdict {
        entity: entity.to_string(),
        chain,
        rows,
    })
}

/// Decimal-string serde for satoshi amounts (JSON numbers lose precision in browsers).
mod dec {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_core::{Confidence, EntityKind};
    use onyx_labels::Label;

    fn set() -> LabelSet {
        let l = |a: &str, e: &str| Label {
            address: a.into(),
            entity: e.into(),
            kind: EntityKind::Exchange,
            confidence: Confidence(900_000),
            evidence: vec![],
        };
        LabelSet {
            name: "test.set".into(),
            version: 1,
            labels: vec![
                l("btc:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa", "entity:b"),
                l("btc:bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4", "entity:a"),
            ],
        }
    }

    fn row(a: &str, e: &str, sats: u64) -> ScanRow {
        ScanRow {
            address: a.into(),
            entity: e.into(),
            utxos: 1,
            sats,
        }
    }

    fn scan(height: u64, rows: Vec<ScanRow>) -> ScanSnapshot {
        let s = set();
        ScanSnapshot {
            version: 1,
            method: "test".into(),
            height,
            block_hash: "00".repeat(32),
            label_set_name: s.name.clone(),
            label_set_version: s.version,
            label_set_hash: format!("0x{}", hex::encode(s.content_hash().0)),
            addresses_scanned: 2,
            rows,
        }
    }

    const A: &str = "btc:bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
    const B: &str = "btc:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa";

    /// RES-2: a scan of another address list, or one naming another entity
    /// or an address outside the set, is refused.
    #[test]
    fn a_scan_of_another_address_list_is_refused() {
        let s = set();
        let good = scan(10, vec![row(B, "entity:b", 5), row(A, "entity:a", 7)]);
        assert_eq!(good.check_labels(&s), Ok(()));
        let mut bad = good.clone();
        bad.addresses_scanned = 1;
        assert!(matches!(
            bad.check_labels(&s),
            Err(ReserveError::AddressList(_))
        ));
        let mut bad = good.clone();
        bad.rows[0].entity = "entity:a".into();
        assert!(matches!(
            bad.check_labels(&s),
            Err(ReserveError::AddressList(_))
        ));
        let mut bad = good.clone();
        bad.rows[1].address = "btc:bc1qnotinset".into();
        assert!(bad.check_labels(&s).is_err());
        let mut bad = good.clone();
        bad.label_set_hash = format!("0x{}", "11".repeat(32));
        assert!(matches!(
            bad.check_labels(&s),
            Err(ReserveError::AddressList(_))
        ));
        let mut bad = good.clone();
        bad.rows.swap(0, 1);
        assert!(matches!(
            bad.check_labels(&s),
            Err(ReserveError::Snapshot(_))
        ));
        // Each label-set field and each empty-row field on its own (mutation L4).
        let mut bad = good.clone();
        bad.label_set_name = "test.other".into();
        assert!(matches!(
            bad.check_labels(&s),
            Err(ReserveError::AddressList(_))
        ));
        let mut bad = good.clone();
        bad.label_set_version += 1;
        assert!(matches!(
            bad.check_labels(&s),
            Err(ReserveError::AddressList(_))
        ));
        for (utxos, sats) in [(0, 5), (1, 0)] {
            let mut bad = good.clone();
            bad.rows[0].utxos = utxos;
            bad.rows[0].sats = sats;
            assert!(matches!(
                bad.check_labels(&s),
                Err(ReserveError::Snapshot(_))
            ));
        }
        // Any edit moves the hash the first shard names.
        let mut moved = good.clone();
        moved.rows[0].sats += 1;
        assert_ne!(moved.hash(), good.hash());
        assert_eq!(
            good.entity_totals(&s).expect("totals"),
            vec![
                EntityBase {
                    entity: "entity:a".into(),
                    balance_sats: 7
                },
                EntityBase {
                    entity: "entity:b".into(),
                    balance_sats: 5
                },
            ]
        );
    }

    /// RES-4: a scan of a block that is not the chain's block at its height
    /// (a reorged-out tip) is refused.
    #[test]
    fn a_scan_of_a_reorged_block_is_refused() {
        let s = scan(10, vec![]);
        assert_eq!(s.check_block(&"00".repeat(32)), Ok(()));
        assert_eq!(
            s.check_block(&"01".repeat(32)),
            Err(ReserveError::NotOnChain(10))
        );
    }

    #[test]
    fn base_is_scan_minus_flows_and_never_negative() {
        let totals = vec![
            EntityBase {
                entity: "entity:a".into(),
                balance_sats: 100,
            },
            EntityBase {
                entity: "entity:b".into(),
                balance_sats: 0,
            },
        ];
        let net: BTreeMap<String, i128> = [("entity:a".to_string(), -50i128)].into();
        assert_eq!(
            derive_base(&totals, &net).expect("base")[0].balance_sats,
            150
        );
        let net: BTreeMap<String, i128> = [("entity:b".to_string(), 1i128)].into();
        assert_eq!(
            derive_base(&totals, &net),
            Err(ReserveError::Negative("entity:b".into()))
        );
        let net: BTreeMap<String, i128> = [("entity:z".to_string(), 0i128)].into();
        assert!(derive_base(&totals, &net).is_err());
    }

    /// R-RES-NEG-2: the per-address derivation refuses an address whose scan
    /// minus its own flows is negative, even when its entity's total is not.
    #[test]
    fn the_base_is_derived_address_by_address() {
        let s = set();
        let t = scan(12, vec![row(B, "entity:b", 5), row(A, "entity:a", 7)]);
        let flows: BTreeMap<String, (u128, u128)> =
            [(A.to_string(), (4u128, 1u128)), (B.to_string(), (0, 5))].into();
        let base = derive_base_by_address(&t, &s, &flows).expect("base");
        assert_eq!(
            base.iter().map(|b| b.balance_sats).collect::<Vec<_>>(),
            vec![4, 10]
        );
        // Equal to the entity-level derivation on honest data.
        let net: BTreeMap<String, i128> = [
            ("entity:a".to_string(), 3i128),
            ("entity:b".to_string(), -5i128),
        ]
        .into();
        assert_eq!(
            derive_base(&t.entity_totals(&s).expect("totals"), &net),
            Ok(base)
        );
        // An address that received more than it holds now and never sent.
        let flows: BTreeMap<String, (u128, u128)> = [(A.to_string(), (8u128, 0u128))].into();
        assert_eq!(
            derive_base_by_address(&t, &s, &flows),
            Err(ReserveError::Negative(A.to_string()))
        );
        // A flow at an address outside the set.
        let flows: BTreeMap<String, (u128, u128)> =
            [("btc:bc1qnotinset".to_string(), (0u128, 0u128))].into();
        assert!(derive_base_by_address(&t, &s, &flows).is_err());
    }

    /// A base or running balance of exactly zero is fine; one satoshi below
    /// is refused, at the base height and after every block (mutation L4).
    #[test]
    fn a_zero_balance_is_allowed_and_one_below_is_not() {
        let s = set();
        let t = scan(12, vec![row(B, "entity:b", 5), row(A, "entity:a", 7)]);
        let zero: BTreeMap<String, (u128, u128)> = [(A.to_string(), (7u128, 0u128))].into();
        let base = derive_base_by_address(&t, &s, &zero).expect("zero base");
        assert_eq!(
            base.iter().map(|b| b.balance_sats).collect::<Vec<_>>(),
            vec![0, 5]
        );
        let below: BTreeMap<String, (u128, u128)> = [(A.to_string(), (8u128, 0u128))].into();
        assert_eq!(
            derive_base_by_address(&t, &s, &below),
            Err(ReserveError::Negative(A.to_string()))
        );
        assert_eq!(
            check_running_balances(&t, &s, &[(11, zero.clone()), (12, BTreeMap::new())]),
            Ok(())
        );
        assert!(matches!(
            check_running_balances(&t, &s, &[(11, BTreeMap::new()), (12, below)]),
            Err(ReserveError::Negative(_))
        ));
        // Still negative after an earlier block brings it back to zero.
        let back: BTreeMap<String, (u128, u128)> = [(A.to_string(), (0u128, 1u128))].into();
        let below: BTreeMap<String, (u128, u128)> = [(A.to_string(), (8u128, 0u128))].into();
        assert!(matches!(
            check_running_balances(&t, &s, &[(11, back), (12, below)]),
            Err(ReserveError::Negative(_))
        ));
    }

    /// R-RES-NEG-2 vs R-RES-NEG-1: two addresses of one entity, one short by
    /// N and one long by N, pass the entity-level check and fail the
    /// per-address one.
    #[test]
    fn a_per_entity_check_misses_what_the_per_address_check_finds() {
        let l = |a: &str| Label {
            address: a.into(),
            entity: "entity:a".into(),
            kind: EntityKind::Exchange,
            confidence: Confidence(900_000),
            evidence: vec![],
        };
        let s = LabelSet {
            name: "test.set".into(),
            version: 1,
            labels: vec![l(B), l(A)],
        };
        let mut t = scan(12, vec![row(B, "entity:a", 5), row(A, "entity:a", 7)]);
        t.label_set_hash = format!("0x{}", hex::encode(s.content_hash().0));
        // B received 6 but holds 5 and sent nothing: impossible at B alone.
        let flows: BTreeMap<String, (u128, u128)> = [(B.to_string(), (6u128, 0u128))].into();
        let net: BTreeMap<String, i128> = [("entity:a".to_string(), 6i128)].into();
        assert!(derive_base(&t.entity_totals(&s).expect("totals"), &net).is_ok());
        assert_eq!(
            derive_base_by_address(&t, &s, &flows),
            Err(ReserveError::Negative(B.to_string()))
        );
    }

    /// RES-1: two scans reconcile through the flows between them, per address;
    /// a scan that missed an address's coins does not.
    #[test]
    fn two_scans_reconcile_through_the_flows() {
        let t0 = scan(10, vec![row(B, "entity:b", 5), row(A, "entity:a", 7)]);
        let t1 = scan(12, vec![row(A, "entity:a", 10)]);
        let flows: BTreeMap<String, (u128, u128)> =
            [(A.to_string(), (4u128, 1u128)), (B.to_string(), (0, 5))].into();
        assert_eq!(reconcile(&t0, &t1, &flows), Ok(vec![]));
        let missed = scan(10, vec![row(A, "entity:a", 7)]);
        assert_eq!(reconcile(&missed, &t1, &flows), Ok(vec![B.to_string()]));
        assert!(reconcile(&t1, &t0, &flows).is_err());
    }

    fn rrow(
        e: &str,
        bh: u64,
        h: u64,
        link: Hash32,
        base: u64,
        rx: u128,
        tx: u128,
    ) -> EntityReserveRow {
        EntityReserveRow {
            entity: e.into(),
            base_height: bh,
            height: h,
            base_ref: link.0,
            base_sats: base,
            received_sats: rx,
            sent_sats: tx,
            tx_count: 1,
            balance_sats: (i128::from(base) + rx as i128 - tx as i128) as u64,
        }
    }

    /// R-RES-CHAIN-1: every link of a reserve chain is checked.
    #[test]
    fn the_chain_checker_finds_every_broken_link() {
        let snap = Hash32([1; 32]);
        let base = vec![EntityBase {
            entity: "entity:a".into(),
            balance_sats: 10,
        }];
        let s1 = ReserveShard {
            from: 11,
            to: 12,
            root: Hash32([2; 32]),
            rows: vec![rrow("entity:a", 10, 12, snap, 10, 5, 3)],
        };
        let s2 = ReserveShard {
            from: 13,
            to: 13,
            root: Hash32([3; 32]),
            rows: vec![rrow("entity:a", 12, 13, Hash32([2; 32]), 12, 0, 2)],
        };
        let end = check_chain(10, &base, &snap, &[s1.clone(), s2.clone()]).expect("chain");
        assert_eq!(end[0].balance_sats, 10);
        // A gap, a wrong base, a wrong link, a wrong first reference, bad arithmetic.
        let mut gap = s2.clone();
        gap.from = 14;
        assert!(check_chain(10, &base, &snap, &[s1.clone(), gap]).is_err());
        // Each start rule on its own (mutation L4): a gap whose rows name
        // its range, and a reversed range that starts in the right place.
        let gap = ReserveShard {
            from: 14,
            to: 14,
            root: Hash32([3; 32]),
            rows: vec![rrow("entity:a", 12, 14, Hash32([2; 32]), 12, 0, 2)],
        };
        assert!(check_chain(10, &base, &snap, &[s1.clone(), gap]).is_err());
        let reversed = ReserveShard {
            from: 13,
            to: 12,
            root: Hash32([3; 32]),
            rows: vec![rrow("entity:a", 12, 12, Hash32([2; 32]), 12, 0, 2)],
        };
        assert!(check_chain(10, &base, &snap, &[s1.clone(), reversed]).is_err());
        // A row for another entity on the same base amount.
        let mut renamed = s2.clone();
        renamed.rows[0].entity = "entity:z".into();
        assert!(check_chain(10, &base, &snap, &[s1.clone(), renamed]).is_err());
        // A row naming another base height, or another end height, alone.
        let mut base_height = s2.clone();
        base_height.rows[0].base_height = 11;
        assert!(check_chain(10, &base, &snap, &[s1.clone(), base_height]).is_err());
        let mut end_height = s2.clone();
        end_height.rows[0].height = 14;
        assert!(check_chain(10, &base, &snap, &[s1.clone(), end_height]).is_err());
        let mut wrong_base = s2.clone();
        wrong_base.rows[0].base_sats = 13;
        assert!(check_chain(10, &base, &snap, &[s1.clone(), wrong_base]).is_err());
        let mut wrong_link = s2.clone();
        wrong_link.rows[0].base_ref = [9; 32];
        assert!(check_chain(10, &base, &snap, &[s1.clone(), wrong_link]).is_err());
        assert!(check_chain(10, &base, &Hash32([8; 32]), &[s1.clone(), s2.clone()]).is_err());
        let mut sum = s1.clone();
        sum.rows[0].balance_sats += 1;
        assert!(check_chain(10, &base, &snap, &[sum]).is_err());
        assert!(check_chain(9, &base, &snap, &[s1]).is_err());
    }

    /// R-RES-PARAMS-1 (THREATS RES-7): each spec's params must be the
    /// committed start, then the previous finalized shard's end; a shard run
    /// on any other base is refused even when its own rows add up.
    #[test]
    fn each_shards_params_must_be_the_previous_finalized_end() {
        let snap = Hash32([1; 32]);
        let base = vec![EntityBase {
            entity: "entity:a".into(),
            balance_sats: 10,
        }];
        let s1 = ReserveShard {
            from: 11,
            to: 12,
            root: Hash32([2; 32]),
            rows: vec![rrow("entity:a", 10, 12, snap, 10, 5, 3)],
        };
        let s2 = ReserveShard {
            from: 13,
            to: 13,
            root: Hash32([3; 32]),
            rows: vec![rrow("entity:a", 12, 13, Hash32([2; 32]), 12, 0, 2)],
        };
        let p = |h: u64, link: Hash32, sats: u64| ReserveParams {
            base_height: h,
            base_ref: link.0,
            base: vec![EntityBase {
                entity: "entity:a".into(),
                balance_sats: sats,
            }],
        };
        let good = vec![p(10, snap, 10), p(12, Hash32([2; 32]), 12)];
        let shards = vec![s1.clone(), s2.clone()];
        assert_eq!(
            check_params_chain(10, &base, &snap, &good, &shards).map(|e| e[0].balance_sats),
            Ok(10)
        );
        // A second shard's spec on another base, height or reference.
        for bad in [
            p(12, Hash32([2; 32]), 13),
            p(11, Hash32([2; 32]), 12),
            p(12, Hash32([9; 32]), 12),
        ] {
            let ps = vec![good[0].clone(), bad];
            assert!(check_params_chain(10, &base, &snap, &ps, &shards).is_err());
        }
        // A forged first base (review RES-R1 F2: 1,000 BTC instead of the
        // committed one), its shard consistent with it, is refused against
        // the committed start.
        let forged = ReserveShard {
            from: 11,
            to: 12,
            root: Hash32([4; 32]),
            rows: vec![rrow("entity:a", 10, 12, snap, 100_000_000_000, 5, 3)],
        };
        assert!(
            check_params_chain(10, &base, &snap, &[p(10, snap, 100_000_000_000)], &[forged])
                .is_err()
        );
        assert!(check_params_chain(10, &base, &snap, &good[..1], &shards).is_err());
    }
}
