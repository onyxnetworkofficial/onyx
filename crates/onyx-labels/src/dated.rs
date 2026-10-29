//! Dated label sets (ADR-0037, THREATS RES-2): each membership "this address
//! belonged to this entity" holds over a range of block heights, taken from
//! the source's own dated publications, so a metric at height `h` uses exactly
//! the addresses valid at `h` and a rotated-out address stops counting after
//! its range ends.
//!
//! Commitment: one Merkle leaf per membership (Borsh [`Membership`], label
//! plus `from`/`to`), leaves sorted by `(address, from)`; the content hash is
//! Keccak of a domain-tagged Borsh header naming the root (format
//! [`DATED_FORMAT`]). A client can check one address's ranges with a leaf
//! proof ([`DatedLabelSet::member_proof`], [`verify_membership`]).
//!
//! Recipes keep reading plain [`LabelSet`]s: a shard over blocks `f..=t` gets
//! the projection [`DatedLabelSet::window`], the members valid at every height
//! of the range, and a membership change inside the range is refused (the
//! shard must start at the change). Undated sets (format 1) keep their bytes,
//! hashes and goldens.

use crate::{validate_label, Label, LabelError, LabelSet};
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::ids::is_dotted_id;
use onyx_core::{keccak256, to_canonical_bytes, Hash32, Registry};
use onyx_merkle::{MerkleTree, Proof};

/// Format number of a dated set (undated [`LabelSet`]s are format 1).
pub const DATED_FORMAT: u32 = 2;
/// Domain tag of the content hash.
pub const DATED_DOMAIN: &str = "onyx-dated-label-set-v1";

/// One membership: `label.address` belonged to `label.entity` at every block
/// height `from..=to` (`to = None`: still listed when the set was published).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Membership {
    /// The label (address, entity, kind, confidence, evidence: the dated
    /// publication that lists it).
    pub label: Label,
    /// First height the address counts for the entity (inclusive).
    pub from: u64,
    /// Last height it counts (inclusive); `None`: open, still listed.
    pub to: Option<u64>,
}

impl Membership {
    /// True if the membership holds at height `h`.
    pub fn holds_at(&self, h: u64) -> bool {
        self.from <= h && self.to.is_none_or(|t| h <= t)
    }

    /// True if it holds at every height of `from..=to`.
    fn covers(&self, from: u64, to: u64) -> bool {
        self.from <= from && self.to.is_none_or(|t| to <= t)
    }
}

/// A versioned set of dated memberships, strictly sorted by `(address, from)`.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DatedLabelSet {
    /// Name, as for [`LabelSet`].
    pub name: String,
    /// Version, monotonic per name.
    pub version: u32,
    /// Block height at which this version was published: no membership starts
    /// after it, no closed range ends at or after it, and a later version may
    /// not change any membership at or before it (no back-dating,
    /// [`DatedLabelSet::check_successor`]).
    pub published: u64,
    /// Memberships, strictly ascending by `(address, from)`.
    pub members: Vec<Membership>,
}

/// The hashed header of a dated set.
#[derive(BorshSerialize)]
struct DatedCommit<'a> {
    domain: &'a str,
    format: u32,
    name: &'a str,
    version: u32,
    published: u64,
    members: u64,
    root: Hash32,
}

/// Dated set error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DatedError {
    /// Name or version malformed.
    #[error("dated label set name or version malformed")]
    BadHeader,
    /// Memberships not strictly ascending by (address, from).
    #[error("memberships must be strictly ascending by address and start (at {0})")]
    NotCanonical(String),
    /// `to` before `from`.
    #[error("{0}: range ends before it starts")]
    BadRange(String),
    /// Two ranges of one address overlap (one entity or two).
    #[error("{0}: two memberships overlap")]
    Overlap(String),
    /// Two adjacent ranges of one address carry the same label: one range.
    #[error("{0}: adjacent ranges with the same label must be one range")]
    Split(String),
    /// A membership starts after, or a closed one ends at or after, the publication.
    #[error("{0}: range reaches past the set's publication height")]
    AfterPublication(String),
    /// A label fails the registry checks.
    #[error("label: {0}")]
    Label(LabelError),
    /// A window crosses a membership change; shards must start at every change.
    #[error("membership changes at height {at}, inside blocks {from}-{to}")]
    ChangesInside {
        /// Window start.
        from: u64,
        /// Window end.
        to: u64,
        /// First height of the new membership.
        at: u64,
    },
    /// An empty window (`from > to`).
    #[error("empty window {0}-{1}")]
    EmptyWindow(u64, u64),
    /// A successor is not the next version of the same set, published later.
    #[error("not the next version: {0}")]
    NotNext(&'static str),
    /// A successor changes a membership at or before the previous publication.
    #[error("{address}: membership at or before height {published} changed (back-dated)")]
    BackDated {
        /// The address whose history changed.
        address: String,
        /// The previous version's publication height.
        published: u64,
    },
}

impl DatedLabelSet {
    /// The undated reading of a format-1 set: every label at every height.
    /// Its windows are the set itself (same name, version and labels, so the
    /// same content hash); it carries the RES-2 residual (rotated-out
    /// addresses keep counting) and is how undated sets run through the dated
    /// checks unchanged.
    pub fn from_undated(set: &LabelSet) -> DatedLabelSet {
        DatedLabelSet {
            name: set.name.clone(),
            version: set.version,
            published: u64::MAX,
            members: set
                .labels
                .iter()
                .map(|l| Membership {
                    label: l.clone(),
                    from: 0,
                    to: None,
                })
                .collect(),
        }
    }

    /// Structure only (no registry): header, order, ranges, no overlap, no
    /// split, nothing past the publication.
    pub fn check_structure(&self) -> Result<(), DatedError> {
        if !is_dotted_id(&self.name) || self.version == 0 {
            return Err(DatedError::BadHeader);
        }
        for w in self.members.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            if (a.label.address.as_str(), a.from) >= (b.label.address.as_str(), b.from) {
                return Err(DatedError::NotCanonical(b.label.address.clone()));
            }
            if a.label.address == b.label.address {
                // Sorted by start: a overlaps b iff a is open or ends at or after b's start.
                match a.to {
                    None => return Err(DatedError::Overlap(b.label.address.clone())),
                    Some(t) if t >= b.from => {
                        return Err(DatedError::Overlap(b.label.address.clone()))
                    }
                    Some(t) if t.checked_add(1) == Some(b.from) && a.label == b.label => {
                        return Err(DatedError::Split(b.label.address.clone()))
                    }
                    Some(_) => {}
                }
            }
        }
        for m in &self.members {
            let addr = || m.label.address.clone();
            if m.to.is_some_and(|t| t < m.from) {
                return Err(DatedError::BadRange(addr()));
            }
            if m.from > self.published || m.to.is_some_and(|t| t >= self.published) {
                return Err(DatedError::AfterPublication(addr()));
            }
        }
        Ok(())
    }

    /// Full check against the reviewed registry: [`Self::check_structure`]
    /// plus every label as [`LabelSet::validate`] checks it.
    pub fn validate(&self, reg: &dyn Registry) -> Result<(), DatedError> {
        self.check_structure()?;
        self.members
            .iter()
            .try_for_each(|m| validate_label(&m.label, reg))
            .map_err(DatedError::Label)
    }

    /// The Merkle leaves: Borsh of each membership, in order.
    pub fn leaves(&self) -> Vec<Vec<u8>> {
        self.members
            .iter()
            .map(|m| to_canonical_bytes(m).unwrap_or_default())
            .collect()
    }

    /// Merkle root over [`Self::leaves`].
    pub fn members_root(&self) -> Hash32 {
        MerkleTree::from_rows(&self.leaves()).root()
    }

    /// Content hash: Keccak of the Borsh header (domain, format, name,
    /// version, publication height, member count, members root).
    pub fn content_hash(&self) -> Hash32 {
        let c = DatedCommit {
            domain: DATED_DOMAIN,
            format: DATED_FORMAT,
            name: &self.name,
            version: self.version,
            published: self.published,
            members: self.members.len() as u64,
            root: self.members_root(),
        };
        keccak256(&to_canonical_bytes(&c).unwrap_or_default())
    }

    /// Leaf proof of membership `index` under [`Self::members_root`].
    pub fn member_proof(&self, index: u64) -> Option<Proof> {
        MerkleTree::from_rows(&self.leaves()).proof(index)
    }

    /// Every height at which the membership changes: each range's start and
    /// the height after each closed range's end, ascending, no duplicates.
    pub fn boundaries(&self) -> Vec<u64> {
        let mut b: Vec<u64> = self
            .members
            .iter()
            .flat_map(|m| [Some(m.from), m.to.and_then(|t| t.checked_add(1))])
            .flatten()
            .collect();
        b.sort_unstable();
        b.dedup();
        b
    }

    /// The memberships that hold at height `h` (at most one per address).
    pub fn members_at(&self, h: u64) -> impl Iterator<Item = &Membership> {
        self.members.iter().filter(move |m| m.holds_at(h))
    }

    /// Every address of the set, ascending, once.
    pub fn addresses(&self) -> Vec<&str> {
        let mut a: Vec<&str> = self
            .members
            .iter()
            .map(|m| m.label.address.as_str())
            .collect();
        a.dedup();
        a
    }

    /// The entity a scan at height `h` files `address` under: its entity at
    /// `h`, else that of its last membership before `h`, else its first after.
    pub fn scan_entity(&self, address: &str, h: u64) -> Option<&str> {
        let mine: Vec<&Membership> = self
            .members
            .iter()
            .filter(|m| m.label.address == address)
            .collect();
        mine.iter()
            .find(|m| m.holds_at(h))
            .or_else(|| mine.iter().rev().find(|m| m.to.is_some_and(|t| t < h)))
            .or_else(|| mine.first())
            .map(|m| m.label.entity.as_str())
    }

    /// [`Self::scan_entity`] of every address at once (linear in the members).
    pub fn scan_owners(&self, h: u64) -> std::collections::BTreeMap<&str, &str> {
        let mut out = std::collections::BTreeMap::new();
        let mut i = 0;
        while i < self.members.len() {
            let addr = self.members[i].label.address.as_str();
            let mut j = i;
            while j < self.members.len() && self.members[j].label.address == addr {
                j += 1;
            }
            let mine = &self.members[i..j];
            let pick = mine
                .iter()
                .find(|m| m.holds_at(h))
                .or_else(|| mine.iter().rev().find(|m| m.to.is_some_and(|t| t < h)))
                .unwrap_or(&mine[0]);
            out.insert(addr, pick.label.entity.as_str());
            i = j;
        }
        out
    }

    /// The labels valid at every height of `from..=to`, as the plain set a
    /// recipe reads (same name and version; its content hash is the
    /// window's). A membership change inside the range is refused: the
    /// first height of a change must be a window's first height.
    pub fn window(&self, from: u64, to: u64) -> Result<LabelSet, DatedError> {
        if from > to {
            return Err(DatedError::EmptyWindow(from, to));
        }
        if let Some(at) = self
            .boundaries()
            .into_iter()
            .find(|b| *b > from && *b <= to)
        {
            return Err(DatedError::ChangesInside { from, to, at });
        }
        Ok(LabelSet {
            name: self.name.clone(),
            version: self.version,
            labels: self
                .members
                .iter()
                .filter(|m| m.covers(from, to))
                .map(|m| m.label.clone())
                .collect(),
        })
    }

    /// Version `n + 1` of `prev` (no back-dating, THREATS RES-2): same name,
    /// the next version number, published at a later height, other content,
    /// and the same memberships as `prev` at every height up to
    /// `prev.published` (a later version may close an open range at or after
    /// that height, or add ranges that start after it; it may not add, end or
    /// change one earlier).
    pub fn check_successor(&self, prev: &DatedLabelSet) -> Result<(), DatedError> {
        if self.name != prev.name {
            return Err(DatedError::NotNext("another name"));
        }
        if prev.version.checked_add(1) != Some(self.version) {
            return Err(DatedError::NotNext("version must be the next number"));
        }
        if self.published <= prev.published {
            return Err(DatedError::NotNext("published at or before the previous"));
        }
        if self.content_hash() == prev.content_hash() {
            return Err(DatedError::NotNext("no change"));
        }
        let p = prev.published;
        let clip = |s: &DatedLabelSet| -> Vec<(Label, u64, u64)> {
            s.members
                .iter()
                .filter(|m| m.from <= p)
                .map(|m| (m.label.clone(), m.from, m.to.map_or(p, |t| t.min(p))))
                .collect()
        };
        let (old, new) = (clip(prev), clip(self));
        if old != new {
            // The first address whose clipped history differs.
            let address = old
                .iter()
                .zip(&new)
                .find(|(a, b)| a != b)
                .map(|(a, b)| a.0.address.clone().min(b.0.address.clone()))
                .or_else(|| {
                    old.get(new.len())
                        .or_else(|| new.get(old.len()))
                        .map(|x| x.0.address.clone())
                })
                .unwrap_or_default();
            return Err(DatedError::BackDated {
                address,
                published: p,
            });
        }
        Ok(())
    }
}

/// A whole version history, oldest first (review D1-R1 F2): version 1, then
/// each version [`DatedLabelSet::check_successor`] of the one before. A
/// serving gate that anchors on version n takes all n versions, so a
/// back-dated version is refused where it is used, not only by publishers.
///
/// Format-1 versions read with [`DatedLabelSet::from_undated`] may come first
/// (ADR-0037 Amendment 1: label set v3 is the first dated version of
/// `test.onyx.entities.btc`, after the undated v1 and v2). The dated history
/// starts at the first dated version: an undated set claims every height, so
/// no dated set can be its successor, and what was served under an undated
/// version stays bound to that version's content hash. Undated versions must
/// be a prefix, numbered from 1 with no gap, of one name, each changing the
/// content.
pub fn check_history(versions: &[DatedLabelSet]) -> Result<(), DatedError> {
    let first = versions.first().ok_or(DatedError::NotNext("no version"))?;
    if first.version != 1 {
        return Err(DatedError::NotNext("the history must start at version 1"));
    }
    first.check_structure()?;
    for w in versions.windows(2) {
        let (prev, next) = (&w[0], &w[1]);
        next.check_structure()?;
        match (is_undated(prev), is_undated(next)) {
            (_, true) if !is_undated(prev) => {
                return Err(DatedError::NotNext("an undated version after a dated one"))
            }
            (true, _) => {
                if next.name != prev.name {
                    return Err(DatedError::NotNext("another name"));
                }
                if prev.version.checked_add(1) != Some(next.version) {
                    return Err(DatedError::NotNext("version must be the next number"));
                }
                if next.members == prev.members {
                    return Err(DatedError::NotNext("no change"));
                }
            }
            _ => next.check_successor(prev)?,
        }
    }
    Ok(())
}

/// A format-1 set read by [`DatedLabelSet::from_undated`]: published "never",
/// every member over all heights.
fn is_undated(s: &DatedLabelSet) -> bool {
    s.published == u64::MAX && s.members.iter().all(|m| m.from == 0 && m.to.is_none())
}

/// Check one membership against a dated set's members root.
pub fn verify_membership(root: &Hash32, m: &Membership, proof: &Proof) -> bool {
    onyx_merkle::verify(root, &to_canonical_bytes(m).unwrap_or_default(), proof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_core::{AddressCanon, Confidence, EntityKind, StaticRegistry};

    struct Lower;
    impl AddressCanon for Lower {
        fn namespace(&self) -> &str {
            "t"
        }
        fn canon_id(&self) -> &str {
            "onyx.canon.test.v1"
        }
        fn vectors(&self) -> &'static [(&'static str, bool)] {
            &[]
        }
        fn is_canonical(&self, body: &str) -> bool {
            !body.bytes().any(|c| c.is_ascii_uppercase())
        }
    }

    fn reg() -> StaticRegistry {
        let mut r = StaticRegistry::new();
        r.add_entity("entity:a", EntityKind::Exchange)
            .and_then(|r| r.add_entity("entity:b", EntityKind::Exchange))
            .and_then(|r| r.add_canon(Box::new(Lower)))
            .expect("registry");
        r
    }

    fn m(addr: &str, entity: &str, from: u64, to: Option<u64>) -> Membership {
        Membership {
            label: Label {
                address: addr.into(),
                entity: entity.into(),
                kind: EntityKind::Exchange,
                confidence: Confidence(900_000),
                evidence: vec![],
            },
            from,
            to,
        }
    }

    fn set(members: Vec<Membership>) -> DatedLabelSet {
        DatedLabelSet {
            name: "onyx.test".into(),
            version: 1,
            published: 1_000,
            members,
        }
    }

    /// A rotation: `t:old` leaves entity A after 199, `t:new` joins at 200,
    /// `t:keep` stays.
    fn rotation() -> DatedLabelSet {
        set(vec![
            m("t:keep", "entity:a", 100, None),
            m("t:new", "entity:a", 200, None),
            m("t:old", "entity:a", 100, Some(199)),
        ])
    }

    #[test]
    fn a_window_holds_exactly_the_addresses_valid_at_every_height() {
        let s = rotation();
        assert_eq!(s.validate(&reg()), Ok(()));
        let addrs =
            |w: LabelSet| -> Vec<String> { w.labels.into_iter().map(|l| l.address).collect() };
        assert_eq!(
            addrs(s.window(100, 199).expect("before")),
            ["t:keep", "t:old"]
        );
        assert_eq!(
            addrs(s.window(200, 900).expect("after")),
            ["t:keep", "t:new"]
        );
        // Before anything is listed: an empty set.
        assert_eq!(
            addrs(s.window(10, 99).expect("empty")),
            Vec::<String>::new()
        );
        // A window crossing the rotation is refused at the first new height.
        assert_eq!(
            s.window(150, 250),
            Err(DatedError::ChangesInside {
                from: 150,
                to: 250,
                at: 200
            })
        );
        // Starting right at the change is fine; ending right before it too.
        assert!(s.window(200, 200).is_ok() && s.window(199, 199).is_ok());
        assert_eq!(s.window(5, 4), Err(DatedError::EmptyWindow(5, 4)));
        assert_eq!(s.boundaries(), vec![100, 200]);
        assert_eq!(s.addresses(), ["t:keep", "t:new", "t:old"]);
    }

    /// RES-2 adversarial: two ranges of one address may not overlap, whether
    /// for one entity or two (an address in two entities at once), and an
    /// open range overlaps everything after it.
    #[test]
    fn overlapping_ranges_and_an_address_in_two_entities_at_once_are_refused() {
        let two = set(vec![
            m("t:x", "entity:a", 100, Some(300)),
            m("t:x", "entity:b", 300, None),
        ]);
        assert_eq!(
            two.check_structure(),
            Err(DatedError::Overlap("t:x".into()))
        );
        let same = set(vec![
            m("t:x", "entity:a", 100, Some(300)),
            m("t:x", "entity:a", 250, Some(400)),
        ]);
        assert_eq!(
            same.check_structure(),
            Err(DatedError::Overlap("t:x".into()))
        );
        let open = set(vec![
            m("t:x", "entity:a", 100, None),
            m("t:x", "entity:b", 500, None),
        ]);
        assert_eq!(
            open.check_structure(),
            Err(DatedError::Overlap("t:x".into()))
        );
        // One after the other is a transfer: allowed, and a boundary.
        let moved = set(vec![
            m("t:x", "entity:a", 100, Some(299)),
            m("t:x", "entity:b", 300, None),
        ]);
        assert_eq!(moved.validate(&reg()), Ok(()));
        assert_eq!(moved.scan_entity("t:x", 250), Some("entity:a"));
        assert_eq!(moved.scan_entity("t:x", 999), Some("entity:b"));
        assert_eq!(moved.scan_entity("t:x", 50), Some("entity:a"));
        assert_eq!(moved.scan_entity("t:y", 50), None);
        for h in [50, 250, 299, 300, 999] {
            assert_eq!(
                moved.scan_owners(h).get("t:x").copied(),
                moved.scan_entity("t:x", h)
            );
        }
        assert!(moved.window(250, 350).is_err());
        // The same label in two adjacent ranges is one range.
        let split = set(vec![
            m("t:x", "entity:a", 100, Some(299)),
            m("t:x", "entity:a", 300, None),
        ]);
        assert_eq!(
            split.check_structure(),
            Err(DatedError::Split("t:x".into()))
        );
    }

    #[test]
    fn order_ranges_and_publication_are_checked() {
        let unsorted = set(vec![
            m("t:b", "entity:a", 1, None),
            m("t:a", "entity:a", 1, None),
        ]);
        assert_eq!(
            unsorted.check_structure(),
            Err(DatedError::NotCanonical("t:a".into()))
        );
        let dup = set(vec![
            m("t:a", "entity:a", 1, Some(5)),
            m("t:a", "entity:b", 1, None),
        ]);
        assert_eq!(
            dup.check_structure(),
            Err(DatedError::NotCanonical("t:a".into()))
        );
        let backwards = set(vec![m("t:a", "entity:a", 9, Some(8))]);
        assert_eq!(
            backwards.check_structure(),
            Err(DatedError::BadRange("t:a".into()))
        );
        // Starts after the publication, or a closed range reaching it.
        let future = set(vec![m("t:a", "entity:a", 1_001, None)]);
        assert_eq!(
            future.check_structure(),
            Err(DatedError::AfterPublication("t:a".into()))
        );
        let closed_late = set(vec![m("t:a", "entity:a", 1, Some(1_000))]);
        assert_eq!(
            closed_late.check_structure(),
            Err(DatedError::AfterPublication("t:a".into()))
        );
        assert_eq!(
            set(vec![m("t:a", "entity:a", 1_000, None)]).check_structure(),
            Ok(())
        );
        let mut bad = rotation();
        bad.version = 0;
        assert_eq!(bad.check_structure(), Err(DatedError::BadHeader));
        // The registry still checks every label (canonical address, entity kind).
        let upper = set(vec![m("t:A", "entity:a", 1, None)]);
        assert!(matches!(upper.validate(&reg()), Err(DatedError::Label(_))));
        let unknown = set(vec![m("t:a", "entity:zz", 1, None)]);
        assert!(matches!(
            unknown.validate(&reg()),
            Err(DatedError::Label(_))
        ));
    }

    /// Review D1-R1 F2: the gate checks the whole history, so a back-dated
    /// version is refused where it is used.
    #[test]
    fn a_history_is_checked_version_by_version() {
        let v1 = DatedLabelSet {
            published: 500,
            members: vec![m("t:keep", "entity:a", 100, None)],
            ..set(vec![])
        };
        let v2 = DatedLabelSet {
            version: 2,
            published: 900,
            members: vec![
                m("t:keep", "entity:a", 100, None),
                m("t:new", "entity:a", 600, None),
            ],
            ..v1.clone()
        };
        assert_eq!(check_history(&[v1.clone(), v2.clone()]), Ok(()));
        assert_eq!(check_history(std::slice::from_ref(&v1)), Ok(()));
        assert!(check_history(&[]).is_err());
        assert!(
            check_history(std::slice::from_ref(&v2)).is_err(),
            "starts at 2"
        );
        let mut back = v2.clone();
        back.members[1].from = 400;
        assert!(matches!(
            check_history(&[v1, back]),
            Err(DatedError::BackDated { .. })
        ));
    }

    /// ADR-0037 Amendment 1: a name's format-1 versions may precede its first
    /// dated version (label set v3 after v1 and v2). The dated history starts
    /// there; an undated version after a dated one, a skipped number, another
    /// name or an unchanged undated version is still refused.
    #[test]
    fn a_dated_history_may_start_after_undated_versions() {
        let u = |version: u32, addr: &str| {
            DatedLabelSet::from_undated(&LabelSet {
                name: "onyx.test".into(),
                version,
                labels: vec![m(addr, "entity:a", 0, None).label],
            })
        };
        let (u1, u2) = (u(1, "t:a"), u(2, "t:b"));
        let d3 = DatedLabelSet {
            version: 3,
            published: 900,
            members: vec![m("t:keep", "entity:a", 100, None)],
            ..set(vec![])
        };
        let d4 = DatedLabelSet {
            version: 4,
            published: 950,
            ..d3.clone()
        };
        assert_eq!(check_history(&[u1.clone(), u2.clone(), d3.clone()]), Ok(()));
        let mut d4b = d4.clone();
        d4b.members.insert(1, m("t:late", "entity:a", 920, None));
        assert_eq!(
            check_history(&[u1.clone(), u2.clone(), d3.clone(), d4b]),
            Ok(())
        );
        // The dated versions after the first still may not back-date.
        let mut back = d4.clone();
        back.members.insert(0, m("t:early", "entity:a", 200, None));
        assert!(matches!(
            check_history(&[u1.clone(), u2.clone(), d3.clone(), back]),
            Err(DatedError::BackDated { .. })
        ));
        // Without the undated prefix, version 3 cannot start a history.
        assert!(check_history(std::slice::from_ref(&d3)).is_err());
        // A skipped number, another name, an unchanged or a late undated version.
        assert!(check_history(&[u1.clone(), d3.clone()]).is_err());
        let other = DatedLabelSet {
            name: "onyx.other".into(),
            ..d3.clone()
        };
        assert!(check_history(&[u1.clone(), u2.clone(), other]).is_err());
        let same = DatedLabelSet {
            version: 2,
            ..u1.clone()
        };
        assert!(check_history(&[u1.clone(), same]).is_err());
        let late = u(4, "t:z");
        assert!(check_history(&[u1.clone(), u2.clone(), d3.clone(), late]).is_err());
        // A dated successor may republish the same members later (a new header).
        assert_eq!(check_history(&[u1, u2, d3, d4]), Ok(()));
    }

    /// RES-2 adversarial (back-dated membership): a later version may not add,
    /// end or change any membership at or before the previous publication.
    #[test]
    fn a_successor_may_not_back_date_a_membership() {
        let v1 = DatedLabelSet {
            published: 500,
            members: vec![
                m("t:keep", "entity:a", 100, None),
                m("t:old", "entity:a", 100, None),
            ],
            ..set(vec![])
        };
        let next = |members: Vec<Membership>| DatedLabelSet {
            version: 2,
            published: 900,
            members,
            ..v1.clone()
        };
        // Honest rotation after the publication: close at >= 500, add from > 500.
        let ok = next(vec![
            m("t:keep", "entity:a", 100, None),
            m("t:new", "entity:a", 600, None),
            m("t:old", "entity:a", 100, Some(599)),
        ]);
        assert_eq!(ok.check_successor(&v1), Ok(()));
        // Closing exactly at the publication height keeps the history too.
        let edge = next(vec![
            m("t:keep", "entity:a", 100, None),
            m("t:old", "entity:a", 100, Some(500)),
        ]);
        assert_eq!(edge.check_successor(&v1), Ok(()));
        let bd = |address: &str| {
            Err(DatedError::BackDated {
                address: address.into(),
                published: 500,
            })
        };
        // A new address back-dated to before the publication.
        let added = next(vec![
            m("t:keep", "entity:a", 100, None),
            m("t:new", "entity:a", 400, None),
            m("t:old", "entity:a", 100, None),
        ]);
        assert_eq!(added.check_successor(&v1), bd("t:new"));
        // An end moved back before the publication (it stops counting earlier).
        let ended = next(vec![
            m("t:keep", "entity:a", 100, None),
            m("t:old", "entity:a", 100, Some(450)),
        ]);
        assert_eq!(ended.check_successor(&v1), bd("t:old"));
        // A start moved earlier, or the entity changed, in published history.
        let earlier = next(vec![
            m("t:keep", "entity:a", 50, None),
            m("t:old", "entity:a", 100, None),
        ]);
        assert_eq!(earlier.check_successor(&v1), bd("t:keep"));
        let moved = next(vec![
            m("t:keep", "entity:b", 100, None),
            m("t:old", "entity:a", 100, None),
        ]);
        assert_eq!(moved.check_successor(&v1), bd("t:keep"));
        // A dropped address (its history vanishes).
        let dropped = next(vec![m("t:keep", "entity:a", 100, None)]);
        assert_eq!(dropped.check_successor(&v1), bd("t:old"));
        // Header rules.
        let mut v = ok.clone();
        v.version = 3;
        assert!(matches!(
            v.check_successor(&v1),
            Err(DatedError::NotNext(_))
        ));
        let mut v = ok.clone();
        v.published = 500;
        assert!(matches!(
            v.check_successor(&v1),
            Err(DatedError::NotNext(_))
        ));
        let mut v = ok.clone();
        v.name = "onyx.other".into();
        assert!(matches!(
            v.check_successor(&v1),
            Err(DatedError::NotNext(_))
        ));
        let mut same = v1.clone();
        same.version = 2;
        same.published = 900;
        assert_eq!(
            same.check_successor(&v1),
            Ok(()),
            "a new version, new header"
        );
    }

    /// The ranges are committed: each changes the content hash, and a leaf
    /// proof shows one address's range against the root.
    #[test]
    fn ranges_are_committed_in_the_merkle_leaves() {
        let s = rotation();
        let h = s.content_hash();
        let mut t = s.clone();
        t.members[2].to = Some(198);
        assert_ne!(t.content_hash(), h);
        let mut t = s.clone();
        t.members[1].from = 201;
        assert_ne!(t.content_hash(), h);
        let mut t = s.clone();
        t.published = 1_001;
        assert_ne!(t.content_hash(), h);
        let root = s.members_root();
        let p = s.member_proof(2).expect("proof");
        assert!(verify_membership(&root, &s.members[2], &p));
        let mut forged = s.members[2].clone();
        forged.to = None;
        assert!(!verify_membership(&root, &forged, &p));
        assert!(s.member_proof(3).is_none());
    }

    /// The undated reading keeps an undated set's windows byte-identical, so
    /// old fixtures verify under the dated checks.
    #[test]
    fn an_undated_set_reads_as_one_unchanging_window() {
        let plain = LabelSet {
            name: "onyx.test".into(),
            version: 2,
            labels: vec![
                m("t:a", "entity:a", 0, None).label,
                m("t:b", "entity:b", 0, None).label,
            ],
        };
        let d = DatedLabelSet::from_undated(&plain);
        assert_eq!(d.check_structure(), Ok(()));
        assert_eq!(d.boundaries(), vec![0]);
        let w = d.window(969_839, 971_199).expect("window");
        assert_eq!(w, plain);
        assert_eq!(w.content_hash(), plain.content_hash());
        assert_ne!(d.content_hash(), plain.content_hash(), "another format");
    }

    /// Shared vector (ADR-0037): the content hash and root of a fixed dated
    /// set. Regenerate only with an ADR change: `ONYX_BLESS=1 cargo test -p onyx-labels`.
    #[test]
    fn dated_set_vector() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/dated/vectors.json"
        );
        let s = rotation();
        let got = serde_json::json!({
            "format": DATED_FORMAT,
            "domain": DATED_DOMAIN,
            "set": {
                "name": s.name,
                "version": s.version,
                "published": s.published,
                "members": s.members.iter().map(|m| serde_json::json!({
                    "address": m.label.address,
                    "entity": m.label.entity,
                    "kind": "Exchange",
                    "confidence_ppm": m.label.confidence.0,
                    "from": m.from,
                    "to": m.to,
                })).collect::<Vec<_>>(),
            },
            "leaves": s.leaves().iter().map(hex::encode).collect::<Vec<_>>(),
            "members_root": format!("0x{}", hex::encode(s.members_root().0)),
            "content_hash": format!("0x{}", hex::encode(s.content_hash().0)),
            "windows": {
                "100-199": format!("0x{}", hex::encode(s.window(100, 199).expect("w").content_hash().0)),
                "200-900": format!("0x{}", hex::encode(s.window(200, 900).expect("w").content_hash().0)),
            },
        });
        if std::env::var_os("ONYX_BLESS").is_some() {
            std::fs::write(
                path,
                serde_json::to_string_pretty(&got).expect("json") + "\n",
            )
            .expect("write");
        }
        let want: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(path)
                .expect("fixtures/dated/vectors.json missing; run with ONYX_BLESS=1"),
        )
        .expect("json");
        assert_eq!(got, want);
    }
}
