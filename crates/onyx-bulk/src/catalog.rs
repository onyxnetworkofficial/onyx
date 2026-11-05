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

/// The leaves before with `changed` merged in by dataset (both sorted): how a
/// stored diff (ADR-0059 amendment 2) becomes the next catalog's leaves.
pub fn apply_diff(before: &[CatalogLeaf], changed: &[CatalogLeaf]) -> Vec<CatalogLeaf> {
    let mut out = Vec::with_capacity(before.len() + changed.len());
    let (mut i, mut j) = (0, 0);
    while i < before.len() || j < changed.len() {
        match (before.get(i), changed.get(j)) {
            (Some(a), Some(b)) if a.dataset == b.dataset => {
                out.push(b.clone());
                i += 1;
                j += 1;
            }
            (Some(a), Some(b)) if a.dataset < b.dataset => {
                out.push(a.clone());
                i += 1;
            }
            (Some(a), None) => {
                out.push(a.clone());
                i += 1;
            }
            (_, Some(b)) => {
                out.push(b.clone());
                j += 1;
            }
            (None, None) => break,
        }
    }
    out
}

/// Most bytes one `GET /v1/bulk/catalogs/{publisher}/files` answer holds
/// (ADR-0059 amendment 3 item 1); it always holds at least one catalog.
pub const MAX_FILES_BYTES: usize = 16 << 20;

/// A stored catalog's leaves as a node keeps them (amendment 2).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum FileLeaves {
    /// Every leaf.
    Whole(Vec<CatalogLeaf>),
    /// The leaves that differ from the catalog before, in dataset order.
    Diff(Vec<CatalogLeaf>),
}

/// One stored catalog, as the files route answers it.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CatalogFile {
    /// The signed header.
    pub header: CatalogHeader,
    /// Its signed report.
    pub report: SignedReport,
    /// Its leaves, whole or as a diff.
    pub leaves: FileLeaves,
}

/// The files route's answer (ADR-0059 amendment 3 item 1).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CatalogFiles {
    /// The node's newest held catalog number for the publisher.
    pub newest: u64,
    /// Consecutive catalogs from the number asked.
    pub files: Vec<CatalogFile>,
}

impl CatalogFile {
    /// The whole catalog, given the one before (`None` for a whole file or
    /// number 0): a diff is merged into `prev`'s leaves. Nothing is checked
    /// here; see [`Catalog::validate`] and [`Catalog::check_successor`].
    pub fn rebuild(&self, prev: Option<&Catalog>) -> Result<Catalog, CatalogError> {
        let leaves = match (&self.leaves, prev) {
            (FileLeaves::Whole(l), _) => l.clone(),
            (FileLeaves::Diff(changed), Some(p)) => apply_diff(&p.leaves, changed),
            (FileLeaves::Diff(_), None) => {
                return Err(bad(format!(
                    "catalog {}: a diff with no catalog before it",
                    self.header.number
                )))
            }
        };
        Ok(Catalog {
            header: self.header.clone(),
            leaves,
        })
    }
}

/// Why a history walk stopped (ADR-0059 amendment 3 item 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WalkError {
    /// The node's answer does not make the signed catalog.
    Node(String),
    /// A signed catalog breaks the catalog rules (amendment 1 item 3).
    Rules(String),
}

/// One catalog of a history walk (ADR-0059 amendment 3 item 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkStep {
    /// Its number.
    pub number: u64,
    /// Its header root.
    pub root: Hash32,
    /// Its creation time.
    pub created_unix: u64,
    /// Its signed report.
    pub report: SignedReport,
    /// The key that signed it, recovered.
    pub signer: [u8; 20],
    /// The followed dataset's leaf, when this catalog lists it.
    pub leaf: Option<CatalogLeaf>,
}

/// A publisher's catalogs from 0, each checked as a node checks it before it
/// takes it (`validate`, the report signs the header, the append-only
/// successor of the one before), following one dataset (ADR-0059 amendment 3
/// item 2). Only the last catalog is kept whole.
pub struct Walker {
    publisher: SourceId,
    dataset: String,
    prev: Option<Catalog>,
    /// Every catalog taken, from 0.
    pub steps: Vec<WalkStep>,
}

impl Walker {
    /// A walk of `publisher`'s catalogs following `dataset`.
    pub fn new(publisher: SourceId, dataset: String) -> Self {
        Walker {
            publisher,
            dataset,
            prev: None,
            steps: Vec::new(),
        }
    }

    /// The number the next catalog must have.
    pub fn next_number(&self) -> u64 {
        self.steps.len() as u64
    }

    /// Take the next catalog, or say why it does not hold. A diff is merged
    /// into the catalog before it. [`WalkError::Node`]: the bytes served do
    /// not make the signed catalog (wrong number, leaves that do not hash to
    /// the signed leaves root, a report that does not sign the header);
    /// [`WalkError::Rules`]: a catalog the publisher signed, served as
    /// signed, that breaks `validate` or the successor rule.
    pub fn take(&mut self, f: &CatalogFile) -> Result<(), WalkError> {
        let n = self.next_number();
        let node = |s: String| WalkError::Node(format!("catalog {n}: {s}"));
        if f.header.number != n {
            return Err(node(format!("the node sent number {}", f.header.number)));
        }
        if f.header.publisher != self.publisher {
            return Err(node("another publisher's".into()));
        }
        let c = f
            .rebuild(self.prev.as_ref())
            .map_err(|e| node(e.to_string()))?;
        if usize::try_from(c.header.leaf_count).ok() != Some(c.leaves.len())
            || leaves_tree(&c.leaves).root() != c.header.leaves_root
        {
            return Err(node("its leaves do not make the signed leaves root".into()));
        }
        let signer = verify(&c.header, &f.report)
            .map_err(|e| node(format!("the report does not sign it: {e}")))?;
        let rules = |s: String| WalkError::Rules(format!("catalog {n}: {s}"));
        c.validate().map_err(|e| rules(e.to_string()))?;
        if let Some(p) = &self.prev {
            c.check_successor(p).map_err(|e| rules(e.to_string()))?;
        }
        let leaf = c.find(&self.dataset).map(|i| c.leaves[i].clone());
        self.steps.push(WalkStep {
            number: n,
            root: c.header.root(),
            created_unix: c.header.created_unix,
            report: f.report.clone(),
            signer,
            leaf,
        });
        self.prev = Some(c);
        Ok(())
    }

    /// The first catalog that lists the dataset (from there every catalog
    /// lists it: `check_successor` drops nothing).
    pub fn entered(&self) -> Option<&WalkStep> {
        self.steps.iter().find(|s| s.leaf.is_some())
    }
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

    fn file(c: &Catalog, leaves: FileLeaves) -> CatalogFile {
        CatalogFile {
            header: c.header.clone(),
            report: sign(&secret(), &c.header).expect("sign"),
            leaves,
        }
    }

    #[test]
    fn cat_1_a_walk_rebuilds_diffs_and_follows_one_dataset() {
        let c0 = Catalog::build(
            src(),
            None,
            100,
            vec![leaf("a.x", 1, 0, 1), leaf("b.x", 1, 0, 2)],
        )
        .expect("c0");
        let c1 = Catalog::build(
            src(),
            Some(&c0),
            110,
            vec![
                leaf("a.x", 2, 1, 3),
                leaf("b.x", 1, 0, 2),
                leaf("c.x", 1, 0, 4),
            ],
        )
        .expect("c1");
        let f0 = file(&c0, FileLeaves::Whole(c0.leaves.clone()));
        let f1 = file(
            &c1,
            FileLeaves::Diff(vec![leaf("a.x", 2, 1, 3), leaf("c.x", 1, 0, 4)]),
        );
        let mut w = Walker::new(src(), "a.x".into());
        w.take(&f0).expect("0");
        w.take(&f1).expect("1");
        assert_eq!(w.entered().map(|s| s.number), Some(0));
        assert_eq!(w.steps[1].leaf.as_ref().map(|l| l.version), Some(2));
        assert_eq!(w.steps[1].root, c1.header.root());
        let mut w = Walker::new(src(), "c.x".into());
        w.take(&f0).expect("0");
        w.take(&f1).expect("1");
        assert_eq!(w.entered().map(|s| s.number), Some(1));
        // A whole file in place of the diff rebuilds the same catalog.
        let mut w = Walker::new(src(), "a.x".into());
        w.take(&f0).expect("0");
        w.take(&file(&c1, FileLeaves::Whole(c1.leaves.clone())))
            .expect("1 whole");
    }

    #[test]
    fn cat_1_bytes_that_do_not_make_the_signed_catalog_are_the_nodes_and_signed_rule_breaks_the_publishers(
    ) {
        let c0 = Catalog::build(
            src(),
            None,
            100,
            vec![leaf("a.x", 1, 0, 1), leaf("b.x", 1, 0, 2)],
        )
        .expect("c0");
        let f0 = file(&c0, FileLeaves::Whole(c0.leaves.clone()));
        let node = |r: Result<(), WalkError>| matches!(r, Err(WalkError::Node(_)));
        // A diff with nothing before it, a number out of order, a diff that
        // leaves out a change, a report of another catalog.
        let c1 = Catalog::build(
            src(),
            Some(&c0),
            110,
            vec![leaf("a.x", 2, 1, 3), leaf("b.x", 1, 0, 2)],
        )
        .expect("c1");
        let mut w = Walker::new(src(), "a.x".into());
        assert!(node(w.take(&file(&c1, FileLeaves::Diff(vec![])))));
        let mut w = Walker::new(src(), "a.x".into());
        w.take(&f0).expect("0");
        assert!(node(w.take(&f0)));
        assert!(node(w.take(&file(&c1, FileLeaves::Diff(vec![])))));
        let mut forged = file(&c1, FileLeaves::Diff(vec![leaf("a.x", 2, 1, 3)]));
        forged.report = f0.report.clone();
        assert!(node(w.take(&forged)));
        w.take(&file(&c1, FileLeaves::Diff(vec![leaf("a.x", 2, 1, 3)])))
            .expect("1");
        // A signed catalog 2 that drops b.x: served as signed, it breaks the
        // successor rule, which is the publisher's.
        let leaves = vec![leaf("a.x", 2, 1, 3)];
        let dropped = Catalog {
            header: CatalogHeader {
                format: FORMAT,
                publisher: src(),
                number: 2,
                prev: c1.header.root(),
                created_unix: 120,
                leaf_count: 1,
                leaves_root: leaves_tree(&leaves).root(),
            },
            leaves: leaves.clone(),
        };
        assert!(matches!(
            w.take(&file(&dropped, FileLeaves::Whole(leaves))),
            Err(WalkError::Rules(e)) if e.contains("b.x")
        ));
        // One dataset listed twice, signed: the publisher's too (review
        // CAT-HIST-R1 finding 3: a client checks the order itself).
        let twice = vec![leaf("a.x", 1, 0, 1), leaf("a.x", 1, 0, 9)];
        let c = Catalog {
            header: CatalogHeader {
                format: FORMAT,
                publisher: src(),
                number: 0,
                prev: Hash32::ZERO,
                created_unix: 100,
                leaf_count: 2,
                leaves_root: leaves_tree(&twice).root(),
            },
            leaves: twice.clone(),
        };
        let mut w = Walker::new(src(), "a.x".into());
        assert!(matches!(
            w.take(&file(&c, FileLeaves::Whole(twice))),
            Err(WalkError::Rules(_))
        ));
    }
}
