//! Issuer rosters to dated label sets (ROADMAP P2a, ADR-0037): an exchange or
//! ETF publishes (or signs) a proof-of-reserve list of its addresses, dated
//! by a block height. [`derive`] turns every issuer's snapshots into one
//! [`DatedLabelSet`] (format 2), deterministically, so anyone holding the same
//! snapshots re-derives the same content hash.
//!
//! Addresses are opaque strings here: the canonical spelling is the
//! registry's job ([`DatedLabelSet::validate`]), not this module's.
//!
//! # Derivation rule
//! For each roster and each address in it, walk the snapshots in height order
//! `s_1..s_n` (heights `h_i`). A range is `(start, grade, first record, last
//! record, last listing height L)`; a listing supports its address for
//! `max_age` blocks, so a range expires after `L + max_age - 1`. At `s_i`:
//! - listed with grade `g`, a range is open with grade `g` and
//!   `h_i <= L + max_age - 1`: extend it (last record `s_i`, `L = h_i`);
//! - listed otherwise (nothing open, the open range expired, or another
//!   grade): close any open range at `min(h_i - 1, L + max_age - 1)`, then
//!   open a new one at `h_i` with grade `g` (first = last = `s_i`, `L = h_i`);
//! - not listed while a range is open: close it at
//!   `min(h_i - 1, L + max_age - 1)`.
//!
//! After `s_n` an open range ends at `E = L + max_age - 1`: open (`to: None`)
//! if `E >= published`, else closed at `E`.
//!
//! Each range is one [`Membership`] of the roster's entity and kind, with
//! confidence [`CONFIDENCE_SIGNED`] or [`CONFIDENCE_PUBLISHED`] by grade, and
//! evidence citing the hash ([`SnapshotRecord::hash`]) of the range's first and
//! last snapshot records (one entry when they are the same record), kind
//! [`EVIDENCE_SIGNED`] or [`EVIDENCE_PUBLISHED`], sorted ascending.
//!
//! Across rosters: every membership of an address in `deny` is dropped
//! ([`DropCause::Denied`]); an address whose memberships of two different
//! entities overlap at any height loses all its memberships
//! ([`DropCause::TwoEntities`]). Memberships of different entities that do
//! not overlap are kept (an address may move). Denied wins when both apply;
//! one [`Dropped`] row per address that had memberships.
//!
//! Before deriving, every snapshot record must be well formed (dotted source
//! id, `YYYY-MM-DD` dates, a known height basis, the roster's entity), heights
//! strictly ascending and at most `published`, listings strictly ascending by
//! address, and one roster per entity. The result must pass
//! [`DatedLabelSet::check_structure`].

use crate::dated::{DatedError, DatedLabelSet, Membership};
use crate::Label;
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::ids::{is_dotted_id, is_entity_id};
use onyx_core::{keccak256, to_canonical_bytes, Confidence, EntityKind, EvidenceRef, Hash32};
use std::collections::BTreeMap;

/// Blocks a listing supports its address: 62 days at the 144-blocks-a-day
/// target (one missed monthly publication).
pub const MAX_LISTING_AGE: u64 = 8_928;
/// Evidence kind of a membership from signed listings.
pub const EVIDENCE_SIGNED: &str = "roster.signed";
/// Evidence kind of a membership from published (unsigned) listings.
pub const EVIDENCE_PUBLISHED: &str = "roster.published";
/// Confidence (ppm) of a membership from signed listings.
pub const CONFIDENCE_SIGNED: u32 = 990_000;
/// Confidence (ppm) of a membership from published listings.
pub const CONFIDENCE_PUBLISHED: u32 = 900_000;
/// Domain tag of [`SnapshotRecord::hash`].
pub const SNAPSHOT_DOMAIN: &str = "onyx-roster-snapshot-v1";
/// The accepted ways a snapshot's height was found
/// ([`SnapshotRecord::height_basis`]): stated by the issuer, mapped from the
/// issuer's date by an indexer, the chain tip when the bytes were fetched, or
/// a bound derived otherwise.
pub const HEIGHT_BASES: [&str; 4] = ["issuer", "indexer-mapped", "fetch-tip", "bound"];

/// How strongly a snapshot ties a listed address to the issuer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, BorshSerialize, BorshDeserialize)]
pub enum Grade {
    /// Listed by the issuer, no verified signature.
    Published,
    /// A signature by the address key verified for that row in that snapshot.
    Signed,
}

impl Grade {
    fn confidence(self) -> Confidence {
        Confidence(match self {
            Grade::Published => CONFIDENCE_PUBLISHED,
            Grade::Signed => CONFIDENCE_SIGNED,
        })
    }

    fn evidence_kind(self) -> &'static str {
        match self {
            Grade::Published => EVIDENCE_PUBLISHED,
            Grade::Signed => EVIDENCE_SIGNED,
        }
    }
}

/// One dated publication of an issuer's roster. Its hash is what each
/// membership's evidence cites.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SnapshotRecord {
    /// Dotted id of the source, e.g. `okx.por`.
    pub source: String,
    /// Entity id, e.g. `entity:okx`; must be the roster's.
    pub entity: String,
    /// Where the issuer serves the snapshot.
    pub url: String,
    /// The issuer's date of the snapshot, `YYYY-MM-DD`.
    pub issued: String,
    /// Date the bytes were fetched, `YYYY-MM-DD`.
    pub fetched: String,
    /// First block height at which the listing holds.
    pub height: u64,
    /// How the height was found: one of [`HEIGHT_BASES`].
    pub height_basis: String,
    /// SHA-256 of the raw snapshot bytes as served.
    pub raw_sha256: [u8; 32],
    /// Keccak-256 of the same bytes.
    pub raw_keccak: Hash32,
}

/// The hashed form of a record: the domain tag, then the record.
#[derive(BorshSerialize)]
struct SnapshotCommit<'a> {
    domain: &'a str,
    record: &'a SnapshotRecord,
}

impl SnapshotRecord {
    /// `keccak256(Borsh((SNAPSHOT_DOMAIN, self)))`.
    pub fn hash(&self) -> Hash32 {
        let c = SnapshotCommit {
            domain: SNAPSHOT_DOMAIN,
            record: self,
        };
        keccak256(&to_canonical_bytes(&c).unwrap_or_default())
    }

    /// Shape checks: dotted source id, both dates `YYYY-MM-DD`, known basis.
    pub fn check_shape(&self) -> Result<(), RosterError> {
        let at = || format!("{}@{}", self.source, self.height);
        if !is_dotted_id(&self.source) {
            return Err(RosterError::BadRecord(format!("{}: bad source id", at())));
        }
        if !is_date(&self.issued) || !is_date(&self.fetched) {
            return Err(RosterError::BadRecord(format!(
                "{}: dates must be YYYY-MM-DD",
                at()
            )));
        }
        if !HEIGHT_BASES.contains(&self.height_basis.as_str()) {
            return Err(RosterError::BadRecord(format!(
                "{}: unknown height basis {:?}",
                at(),
                self.height_basis
            )));
        }
        Ok(())
    }
}

/// `YYYY-MM-DD` shape only: ten bytes, dashes at 4 and 7, digits elsewhere.
fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

/// One snapshot with the addresses it lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterSnapshot {
    /// The dated publication.
    pub record: SnapshotRecord,
    /// Listed addresses with their grade, strictly ascending by address.
    pub listed: Vec<(String, Grade)>,
}

/// Every snapshot of one issuer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roster {
    /// Entity id of the issuer.
    pub entity: String,
    /// Entity kind of the issuer.
    pub kind: EntityKind,
    /// Snapshots, strictly ascending by `record.height`.
    pub snapshots: Vec<RosterSnapshot>,
}

/// Why an address was left out of the derived set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropCause {
    /// The address is on the deny list.
    Denied,
    /// Two entities list the address at an overlapping height.
    TwoEntities,
}

/// An address left out of the derived set, with every entity that listed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dropped {
    /// The address.
    pub address: String,
    /// Why it was dropped.
    pub cause: DropCause,
    /// Entities that listed it, sorted, unique.
    pub entities: Vec<String>,
}

/// The result of [`derive`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Derived {
    /// The dated set.
    pub set: DatedLabelSet,
    /// Addresses left out, sorted by address.
    pub dropped: Vec<Dropped>,
}

/// Roster derivation error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RosterError {
    /// A malformed record, entity id, or a record of another entity.
    #[error("bad snapshot record: {0}")]
    BadRecord(String),
    /// Snapshot heights or a listing not strictly ascending.
    #[error("not strictly ascending: {0}")]
    NotAscending(String),
    /// A snapshot height after the set's publication height.
    #[error("snapshot after the publication: {0}")]
    AfterPublication(String),
    /// Two rosters for one entity.
    #[error("two rosters for {0}")]
    DuplicateEntity(String),
    /// `max_age` is zero: no listing would support even its own height.
    #[error("max_age must be at least 1")]
    ZeroMaxAge,
    /// The derived set fails [`DatedLabelSet::check_structure`] (a bug).
    #[error("derived set: {0}")]
    Dated(#[from] DatedError),
}

/// Check one roster's records, heights and listings.
fn check_roster(r: &Roster, published: u64) -> Result<(), RosterError> {
    if !is_entity_id(&r.entity) {
        return Err(RosterError::BadRecord(format!(
            "bad entity id {:?}",
            r.entity
        )));
    }
    for s in &r.snapshots {
        let rec = &s.record;
        rec.check_shape()?;
        if rec.entity != r.entity {
            return Err(RosterError::BadRecord(format!(
                "{}@{}: entity {} in the roster of {}",
                rec.source, rec.height, rec.entity, r.entity
            )));
        }
        if rec.height > published {
            return Err(RosterError::AfterPublication(format!(
                "{}@{} > {published}",
                rec.source, rec.height
            )));
        }
        if let Some(w) = s.listed.windows(2).find(|w| w[0].0 >= w[1].0) {
            return Err(RosterError::NotAscending(format!(
                "{}@{}: listing at {}",
                rec.source, rec.height, w[1].0
            )));
        }
    }
    if let Some(w) = r
        .snapshots
        .windows(2)
        .find(|w| w[0].record.height >= w[1].record.height)
    {
        return Err(RosterError::NotAscending(format!(
            "{}: snapshot height {} after {}",
            r.entity, w[1].record.height, w[0].record.height
        )));
    }
    Ok(())
}

/// A range being walked: start, grade, first and last record, last listing.
struct Open<'a> {
    start: u64,
    grade: Grade,
    first: &'a SnapshotRecord,
    last: &'a SnapshotRecord,
    listed_at: u64,
}

/// The memberships of one roster, by the rule in the module doc.
fn roster_memberships(r: &Roster, published: u64, max_age: u64) -> Vec<Membership> {
    // Last height a listing at `l` supports.
    let expiry = |l: u64| l.saturating_add(max_age - 1);
    // address -> (snapshot index, grade), indices ascending.
    let mut by_addr: BTreeMap<&str, Vec<(usize, Grade)>> = BTreeMap::new();
    for (i, s) in r.snapshots.iter().enumerate() {
        for (a, g) in &s.listed {
            by_addr.entry(a.as_str()).or_default().push((i, *g));
        }
    }
    let make = |address: &str, o: &Open, to: Option<u64>| {
        debug_assert!(to.is_none_or(|t| t >= o.start), "range ends before start");
        let kind = o.grade.evidence_kind().to_string();
        let mut evidence = vec![EvidenceRef {
            kind: kind.clone(),
            content_hash: o.first.hash(),
        }];
        let last = o.last.hash();
        if last != evidence[0].content_hash {
            evidence.push(EvidenceRef {
                kind,
                content_hash: last,
            });
        }
        evidence.sort();
        Membership {
            label: Label {
                address: address.to_string(),
                entity: r.entity.clone(),
                kind: r.kind,
                confidence: o.grade.confidence(),
                evidence,
            },
            from: o.start,
            to,
        }
    };
    let mut out = Vec::new();
    for (address, hits) in &by_addr {
        let mut hits = hits.iter().peekable();
        let mut open: Option<Open> = None;
        for (i, s) in r.snapshots.iter().enumerate() {
            let h = s.record.height;
            let listed = hits.next_if(|(j, _)| *j == i).map(|(_, g)| *g);
            match (listed, open.as_mut()) {
                (Some(g), Some(o)) if o.grade == g && h <= expiry(o.listed_at) => {
                    o.last = &s.record;
                    o.listed_at = h;
                }
                (Some(g), _) => {
                    if let Some(o) = open.take() {
                        let end = (h - 1).min(expiry(o.listed_at));
                        out.push(make(address, &o, Some(end)));
                    }
                    open = Some(Open {
                        start: h,
                        grade: g,
                        first: &s.record,
                        last: &s.record,
                        listed_at: h,
                    });
                }
                (None, Some(_)) => {
                    if let Some(o) = open.take() {
                        let end = (h - 1).min(expiry(o.listed_at));
                        out.push(make(address, &o, Some(end)));
                    }
                }
                (None, None) => {}
            }
        }
        if let Some(o) = open {
            let end = expiry(o.listed_at);
            let to = if end >= published { None } else { Some(end) };
            out.push(make(address, &o, to));
        }
    }
    out
}

/// True if two memberships hold at some common height.
fn overlap(a: &Membership, b: &Membership) -> bool {
    a.to.is_none_or(|t| b.from <= t) && b.to.is_none_or(|t| a.from <= t)
}

/// Derive the dated set `name` version `version`, published at `published`,
/// from the issuers' rosters, dropping the addresses in `deny` and those two
/// entities claim at once (rule in the module doc). The result does not
/// depend on the order of `rosters` or `deny`.
pub fn derive(
    name: &str,
    version: u32,
    published: u64,
    max_age: u64,
    rosters: &[Roster],
    deny: &[String],
) -> Result<Derived, RosterError> {
    if max_age == 0 {
        return Err(RosterError::ZeroMaxAge);
    }
    let mut entities: Vec<&str> = rosters.iter().map(|r| r.entity.as_str()).collect();
    entities.sort_unstable();
    if let Some(w) = entities.windows(2).find(|w| w[0] == w[1]) {
        return Err(RosterError::DuplicateEntity(w[0].to_string()));
    }
    for r in rosters {
        check_roster(r, published)?;
    }
    let mut all: Vec<Membership> = rosters
        .iter()
        .flat_map(|r| roster_memberships(r, published, max_age))
        .collect();
    all.sort_by(|a, b| {
        (a.label.address.as_str(), a.from, a.label.entity.as_str()).cmp(&(
            b.label.address.as_str(),
            b.from,
            b.label.entity.as_str(),
        ))
    });
    let mut deny: Vec<&str> = deny.iter().map(String::as_str).collect();
    deny.sort_unstable();
    let mut members = Vec::with_capacity(all.len());
    let mut dropped = Vec::new();
    for group in all.chunk_by(|a, b| a.label.address == b.label.address) {
        let address = &group[0].label.address;
        let mut ents: Vec<String> = group.iter().map(|m| m.label.entity.clone()).collect();
        ents.sort_unstable();
        ents.dedup();
        let cause = if deny.binary_search(&address.as_str()).is_ok() {
            Some(DropCause::Denied)
        } else if group.iter().enumerate().any(|(i, a)| {
            group[i + 1..]
                .iter()
                .any(|b| a.label.entity != b.label.entity && overlap(a, b))
        }) {
            Some(DropCause::TwoEntities)
        } else {
            None
        };
        match cause {
            Some(cause) => dropped.push(Dropped {
                address: address.clone(),
                cause,
                entities: ents,
            }),
            None => members.extend_from_slice(group),
        }
    }
    let set = DatedLabelSet {
        name: name.to_string(),
        version,
        published,
        members,
    };
    set.check_structure()?;
    Ok(Derived { set, dropped })
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_core::evidence_is_canonical;
    use Grade::{Published as P, Signed as S};

    fn rec(entity: &str, height: u64) -> SnapshotRecord {
        SnapshotRecord {
            source: "test.por".into(),
            entity: entity.into(),
            url: format!("https://example.test/por/{height}"),
            issued: "2026-01-01".into(),
            fetched: "2026-01-02".into(),
            height,
            height_basis: "issuer".into(),
            raw_sha256: [7; 32],
            raw_keccak: keccak256(&height.to_le_bytes()),
        }
    }

    fn snap(entity: &str, height: u64, listed: &[(&str, Grade)]) -> RosterSnapshot {
        RosterSnapshot {
            record: rec(entity, height),
            listed: listed.iter().map(|(a, g)| (a.to_string(), *g)).collect(),
        }
    }

    /// A roster of `entity` from `(height, listing)` pairs.
    fn roster(entity: &str, snaps: &[(u64, &[(&str, Grade)])]) -> Roster {
        Roster {
            entity: entity.into(),
            kind: EntityKind::Exchange,
            snapshots: snaps.iter().map(|(h, l)| snap(entity, *h, l)).collect(),
        }
    }

    fn run(published: u64, rosters: &[Roster]) -> Result<Derived, RosterError> {
        derive("onyx.test", 1, published, 100, rosters, &[])
    }

    /// `(address, entity, from, to)` of every member.
    fn ranges(d: &Derived) -> Vec<(String, String, u64, Option<u64>)> {
        d.set
            .members
            .iter()
            .map(|m| {
                (
                    m.label.address.clone(),
                    m.label.entity.clone(),
                    m.from,
                    m.to,
                )
            })
            .collect()
    }

    fn r(
        addr: &str,
        entity: &str,
        from: u64,
        to: Option<u64>,
    ) -> (String, String, u64, Option<u64>) {
        (addr.into(), entity.into(), from, to)
    }

    fn ev(grade: Grade, recs: &[SnapshotRecord]) -> Vec<EvidenceRef> {
        let mut e: Vec<EvidenceRef> = recs
            .iter()
            .map(|r| EvidenceRef {
                kind: grade.evidence_kind().into(),
                content_hash: r.hash(),
            })
            .collect();
        e.sort();
        e
    }

    const A: &str = "entity:a";
    const B: &str = "entity:b";

    #[test]
    fn listed_in_every_snapshot_gives_one_open_range_citing_first_and_last() {
        let ro = roster(
            A,
            &[
                (100, &[("t:a", P)]),
                (150, &[("t:a", P)]),
                (190, &[("t:a", P)]),
            ],
        );
        let d = run(250, &[ro]).expect("ok");
        assert_eq!(ranges(&d), vec![r("t:a", A, 100, None)]);
        let l = &d.set.members[0].label;
        assert_eq!(l.evidence, ev(P, &[rec(A, 100), rec(A, 190)]));
        assert!(evidence_is_canonical(&l.evidence));
        assert_eq!(l.confidence, Confidence(CONFIDENCE_PUBLISHED));
        assert_eq!(l.kind, EntityKind::Exchange);
        assert!(d.dropped.is_empty());
        // One snapshot: one evidence entry.
        let d = run(150, &[roster(A, &[(100, &[("t:a", S)])])]).expect("ok");
        assert_eq!(d.set.members[0].label.evidence, ev(S, &[rec(A, 100)]));
    }

    #[test]
    fn an_omitted_address_ends_the_block_before_the_omitting_snapshot() {
        let ro = roster(
            A,
            &[
                (100, &[("t:a", P), ("t:b", P)]),
                (150, &[("t:a", P), ("t:b", P)]),
                (180, &[("t:b", P)]),
            ],
        );
        let d = run(200, &[ro]).expect("ok");
        assert_eq!(
            ranges(&d),
            vec![r("t:a", A, 100, Some(179)), r("t:b", A, 100, None)]
        );
        assert_eq!(
            d.set.members[0].label.evidence,
            ev(P, &[rec(A, 100), rec(A, 150)])
        );
    }

    #[test]
    fn a_gap_longer_than_max_age_splits_the_range_at_expiry() {
        // 300 > 100 + 99: the first listing expired at 199.
        let ro = roster(A, &[(100, &[("t:a", P)]), (300, &[("t:a", P)])]);
        let d = run(350, &[ro]).expect("ok");
        assert_eq!(
            ranges(&d),
            vec![r("t:a", A, 100, Some(199)), r("t:a", A, 300, None)]
        );
        // Exactly at expiry (199 = 100 + 99) the listing still extends.
        let ro = roster(A, &[(100, &[("t:a", P)]), (199, &[("t:a", P)])]);
        assert_eq!(
            ranges(&run(250, &[ro]).expect("ok")),
            vec![r("t:a", A, 100, None)]
        );
        // One block later: a new, adjacent range (other evidence, so not a split).
        let ro = roster(A, &[(100, &[("t:a", P)]), (200, &[("t:a", P)])]);
        let d = run(250, &[ro]).expect("ok");
        assert_eq!(
            ranges(&d),
            vec![r("t:a", A, 100, Some(199)), r("t:a", A, 200, None)]
        );
        // An omission after expiry closes at expiry, not at the omission.
        let ro = roster(A, &[(100, &[("t:a", P)]), (400, &[("t:b", P)])]);
        let d = run(450, &[ro]).expect("ok");
        assert_eq!(
            ranges(&d),
            vec![r("t:a", A, 100, Some(199)), r("t:b", A, 400, None)]
        );
    }

    #[test]
    fn a_stale_last_listing_closes_at_expiry_with_both_edges() {
        let ro = || roster(A, &[(100, &[("t:a", P)])]);
        // E = 199 == published - 1: closed.
        assert_eq!(
            ranges(&run(200, &[ro()]).expect("ok")),
            vec![r("t:a", A, 100, Some(199))]
        );
        // E = 199 == published: open.
        assert_eq!(
            ranges(&run(199, &[ro()]).expect("ok")),
            vec![r("t:a", A, 100, None)]
        );
        assert_eq!(
            ranges(&run(10_000, &[ro()]).expect("ok")),
            vec![r("t:a", A, 100, Some(199))]
        );
    }

    #[test]
    fn a_relisted_address_starts_again_at_its_snapshot() {
        let ro = roster(A, &[(100, &[("t:a", P)]), (150, &[]), (200, &[("t:a", P)])]);
        let d = run(250, &[ro]).expect("ok");
        assert_eq!(
            ranges(&d),
            vec![r("t:a", A, 100, Some(149)), r("t:a", A, 200, None)]
        );
        assert_eq!(d.set.members[1].label.evidence, ev(P, &[rec(A, 200)]));
    }

    #[test]
    fn a_grade_change_gives_adjacent_ranges_with_different_labels() {
        let ro = roster(
            A,
            &[
                (100, &[("t:a", P)]),
                (150, &[("t:a", S)]),
                (180, &[("t:a", S)]),
            ],
        );
        let d = run(200, &[ro]).expect("ok");
        assert_eq!(
            ranges(&d),
            vec![r("t:a", A, 100, Some(149)), r("t:a", A, 150, None)]
        );
        let (p, s) = (&d.set.members[0].label, &d.set.members[1].label);
        assert_eq!(p.confidence, Confidence(CONFIDENCE_PUBLISHED));
        assert_eq!(s.confidence, Confidence(CONFIDENCE_SIGNED));
        assert_eq!(p.evidence, ev(P, &[rec(A, 100)]));
        assert_eq!(s.evidence, ev(S, &[rec(A, 150), rec(A, 180)]));
        assert_eq!(d.set.check_structure(), Ok(()));
        // And back down to Published.
        let ro = roster(A, &[(100, &[("t:a", S)]), (150, &[("t:a", P)])]);
        let d = run(200, &[ro]).expect("ok");
        assert_eq!(
            ranges(&d),
            vec![r("t:a", A, 100, Some(149)), r("t:a", A, 150, None)]
        );
        assert_eq!(
            d.set.members[1].label.confidence,
            Confidence(CONFIDENCE_PUBLISHED)
        );
    }

    #[test]
    fn two_entities_at_once_drop_the_address_but_a_move_is_kept() {
        let a = roster(A, &[(100, &[("t:x", P), ("t:y", P)])]);
        let b = roster(B, &[(150, &[("t:x", S)])]);
        let d = run(160, &[a, b]).expect("ok");
        assert_eq!(ranges(&d), vec![r("t:y", A, 100, None)]);
        assert_eq!(
            d.dropped,
            vec![Dropped {
                address: "t:x".into(),
                cause: DropCause::TwoEntities,
                entities: vec![A.into(), B.into()],
            }]
        );
        // A leaves before B lists it: kept, with a gap or adjacent.
        for b_at in [200, 150] {
            let a = roster(A, &[(100, &[("t:x", P)]), (150, &[])]);
            let b = roster(B, &[(b_at, &[("t:x", P)])]);
            let d = run(240, &[b, a]).expect("ok");
            assert_eq!(
                ranges(&d),
                vec![r("t:x", A, 100, Some(149)), r("t:x", B, b_at, None)]
            );
            assert!(d.dropped.is_empty());
        }
        // A's listing expired (no omission) before B lists it: kept.
        let a = roster(A, &[(100, &[("t:x", P)])]);
        let b = roster(B, &[(200, &[("t:x", P)])]);
        let d = run(250, &[a, b]).expect("ok");
        assert_eq!(
            ranges(&d),
            vec![r("t:x", A, 100, Some(199)), r("t:x", B, 200, None)]
        );
    }

    #[test]
    fn a_denied_address_is_dropped_and_denial_wins() {
        let a = roster(A, &[(100, &[("t:x", P), ("t:y", P)])]);
        let b = roster(B, &[(120, &[("t:z", P)])]);
        let deny = vec!["t:x".to_string(), "t:absent".to_string()];
        let d = derive("onyx.test", 1, 150, 100, &[a.clone(), b], &deny).expect("ok");
        assert_eq!(
            ranges(&d),
            vec![r("t:y", A, 100, None), r("t:z", B, 120, None)]
        );
        assert_eq!(
            d.dropped,
            vec![Dropped {
                address: "t:x".into(),
                cause: DropCause::Denied,
                entities: vec![A.into()],
            }]
        );
        // Denied and claimed by two entities: one row, Denied.
        let b = roster(B, &[(120, &[("t:x", P)])]);
        let d = derive("onyx.test", 1, 150, 100, &[a, b], &deny).expect("ok");
        assert_eq!(
            d.dropped,
            vec![Dropped {
                address: "t:x".into(),
                cause: DropCause::Denied,
                entities: vec![A.into(), B.into()],
            }]
        );
    }

    #[test]
    fn malformed_rosters_are_refused() {
        let ok = || roster(A, &[(100, &[("t:a", P)]), (150, &[("t:a", P)])]);
        assert!(run(200, &[ok()]).is_ok());
        let not_asc =
            |e: Result<Derived, RosterError>| matches!(e, Err(RosterError::NotAscending(_)));
        let bad = |e: Result<Derived, RosterError>| matches!(e, Err(RosterError::BadRecord(_)));
        // Heights descending or equal.
        let mut x = ok();
        x.snapshots.swap(0, 1);
        assert!(not_asc(run(200, &[x])));
        let mut x = ok();
        x.snapshots[1].record.height = 100;
        assert!(not_asc(run(200, &[x])));
        // Listing unsorted or duplicated.
        let x = roster(A, &[(100, &[("t:b", P), ("t:a", P)])]);
        assert!(not_asc(run(200, &[x])));
        let x = roster(A, &[(100, &[("t:a", P), ("t:a", S)])]);
        assert!(not_asc(run(200, &[x])));
        // A snapshot after the publication.
        assert!(matches!(
            run(149, &[ok()]),
            Err(RosterError::AfterPublication(_))
        ));
        // A record of another entity.
        let mut x = ok();
        x.snapshots[0].record.entity = B.into();
        assert!(bad(run(200, &[x])));
        // Bad date shapes.
        for d in [
            "2026-1-01",
            "2026/01/01",
            "20260101xx",
            "2026-01-011",
            "abcd-ef-gh",
        ] {
            let mut x = ok();
            x.snapshots[0].record.issued = d.into();
            assert!(bad(run(200, &[x])), "issued {d}");
            let mut x = ok();
            x.snapshots[1].record.fetched = d.into();
            assert!(bad(run(200, &[x])), "fetched {d}");
        }
        // Bad basis, bad source id, bad entity id.
        let mut x = ok();
        x.snapshots[0].record.height_basis = "guess".into();
        assert!(bad(run(200, &[x])));
        for b in HEIGHT_BASES {
            let mut x = ok();
            x.snapshots[0].record.height_basis = b.into();
            assert!(run(200, &[x]).is_ok(), "basis {b}");
        }
        let mut x = ok();
        x.snapshots[0].record.source = "Test.POR".into();
        assert!(bad(run(200, &[x])));
        let x = roster("okx", &[(100, &[("t:a", P)])]);
        assert!(bad(run(200, &[x])));
        // Two rosters for one entity.
        assert_eq!(
            run(200, &[ok(), ok()]),
            Err(RosterError::DuplicateEntity(A.into()))
        );
        // max_age 0, and a bad set header reaches check_structure.
        assert_eq!(
            derive("onyx.test", 1, 200, 0, &[ok()], &[]),
            Err(RosterError::ZeroMaxAge)
        );
        assert_eq!(
            derive("onyx.test", 0, 200, 100, &[ok()], &[]),
            Err(RosterError::Dated(DatedError::BadHeader))
        );
    }

    #[test]
    fn derivation_is_deterministic_and_commits_the_records() {
        let a = roster(A, &[(100, &[("t:a", P), ("t:x", S)]), (150, &[("t:a", S)])]);
        let b = roster(B, &[(120, &[("t:b", P)]), (140, &[("t:b", P), ("t:x", P)])]);
        let h = run(200, &[a.clone(), b.clone()])
            .expect("ok")
            .set
            .content_hash();
        assert_eq!(
            run(200, &[a.clone(), b.clone()])
                .expect("ok")
                .set
                .content_hash(),
            h
        );
        assert_eq!(
            run(200, &[b.clone(), a.clone()]).expect("ok"),
            run(200, &[a.clone(), b.clone()]).expect("ok"),
            "roster order does not matter"
        );
        let base = rec(A, 100);
        let changed: [fn(&mut SnapshotRecord); 5] = [
            |r| r.url.push('x'),
            |r| r.fetched = "2026-01-03".into(),
            |r| r.raw_sha256[0] ^= 1,
            |r| r.raw_keccak.0[0] ^= 1,
            |r| r.height_basis = "bound".into(),
        ];
        for f in changed {
            let mut r = base.clone();
            f(&mut r);
            assert_ne!(r.hash(), base.hash());
        }
        // The evidence commits the record: another url, another set.
        let mut a2 = a.clone();
        a2.snapshots[0].record.url.push('x');
        assert_ne!(run(200, &[a2, b]).expect("ok").set.content_hash(), h);
    }

    #[test]
    fn monthly_publications_survive_one_missed_month() {
        assert_eq!(MAX_LISTING_AGE, 62 * 144);
        let month = 30 * 144;
        let at = |h: u64| (h, [("t:a", P)].as_slice());
        // Months 0 and 2 (month 1 missed): one range.
        let ro = roster(A, &[at(1_000), at(1_000 + 2 * month)]);
        let p = 1_000 + 2 * month + 10;
        let d = derive("onyx.test", 1, p, MAX_LISTING_AGE, &[ro], &[]).expect("ok");
        assert_eq!(ranges(&d), vec![r("t:a", A, 1_000, None)]);
        // Months 0 and 3 (two missed): the first expires after 62 days.
        let ro = roster(A, &[at(1_000), at(1_000 + 3 * month)]);
        let p = 1_000 + 3 * month + 10;
        let d = derive("onyx.test", 1, p, MAX_LISTING_AGE, &[ro], &[]).expect("ok");
        assert_eq!(
            ranges(&d),
            vec![
                r("t:a", A, 1_000, Some(1_000 + MAX_LISTING_AGE - 1)),
                r("t:a", A, 1_000 + 3 * month, None)
            ]
        );
    }

    /// Review P2a-R2-4, residual pinned (TODO D25): a membership cites its last
    /// snapshot, so the next version derived by the same rule from one more
    /// snapshot that relists an address open at the previous publication
    /// changes that membership's evidence below the publication, and
    /// `check_successor` refuses it as back-dated. Omitting the address, or
    /// relisting it only after its listing expired, is a valid successor. A
    /// successor needs `append_dated` (D25): close the open range at the old
    /// publication's height or carry its evidence, then derive from there.
    /// This test flips when D25 lands.
    #[test]
    fn p2a_r2_4_a_relisting_successor_is_refused_until_d25() {
        let a = [("t:a", P)];
        let v = |version: u32, published: u64, snaps: &[(u64, &[(&str, Grade)])]| {
            derive(
                "onyx.test",
                version,
                published,
                100,
                &[roster(A, snaps)],
                &[],
            )
            .expect("derives")
            .set
        };
        let v1 = v(1, 150, &[(100, &a)]);
        assert_eq!(v1.members[0].to, None, "open at its publication");
        let relist = v(2, 250, &[(100, &a), (180, &a)]);
        assert!(matches!(
            relist.check_successor(&v1),
            Err(DatedError::BackDated { .. })
        ));
        let omit = v(2, 250, &[(100, &a), (180, &[])]);
        assert_eq!(omit.check_successor(&v1), Ok(()));
        let after_expiry = v(2, 250, &[(100, &a), (220, &a)]);
        assert_eq!(after_expiry.check_successor(&v1), Ok(()));
    }

    /// Pinned: any change of the record encoding or domain changes this hash.
    #[test]
    fn snapshot_record_hash_vector() {
        let r = SnapshotRecord {
            source: "okx.por".into(),
            entity: "entity:okx".into(),
            url: "https://example.test/okx/por-2026-09.csv".into(),
            issued: "2026-09-30".into(),
            fetched: "2026-10-01".into(),
            height: 917_000,
            height_basis: "indexer-mapped".into(),
            raw_sha256: [0x11; 32],
            raw_keccak: Hash32([0x22; 32]),
        };
        assert_eq!(
            r.hash(),
            keccak256(&borsh::to_vec(&(SNAPSHOT_DOMAIN, &r)).expect("ok")),
            "the domain string, then the record"
        );
        assert_eq!(
            hex::encode(r.hash().0),
            "8958ba7b5673bc98dd6b5a810bdf6ccf822d34ace025331568526c5b52014454"
        );
    }
}
