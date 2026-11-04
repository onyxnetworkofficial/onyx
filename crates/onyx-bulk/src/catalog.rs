//! Catalogs (ADR-0059 amendment 1): one signed report that commits to the
//! newest manifest of each of a publisher's datasets, so one `postReport`
//! anchors any number of them.
//!
//! A leaf is the row `"onyx-bulk-catalog-leaf-v1" ‖ borsh(publisher, dataset,
//! version, previous manifest root, manifest root)`; leaves are sorted
//! strictly by dataset id and committed by the ADR-0003 Merkle root over rows
//! (a path binds its index and the leaf count). The catalog root is
//! `keccak256("onyx-bulk-catalog-v1" ‖ borsh(header))`, and the publisher signs
//! it in the level-3 scheme (ADR-0028) under a sequence fixed by the catalog
//! number: two catalogs with one number under one key are an equivocation the
//! book records on chain.
//!
//! Between catalogs, append only ([`Catalog::check_successor`]): every dataset
//! stays, and each moves by at most one version whose leaf names the previous
//! version's root.

use crate::anchor::{key_id, AnchorError};
use crate::manifest::ManifestHeader;
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::ids::is_dotted_id;
use onyx_core::{
    keccak256, Hash32, MetricId, SignedPayload, SignedReport, SourceId, Subject, Value,
};
use onyx_labels::attest::{eip191_recover, eip191_sign_with, secret_address};
use onyx_merkle::{MerkleTree, Proof};

/// Domain tag of [`CatalogHeader::root`].
pub const CATALOG_DOMAIN: &[u8] = b"onyx-bulk-catalog-v1";
/// Domain tag of a leaf row ([`CatalogLeaf::row`]).
pub const LEAF_DOMAIN: &[u8] = b"onyx-bulk-catalog-leaf-v1";
/// Domain tag of [`sequence_for_catalog`].
pub const CATALOG_SEQUENCE_DOMAIN: &[u8] = b"onyx-bulk-catalog-sequence-v1";
/// The metric every catalog anchor uses.
pub const CATALOG_METRIC: &str = "bulk.catalog";
/// The subject every catalog anchor uses (`Aggregate("catalog")`).
pub const CATALOG_SUBJECT: &str = "catalog";
/// The one catalog format this crate reads.
pub const FORMAT: u8 = 1;
/// Most leaves in one catalog (ADR-0059 amendment 1 item 1; 10,000 until
/// THREATS CAT-4: the first publisher reached 9,044).
pub const MAX_CATALOG_LEAVES: usize = 50_000;
/// Largest Borsh catalog (header and every leaf), for request limits: about
/// 120 bytes a leaf, so 50,000 leaves fit with room for long ids.
pub const MAX_CATALOG_BYTES: usize = 8 << 20;

/// One dataset's newest manifest, as a catalog lists it.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CatalogLeaf {
    /// The publisher (equal to the catalog's; review CAT-R1 H2).
    pub publisher: SourceId,
    /// Dataset id.
    pub dataset: String,
    /// The manifest's version.
    pub version: u32,
    /// The manifest's previous root (zero for version 1; review CAT-R1 H1).
    pub prev: Hash32,
    /// The manifest root.
    pub root: Hash32,
}

impl CatalogLeaf {
    /// The leaf of a manifest header.
    pub fn of(h: &ManifestHeader) -> Self {
        CatalogLeaf {
            publisher: h.publisher.clone(),
            dataset: h.dataset.clone(),
            version: h.version,
            prev: h.prev,
            root: h.root(),
        }
    }

    /// Whether this leaf names exactly `h` (publisher, dataset, version,
    /// previous root and root).
    pub fn names(&self, h: &ManifestHeader) -> bool {
        *self == CatalogLeaf::of(h)
    }

    /// The leaf's row in the catalog tree.
    pub fn row(&self) -> Vec<u8> {
        let mut b = LEAF_DOMAIN.to_vec();
        b.extend_from_slice(&borsh::to_vec(self).unwrap_or_default());
        b
    }
}

/// What the publisher signs (through its root).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CatalogHeader {
    /// [`FORMAT`].
    pub format: u8,
    /// The publisher.
    pub publisher: SourceId,
    /// 0, 1, 2, ...
    pub number: u64,
    /// Root of the previous catalog; zero for number 0.
    pub prev: Hash32,
    /// When the publisher made it (unix seconds); the anchor's observation
    /// time, strictly later than the previous catalog's (review CAT-R1 M3).
    pub created_unix: u64,
    /// Leaves listed.
    pub leaf_count: u32,
    /// Merkle root over the leaves' rows.
    pub leaves_root: Hash32,
}

impl CatalogHeader {
    /// `keccak256("onyx-bulk-catalog-v1" ‖ borsh(header))`.
    pub fn root(&self) -> Hash32 {
        let mut b = CATALOG_DOMAIN.to_vec();
        b.extend_from_slice(&borsh::to_vec(self).unwrap_or_default());
        keccak256(&b)
    }

    /// Whether `lp` is a leaf of this catalog, and the only one for its
    /// dataset: its neighbours (at index - 1 and + 1, or none at the ends)
    /// bracket its dataset id (ADR-0059 amendment 1 item 6).
    pub fn proves(&self, lp: &LeafProof) -> Result<(), CatalogError> {
        let n = u64::from(self.leaf_count);
        let at = |leaf: &CatalogLeaf, p: &Proof, i: u64| {
            p.index == i
                && p.leaf_count == n
                && leaf.publisher == self.publisher
                && onyx_merkle::verify(&self.leaves_root, &leaf.row(), p)
        };
        let i = lp.proof.index;
        if !at(&lp.leaf, &lp.proof, i) {
            return Err(CatalogError::Proof("the leaf is not in the catalog"));
        }
        match (&lp.left, i) {
            (None, 0) => {}
            (Some((l, p)), i) if i > 0 => {
                if !at(l, p, i - 1) || l.dataset >= lp.leaf.dataset {
                    return Err(CatalogError::Proof("the left neighbour does not bracket"));
                }
            }
            _ => return Err(CatalogError::Proof("left neighbour missing or extra")),
        }
        match &lp.right {
            None if i + 1 == n => {}
            Some((r, p)) if i + 1 < n => {
                if !at(r, p, i + 1) || r.dataset <= lp.leaf.dataset {
                    return Err(CatalogError::Proof("the right neighbour does not bracket"));
                }
            }
            _ => return Err(CatalogError::Proof("right neighbour missing or extra")),
        }
        Ok(())
    }
}

/// A leaf with its path and its two neighbours.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct LeafProof {
    /// The leaf.
    pub leaf: CatalogLeaf,
    /// Its path.
    pub proof: Proof,
    /// The leaf before it, unless it is the first.
    pub left: Option<(CatalogLeaf, Proof)>,
    /// The leaf after it, unless it is the last.
    pub right: Option<(CatalogLeaf, Proof)>,
}

/// A catalog that does not hold.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    /// Bad shape.
    #[error("catalog: {0}")]
    Bad(String),
    /// Not an append-only successor of the previous catalog.
    #[error("not a successor: {0}")]
    Successor(String),
    /// A leaf path that does not hold.
    #[error("catalog proof: {0}")]
    Proof(&'static str),
    /// Encoding.
    #[error("catalog bytes do not decode")]
    Encoding,
}

fn bad(s: impl Into<String>) -> CatalogError {
    CatalogError::Bad(s.into())
}

/// A catalog: header and every leaf, sorted by dataset id.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Catalog {
    /// The signed part.
    pub header: CatalogHeader,
    /// Every leaf, sorted strictly by dataset id.
    pub leaves: Vec<CatalogLeaf>,
}

/// The tree over a leaf list.
pub fn leaves_tree(leaves: &[CatalogLeaf]) -> MerkleTree {
    let rows: Vec<Vec<u8>> = leaves.iter().map(CatalogLeaf::row).collect();
    MerkleTree::from_rows(&rows)
}

impl Catalog {
    /// Catalog `number` of `publisher` over `leaves` (sorted here), after
    /// `prev` (`None` for number 0). Checks the result ([`Catalog::validate`]
    /// and, after `prev`, [`Catalog::check_successor`]).
    pub fn build(
        publisher: SourceId,
        prev: Option<&Catalog>,
        created_unix: u64,
        mut leaves: Vec<CatalogLeaf>,
    ) -> Result<Catalog, CatalogError> {
        leaves.sort_by(|a, b| a.dataset.cmp(&b.dataset));
        let count = u32::try_from(leaves.len()).map_err(|_| bad("too many leaves"))?;
        let c = Catalog {
            header: CatalogHeader {
                format: FORMAT,
                publisher,
                number: prev.map_or(0, |p| p.header.number.saturating_add(1)),
                prev: prev.map_or(Hash32::ZERO, |p| p.header.root()),
                created_unix,
                leaf_count: count,
                leaves_root: leaves_tree(&leaves).root(),
            },
            leaves,
        };
        c.validate()?;
        if let Some(p) = prev {
            c.check_successor(p)?;
        }
        Ok(c)
    }

    /// Shape: format, a publisher, the leaf count and root, at most
    /// [`MAX_CATALOG_LEAVES`] leaves, sorted strictly by dataset id, each
    /// naming this publisher, a dotted dataset id, a version of at least 1,
    /// and a previous root that is zero exactly for version 1.
    pub fn validate(&self) -> Result<(), CatalogError> {
        let h = &self.header;
        if h.format != FORMAT {
            return Err(bad("format"));
        }
        if !is_dotted_id(&h.publisher.0) {
            return Err(bad("publisher id"));
        }
        if self.leaves.len() > MAX_CATALOG_LEAVES {
            return Err(bad(format!("more than {MAX_CATALOG_LEAVES} leaves")));
        }
        if usize::try_from(h.leaf_count).ok() != Some(self.leaves.len()) {
            return Err(bad("leaf count"));
        }
        if (h.number == 0) != (h.prev == Hash32::ZERO) {
            return Err(bad("previous catalog root (zero exactly for number 0)"));
        }
        for (i, l) in self.leaves.iter().enumerate() {
            if l.publisher != h.publisher {
                return Err(bad(format!("leaf {i} names another publisher")));
            }
            if !is_dotted_id(&l.dataset) {
                return Err(bad(format!("leaf {i}: dataset id")));
            }
            if l.version == 0 || (l.version == 1) != (l.prev == Hash32::ZERO) {
                return Err(bad(format!("leaf {i}: version and previous root")));
            }
            if i > 0 && self.leaves[i - 1].dataset >= l.dataset {
                return Err(bad(format!("leaf {i}: not sorted strictly by dataset")));
            }
        }
        if leaves_tree(&self.leaves).root() != h.leaves_root {
            return Err(bad("leaves root"));
        }
        Ok(())
    }

    /// The index of `dataset`'s leaf.
    pub fn find(&self, dataset: &str) -> Option<usize> {
        self.leaves
            .binary_search_by(|l| l.dataset.as_str().cmp(dataset))
            .ok()
    }

    /// The leaf at `i` with its path and its neighbours.
    pub fn leaf_proof(&self, i: usize) -> Option<LeafProof> {
        self.leaf_proof_in(&leaves_tree(&self.leaves), i)
    }

    /// [`Catalog::leaf_proof`] from the leaves' tree built once
    /// ([`leaves_tree`]); a tree of other leaves gives proofs that fail.
    pub fn leaf_proof_in(&self, tree: &MerkleTree, i: usize) -> Option<LeafProof> {
        let at = |j: usize| -> Option<(CatalogLeaf, Proof)> {
            Some((
                self.leaves.get(j)?.clone(),
                tree.proof(u64::try_from(j).ok()?)?,
            ))
        };
        let (leaf, proof) = at(i)?;
        Some(LeafProof {
            leaf,
            proof,
            left: i.checked_sub(1).and_then(at),
            right: at(i + 1),
        })
    }

    /// Append only (ADR-0059 amendment 1 item 3): this catalog is number
    /// `prev.number + 1`, names `prev`'s root, is strictly later, has the
    /// same publisher, keeps every dataset of `prev`, and moves each by at most
    /// one version whose leaf names the previous version's root. Datasets may
    /// be added (the node checks how they enter, item 3).
    pub fn check_successor(&self, prev: &Catalog) -> Result<(), CatalogError> {
        let (h, p) = (&self.header, &prev.header);
        let no = |s: String| Err(CatalogError::Successor(s));
        if h.publisher != p.publisher {
            return no("another publisher".into());
        }
        if Some(h.number) != p.number.checked_add(1) {
            return no(format!("number {} after {}", h.number, p.number));
        }
        if h.prev != p.root() {
            return no("does not name the previous catalog's root".into());
        }
        if h.created_unix <= p.created_unix {
            return no("not later than the previous catalog".into());
        }
        for old in &prev.leaves {
            let Some(i) = self.find(&old.dataset) else {
                return no(format!("dataset {} dropped", old.dataset));
            };
            let new = &self.leaves[i];
            let same = new == old;
            let next = Some(new.version) == old.version.checked_add(1) && new.prev == old.root;
            if !same && !next {
                return no(format!(
                    "dataset {}: version {} after {} (one step, naming the previous root)",
                    old.dataset, new.version, old.version
                ));
            }
        }
        Ok(())
    }
}

/// The one sequence catalog `number` is signed under: the first 8 bytes
/// (big-endian) of `keccak256("onyx-bulk-catalog-sequence-v1" ‖
/// borsh(number))`, never the reserved `u64::MAX`.
pub fn sequence_for_catalog(number: u64) -> u64 {
    let mut b = CATALOG_SEQUENCE_DOMAIN.to_vec();
    b.extend_from_slice(&borsh::to_vec(&number).unwrap_or_default());
    let h = keccak256(&b);
    let mut w = [0u8; 8];
    w.copy_from_slice(&h.0[..8]);
    u64::from_be_bytes(w).min(u64::MAX - 1)
}

/// The payload that anchors `header` for `signer`.
pub fn payload(header: &CatalogHeader, signer: &[u8; 20]) -> SignedPayload {
    SignedPayload {
        source: header.publisher.clone(),
        key: key_id(&header.publisher, signer),
        metric: MetricId(CATALOG_METRIC.into()),
        subject: Subject::Aggregate(CATALOG_SUBJECT.into()),
        value: Value::Bytes(header.root().0.to_vec()),
        observed_unix: header.created_unix,
        sequence: sequence_for_catalog(header.number),
    }
}

/// Sign `header` with `secret`.
pub fn sign(secret: &[u8; 32], header: &CatalogHeader) -> Result<SignedReport, AnchorError> {
    let signer = secret_address(secret).ok_or(AnchorError::BadSecret)?;
    let p = payload(header, &signer);
    let signature =
        eip191_sign_with(secret, p.message().as_bytes()).ok_or(AnchorError::BadSecret)?;
    Ok(SignedReport {
        payload: p,
        signature,
    })
}

/// Check that `report` signs exactly `header`'s root for its publisher under
/// the catalog's fixed sequence, and that the signer's key id is the
/// payload's key. Returns the signer (the book's record is the caller's chain
/// read).
pub fn verify(header: &CatalogHeader, report: &SignedReport) -> Result<[u8; 20], AnchorError> {
    let p = &report.payload;
    if p.source != header.publisher {
        return Err(AnchorError::Mismatch("source"));
    }
    if p.metric.0 != CATALOG_METRIC {
        return Err(AnchorError::Mismatch("metric"));
    }
    if p.subject != Subject::Aggregate(CATALOG_SUBJECT.into()) {
        return Err(AnchorError::Mismatch("subject"));
    }
    if p.value != Value::Bytes(header.root().0.to_vec()) {
        return Err(AnchorError::Mismatch("value (catalog root)"));
    }
    if p.observed_unix != header.created_unix {
        return Err(AnchorError::Mismatch("observed time"));
    }
    if p.sequence == u64::MAX {
        return Err(AnchorError::ReservedSequence);
    }
    if p.sequence != sequence_for_catalog(header.number) {
        return Err(AnchorError::Mismatch(
            "sequence (fixed by the catalog number)",
        ));
    }
    let signer = eip191_recover(p.message().as_bytes(), &report.signature)
        .ok_or(AnchorError::BadSignature)?;
    if key_id(&p.source, &signer) != p.key {
        return Err(AnchorError::WrongKey);
    }
    Ok(signer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anchor::sequence_for;

    fn src() -> SourceId {
        SourceId("test.publisher".into())
    }

    fn leaf(d: &str, v: u32, prev: u8, root: u8) -> CatalogLeaf {
        CatalogLeaf {
            publisher: src(),
            dataset: d.into(),
            version: v,
            prev: if prev == 0 {
                Hash32::ZERO
            } else {
                Hash32([prev; 32])
            },
            root: Hash32([root; 32]),
        }
    }

    fn secret() -> [u8; 32] {
        keccak256(b"onyx test catalog key").0
    }

    #[test]
    fn a_catalog_sorts_and_proves_each_leaf_with_its_neighbours() {
        let c = Catalog::build(
            src(),
            None,
            100,
            vec![
                leaf("c.x", 1, 0, 3),
                leaf("a.x", 1, 0, 1),
                leaf("b.x", 2, 9, 2),
            ],
        )
        .expect("catalog");
        assert_eq!(c.leaves[0].dataset, "a.x");
        assert_eq!(c.find("b.x"), Some(1));
        assert_eq!(c.find("z.x"), None);
        for i in 0..3 {
            let lp = c.leaf_proof(i).expect("proof");
            assert_eq!(c.header.proves(&lp), Ok(()), "leaf {i}");
        }
        // A neighbour left out, a wrong neighbour, a leaf from elsewhere.
        let mut lp = c.leaf_proof(1).expect("proof");
        lp.left = None;
        assert!(c.header.proves(&lp).is_err());
        let mut lp = c.leaf_proof(1).expect("proof");
        lp.right = c.leaf_proof(0).expect("p").left.or(lp.left.clone());
        assert!(c.header.proves(&lp).is_err());
        let mut lp = c.leaf_proof(1).expect("proof");
        lp.leaf.root = Hash32([7; 32]);
        assert!(c.header.proves(&lp).is_err());
        let mut lp = c.leaf_proof(2).expect("proof");
        lp.leaf.publisher = SourceId("other.publisher".into());
        assert!(c.header.proves(&lp).is_err());
    }

    #[test]
    fn a_catalog_refuses_bad_shapes() {
        // Two leaves for one dataset, another publisher, version 1 with a
        // previous root, version 2 without one.
        for leaves in [
            vec![leaf("a.x", 1, 0, 1), leaf("a.x", 2, 1, 2)],
            vec![CatalogLeaf {
                publisher: SourceId("other.publisher".into()),
                ..leaf("a.x", 1, 0, 1)
            }],
            vec![leaf("a.x", 1, 5, 1)],
            vec![leaf("a.x", 2, 0, 1)],
            vec![leaf("A b", 1, 0, 1)],
        ] {
            assert!(Catalog::build(src(), None, 1, leaves).is_err());
        }
        let mut c = Catalog::build(src(), None, 1, vec![leaf("a.x", 1, 0, 1)]).expect("ok");
        c.header.leaf_count = 2;
        assert!(c.validate().is_err());
        let mut c = Catalog::build(src(), None, 1, vec![leaf("a.x", 1, 0, 1)]).expect("ok");
        c.leaves[0].root = Hash32([9; 32]);
        assert_eq!(c.validate(), Err(bad("leaves root")));
        let too_many: Vec<_> = (0..=MAX_CATALOG_LEAVES)
            .map(|i| leaf(&format!("d.n{i:05}"), 1, 0, 1))
            .collect();
        assert!(Catalog::build(src(), None, 1, too_many).is_err());
    }

    /// THREATS CAT-4: a full catalog (MAX_CATALOG_LEAVES leaves of the first
    /// publisher's id and long dataset ids) builds, proves its last leaf and
    /// fits MAX_CATALOG_BYTES.
    #[test]
    fn cat_4_a_full_catalog_fits_its_byte_cap() {
        let p = SourceId("onchain-archive".into());
        let leaves: Vec<_> = (0..MAX_CATALOG_LEAVES)
            .map(|i| CatalogLeaf {
                publisher: p.clone(),
                dataset: format!("usdt-trx.entity-adjusted-realized-profit-loss-{i:06}"),
                version: 1,
                prev: Hash32::ZERO,
                root: Hash32([7; 32]),
            })
            .collect();
        let c = Catalog::build(p, None, 1, leaves).expect("a full catalog builds");
        let bytes = borsh::to_vec(&c).expect("encode");
        assert!(bytes.len() <= MAX_CATALOG_BYTES, "{} bytes", bytes.len());
        let lp = c.leaf_proof(MAX_CATALOG_LEAVES - 1).expect("proof");
        assert!(c.header.proves(&lp).is_ok());
    }

    #[test]
    fn successors_append_one_version_at_a_time() {
        let c0 = Catalog::build(
            src(),
            None,
            100,
            vec![leaf("a.x", 1, 0, 1), leaf("b.x", 1, 0, 2)],
        )
        .expect("c0");
        // Same leaf, one step naming the previous root, a new dataset.
        let c1 = Catalog::build(
            src(),
            Some(&c0),
            200,
            vec![
                leaf("a.x", 1, 0, 1),
                leaf("b.x", 2, 2, 3),
                leaf("c.x", 1, 0, 4),
            ],
        )
        .expect("c1");
        assert_eq!(c1.header.number, 1);
        assert_eq!(c1.header.prev, c0.header.root());
        // Refused: a dropped dataset, two steps, a step naming another root,
        // a rewrite of the same version, not later, a skipped number.
        for (t, leaves) in [
            (300, vec![leaf("a.x", 1, 0, 1), leaf("b.x", 2, 2, 3)]),
            (
                300,
                vec![
                    leaf("a.x", 3, 1, 5),
                    leaf("b.x", 2, 2, 3),
                    leaf("c.x", 1, 0, 4),
                ],
            ),
            (
                300,
                vec![
                    leaf("a.x", 2, 8, 5),
                    leaf("b.x", 2, 2, 3),
                    leaf("c.x", 1, 0, 4),
                ],
            ),
            (
                300,
                vec![
                    leaf("a.x", 1, 0, 6),
                    leaf("b.x", 2, 2, 3),
                    leaf("c.x", 1, 0, 4),
                ],
            ),
            (
                200,
                vec![
                    leaf("a.x", 1, 0, 1),
                    leaf("b.x", 2, 2, 3),
                    leaf("c.x", 1, 0, 4),
                ],
            ),
        ] {
            assert!(Catalog::build(src(), Some(&c1), t, leaves).is_err());
        }
        let mut skip = c1.clone();
        skip.header.number = 3;
        assert!(skip.check_successor(&c0).is_err());
    }

    #[test]
    fn a_catalog_is_signed_under_its_number_and_checked() {
        let c = Catalog::build(src(), None, 100, vec![leaf("a.x", 1, 0, 1)]).expect("c");
        let r = sign(&secret(), &c.header).expect("sign");
        let signer = verify(&c.header, &r).expect("verify");
        assert_eq!(r.payload.sequence, sequence_for_catalog(0));
        assert_eq!(r.payload.metric.0, CATALOG_METRIC);
        assert_eq!(key_id(&src(), &signer), r.payload.key);
        // Another catalog's header, a moved time, a chosen sequence.
        let c2 = Catalog::build(src(), None, 100, vec![leaf("b.x", 1, 0, 1)]).expect("c2");
        assert!(verify(&c2.header, &r).is_err());
        let mut moved = r.clone();
        moved.payload.observed_unix += 1;
        assert!(verify(&c.header, &moved).is_err());
        let mut chosen = r.clone();
        chosen.payload.sequence = 7;
        assert!(verify(&c.header, &chosen).is_err());
        // Catalog sequences are their own: never the manifest sequence of a
        // dataset named like the subject.
        assert_ne!(sequence_for_catalog(0), sequence_for("catalog", 0));
        assert_ne!(sequence_for_catalog(0), sequence_for_catalog(1));
    }

    #[test]
    fn a_leaf_names_exactly_its_manifest_header() {
        let c = Catalog::build(src(), None, 1, vec![leaf("a.x", 1, 0, 1)]).expect("c");
        assert_eq!(c.leaves[0].row()[..LEAF_DOMAIN.len()], *LEAF_DOMAIN);
        assert_ne!(c.header.root(), c.header.leaves_root);
    }
}
