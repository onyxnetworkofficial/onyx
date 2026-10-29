//! Label sets: versioned, content-addressed lists of `address -> entity`.
//! Types fixed in M0. M2 adds first-party attestations ([`attest`]).

pub mod attest;
pub mod dated;
pub mod revoke;
pub mod roster;
pub mod salted;
pub mod score;
pub mod spread;
pub mod version;

use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::ids::is_dotted_id;
use onyx_core::registry::is_registered_canonical_address;
use onyx_core::{
    evidence_is_canonical, is_estimate_id, keccak256, to_canonical_bytes, Confidence, EvidenceRef,
    Hash32, Registry,
};

pub use onyx_core::EntityKind;

use attest::{AttestContext, AttestError, Attestation};
use std::collections::BTreeMap;

/// Most attestations one check accepts (bounds hashing and verification).
pub const MAX_ATTESTATIONS: usize = 1 << 16;
/// Most total attestation bytes (statement + address signature) one check accepts.
pub const MAX_ATTESTATION_BYTES: usize = 16 << 20;
/// Most evidence entries per label (L13-F5).
pub const MAX_EVIDENCE_PER_LABEL: usize = 8;
/// Most attestations a caller may supply to one check (it fetches them by the
/// cited hashes, so an honest caller supplies at most the cited ones).
pub const MAX_SUPPLIED_ATTESTATIONS: usize = 2 * MAX_ATTESTATIONS;
/// Most attestation citations per label.
pub const MAX_ATTESTATIONS_PER_LABEL: usize = 2;

/// Why a label was left out of a shard's input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropReason {
    /// Cites an attestation that was not supplied.
    Missing,
    /// Cited attestation fails verification.
    Invalid(AttestError),
    /// Cited attestation is for another address or entity.
    Mismatch,
    /// Full confidence without a valid attestation.
    UnattestedFullConfidence,
}

impl DropReason {
    fn into_error(self, address: &str) -> LabelError {
        let a = address.to_string();
        match self {
            DropReason::Missing => LabelError::AttestationMissing(a),
            DropReason::Invalid(e) => LabelError::AttestationInvalid(a, e),
            DropReason::Mismatch => LabelError::AttestationMismatch(a),
            DropReason::UnattestedFullConfidence => LabelError::UnattestedFullConfidence(a),
        }
    }
}

/// Attestation lookup with caps, hashing each attestation once and verifying
/// each at most once (L12-F5, L13-F5). Only attestations the set cites are
/// kept; the caps count those (L14-F1).
struct Judge<'a> {
    by_hash: BTreeMap<Hash32, &'a Attestation>,
    /// Cited, supplied, but over the size limits: invalid, not missing (L15-F3).
    too_large: std::collections::BTreeSet<Hash32>,
    verified: BTreeMap<Hash32, Result<attest::Statement, AttestError>>,
    ctx: &'a AttestContext<'a>,
    reg: &'a dyn Registry,
    /// Cross-shard verdict cache and the registry root it is keyed by.
    cache: Option<(&'a attest::AttestCache, Hash32)>,
}

impl<'a> Judge<'a> {
    fn new(
        set: &LabelSet,
        atts: &'a [Attestation],
        ctx: &'a AttestContext<'a>,
        reg: &'a dyn Registry,
        cache: Option<&'a attest::AttestCache>,
    ) -> Result<Self, LabelError> {
        let cited: std::collections::BTreeSet<Hash32> = set
            .labels
            .iter()
            .flat_map(|l| l.evidence.iter())
            .filter(|e| e.kind == ATTESTATION_EVIDENCE)
            .map(|e| e.content_hash)
            .collect();
        if cited.len() > MAX_ATTESTATIONS {
            return Err(LabelError::TooManyAttestations);
        }
        // Supplied caps are about what this caller holds, not about the set:
        // a separate error, so the host abstains instead of judging (L16-F7).
        if atts.len() > MAX_SUPPLIED_ATTESTATIONS {
            return Err(LabelError::SuppliedTooLarge);
        }
        let supplied = atts
            .iter()
            .map(|a| a.statement.len().saturating_add(a.address_sig.len()))
            .fold(0usize, usize::saturating_add);
        if supplied > 4 * MAX_ATTESTATION_BYTES {
            return Err(LabelError::SuppliedTooLarge);
        }
        let mut by_hash = BTreeMap::new();
        let mut too_large = std::collections::BTreeSet::new();
        let mut bytes = 0usize;
        for a in atts {
            let h = a.content_hash();
            if !cited.contains(&h) {
                continue;
            }
            if a.statement.len() > attest::MAX_STATEMENT_LEN
                || a.address_sig.len() > attest::MAX_ADDRESS_SIG_LEN
            {
                too_large.insert(h);
                continue;
            }
            if by_hash.insert(h, a).is_none() {
                bytes = bytes
                    .saturating_add(a.statement.len())
                    .saturating_add(a.address_sig.len());
            }
        }
        if bytes > MAX_ATTESTATION_BYTES {
            return Err(LabelError::TooManyAttestations);
        }
        Ok(Judge {
            by_hash,
            too_large,
            verified: BTreeMap::new(),
            ctx,
            reg,
            cache: cache.map(|c| (c, reg.root())),
        })
    }

    fn label(&mut self, l: &Label) -> Result<(), DropReason> {
        let mut attested = false;
        for e in l.evidence.iter().filter(|e| e.kind == ATTESTATION_EVIDENCE) {
            if self.too_large.contains(&e.content_hash) {
                return Err(DropReason::Invalid(AttestError::TooLarge));
            }
            let a = self
                .by_hash
                .get(&e.content_hash)
                .ok_or(DropReason::Missing)?;
            let (ctx, reg, cache, h) = (self.ctx, self.reg, self.cache, e.content_hash);
            let st = self
                .verified
                .entry(e.content_hash)
                .or_insert_with(|| match cache {
                    Some((c, root)) => c.verify_hashed(a, h, root, ctx, reg),
                    None => a.verify(ctx, reg),
                })
                .as_ref()
                .map_err(|err| DropReason::Invalid(err.clone()))?;
            if st.address != l.address || st.entity != l.entity {
                return Err(DropReason::Mismatch);
            }
            attested = true;
        }
        if l.confidence.0 == Confidence::ONE && !attested {
            return Err(DropReason::UnattestedFullConfidence);
        }
        Ok(())
    }
}

/// Evidence kind for a first-party attestation; its hash is [`Attestation::content_hash`].
pub const ATTESTATION_EVIDENCE: &str = "attestation";
/// Label set names under this prefix are test data and never served (R-LBL-FIX-1).
pub const TEST_SET_PREFIX: &str = "test.";

/// One label: this address belongs to this entity.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Label {
    /// Namespaced address, e.g. `btc:bc1q...`.
    pub address: String,
    /// Onyx entity id, e.g. `entity:coinbase`.
    pub entity: String,
    /// Entity kind.
    pub kind: EntityKind,
    /// Confidence in parts per million.
    pub confidence: Confidence,
    /// Supporting evidence.
    pub evidence: Vec<EvidenceRef>,
}

/// A versioned label set. Labels are strictly sorted by canonical address, one per address.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct LabelSet {
    /// Name, e.g. `onyx.entities.btc`.
    pub name: String,
    /// Version, monotonic per name.
    pub version: u32,
    /// Labels, strictly ascending by address.
    pub labels: Vec<Label>,
}

/// Label set error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum LabelError {
    /// Not strictly sorted or duplicate address.
    #[error("labels must be strictly ascending by address (at {0})")]
    NotCanonical(String),
    /// Confidence above one.
    #[error("confidence out of range for {0}")]
    ConfidenceOutOfRange(String),
    /// Address not in its namespace's canonical form, or namespace unknown (R-LBL-CANON-1).
    #[error("address not canonical: {0}")]
    AddressNotCanonical(String),
    /// Entity not registered, or registered with a different kind (R-PRIV-1).
    #[error("entity not registered or kind mismatch: {0}")]
    BadEntity(String),
    /// Evidence unsorted, duplicated, malformed, or over the per-label caps (R-ENC-2, L13-F5).
    #[error("evidence not canonical for {0}")]
    EvidenceNotCanonical(String),
    /// Set name or version malformed.
    #[error("label set name or version malformed")]
    BadHeader,
    /// Label cites an attestation that was not supplied.
    #[error("attestation missing for {0}")]
    AttestationMissing(String),
    /// Cited attestation fails verification.
    #[error("attestation invalid for {0}: {1}")]
    AttestationInvalid(String, AttestError),
    /// Attestation is valid but for another address or entity.
    #[error("attestation does not match label {0}")]
    AttestationMismatch(String),
    /// The caller supplied more attestations, or bytes, than any check needs
    /// (it should fetch only the cited ones by hash). Not a verdict on the set.
    #[error("caller supplied too many attestation bytes")]
    SuppliedTooLarge,
    /// More than [`MAX_ATTESTATIONS`] cited, or more than [`MAX_ATTESTATION_BYTES`] of them.
    #[error("too many attestations")]
    TooManyAttestations,
    /// Full confidence without a valid first-party attestation (ADR-0015).
    #[error("full confidence needs an attestation: {0}")]
    UnattestedFullConfidence(String),
    /// Test-only set, or a label with no evidence, on the serving path (R-LBL-FIX-1).
    #[error("label set not servable: {0}")]
    NotServable(String),
    /// The label rests on an estimate (evidence kind `estimate` or an
    /// `estimate.` kind: a cluster inference, a model). Checked shards pin
    /// label sets, so an estimated label never enters one (R-EST-2, ADR-0050).
    #[error("label rests on an estimate: {0}")]
    EstimatedLabel(String),
}

/// True for an evidence kind that marks an estimate (R-EST-2).
pub fn is_estimate_evidence(kind: &str) -> bool {
    kind == "estimate" || is_estimate_id(kind)
}

/// One label against the registry: canonical address in a registered
/// namespace, entity registered with the same kind, confidence at most one,
/// evidence canonical and within the per-label caps. Shared by
/// [`LabelSet::validate`] and [`dated::DatedLabelSet::validate`].
pub(crate) fn validate_label(l: &Label, reg: &dyn Registry) -> Result<(), LabelError> {
    if !is_registered_canonical_address(&l.address, reg) {
        return Err(LabelError::AddressNotCanonical(l.address.clone()));
    }
    if reg.entity_kind(&l.entity) != Some(l.kind) {
        return Err(LabelError::BadEntity(l.entity.clone()));
    }
    if l.confidence.0 > Confidence::ONE {
        return Err(LabelError::ConfidenceOutOfRange(l.address.clone()));
    }
    if !evidence_is_canonical(&l.evidence)
        || l.evidence.len() > MAX_EVIDENCE_PER_LABEL
        || l.evidence
            .iter()
            .filter(|e| e.kind == ATTESTATION_EVIDENCE)
            .count()
            > MAX_ATTESTATIONS_PER_LABEL
    {
        return Err(LabelError::EvidenceNotCanonical(l.address.clone()));
    }
    if l.evidence.iter().any(|e| is_estimate_evidence(&e.kind)) {
        return Err(LabelError::EstimatedLabel(l.address.clone()));
    }
    Ok(())
}

impl LabelSet {
    /// Check canonical form against the reviewed registry: every address in its
    /// namespace's canonical spelling (unknown namespaces rejected), every entity
    /// registered with the same kind, evidence sorted.
    pub fn validate(&self, reg: &dyn Registry) -> Result<(), LabelError> {
        if !is_dotted_id(&self.name) || self.version == 0 {
            return Err(LabelError::BadHeader);
        }
        for w in self.labels.windows(2) {
            if w[0].address >= w[1].address {
                return Err(LabelError::NotCanonical(w[1].address.clone()));
            }
        }
        self.labels.iter().try_for_each(|l| validate_label(l, reg))
    }

    /// Strict check, for publishers: every label that cites an attestation
    /// (evidence kind [`ATTESTATION_EVIDENCE`]) must have it in `atts`, valid over
    /// `ctx`, for exactly this address and entity, and a label at full
    /// confidence must have one. The first failing label is the error.
    pub fn check_attestations(
        &self,
        atts: &[Attestation],
        ctx: &AttestContext<'_>,
        reg: &dyn Registry,
    ) -> Result<(), LabelError> {
        let mut judge = Judge::new(self, atts, ctx, reg, None)?;
        for l in &self.labels {
            judge.label(l).map_err(|r| r.into_error(&l.address))?;
        }
        Ok(())
    }

    /// The labels a shard may use (validator path, `assemble_input`): every
    /// label whose cited attestation is invalid, expired, revoked or for
    /// another address or entity is dropped, as is a full-confidence label
    /// without one. The rest of the set is unaffected, so one revocation or
    /// expiry cannot void a whole set (L13-F4).
    ///
    /// The set's content hash pins the hash of every cited attestation, so a
    /// party holding the cited bytes gets exactly this subset. A cited
    /// attestation that is not supplied is an error
    /// ([`LabelError::AttestationMissing`]): the caller lacks data and must
    /// abstain, never drop the label (L14-F1). Supplied attestations that no
    /// label cites are ignored and do not count toward the caps.
    pub fn attested_subset(
        &self,
        atts: &[Attestation],
        ctx: &AttestContext<'_>,
        reg: &dyn Registry,
    ) -> Result<(LabelSet, Vec<(String, DropReason)>), LabelError> {
        self.attested_subset_cached(atts, ctx, reg, None)
    }

    /// As [`LabelSet::attested_subset`], reusing signature verdicts from `cache`
    /// across shards and label sets (L15-F4).
    pub fn attested_subset_cached(
        &self,
        atts: &[Attestation],
        ctx: &AttestContext<'_>,
        reg: &dyn Registry,
        cache: Option<&attest::AttestCache>,
    ) -> Result<(LabelSet, Vec<(String, DropReason)>), LabelError> {
        let mut judge = Judge::new(self, atts, ctx, reg, cache)?;
        let mut kept = Vec::with_capacity(self.labels.len());
        let mut dropped = Vec::new();
        for l in &self.labels {
            match judge.label(l) {
                Ok(()) => kept.push(l.clone()),
                Err(DropReason::Missing) => {
                    return Err(LabelError::AttestationMissing(l.address.clone()))
                }
                Err(r) => dropped.push((l.address.clone(), r)),
            }
        }
        Ok((
            LabelSet {
                name: self.name.clone(),
                version: self.version,
                labels: kept,
            },
            dropped,
        ))
    }

    /// True if this is a test-only set (name starts with [`TEST_SET_PREFIX`]).
    pub fn is_test_only(&self) -> bool {
        self.name.starts_with(TEST_SET_PREFIX)
    }

    /// Gate for the serving path (R-LBL-FIX-1): no test-only sets, every label
    /// cites at least one piece of evidence, and every cited evidence hash
    /// resolves in the operator's evidence store (`resolves`), so placeholder
    /// hashes cannot pass (L12-F7). Wired into delivery in M7.
    pub fn check_servable(
        &self,
        resolves: &dyn Fn(&EvidenceRef) -> bool,
    ) -> Result<(), LabelError> {
        if self.is_test_only() {
            return Err(LabelError::NotServable(self.name.clone()));
        }
        if let Some(l) = self
            .labels
            .iter()
            .find(|l| l.evidence.is_empty() || !l.evidence.iter().all(resolves))
        {
            return Err(LabelError::NotServable(l.address.clone()));
        }
        Ok(())
    }

    /// Hash of the labels alone (no name or version), so two sets with the same
    /// labels under different names are recognised as the same (L15-F5).
    pub fn labels_hash(&self) -> Hash32 {
        let mut b = b"onyx-labels-v1".to_vec();
        b.extend_from_slice(&to_canonical_bytes(&self.labels).unwrap_or_default());
        keccak256(&b)
    }

    /// Content hash of the canonical encoding.
    pub fn content_hash(&self) -> Hash32 {
        // Borsh into a Vec cannot fail.
        keccak256(&to_canonical_bytes(self).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_core::{AddressCanon, StaticRegistry};

    /// Test canon: namespace `t`, lowercase only.
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
        r.add_entity("entity:example", EntityKind::Exchange)
            .and_then(|r| r.add_entity("entity:spammer", EntityKind::Exchange))
            .and_then(|r| r.add_canon(Box::new(Lower)))
            .expect("registry");
        r
    }

    fn set(labels: Vec<Label>) -> LabelSet {
        LabelSet {
            name: "onyx.test".into(),
            version: 1,
            labels,
        }
    }

    fn label(a: &str) -> Label {
        Label {
            address: a.into(),
            entity: "entity:example".into(),
            kind: EntityKind::Exchange,
            confidence: Confidence(900_000),
            evidence: vec![],
        }
    }

    #[test]
    fn duplicate_or_unsorted_rejected() {
        assert!(set(vec![label("t:b"), label("t:a")])
            .validate(&reg())
            .is_err());
        assert!(set(vec![label("t:a"), label("t:a")])
            .validate(&reg())
            .is_err());
        assert_eq!(
            set(vec![label("t:a"), label("t:b")]).validate(&reg()),
            Ok(())
        );
    }

    /// R-EST-2 (ADR-0050): a label resting on an estimate never enters a set
    /// a checked shard can pin, whatever else it cites.
    #[test]
    fn a_checked_label_set_refuses_an_estimated_label() {
        let r = reg();
        for kind in ["estimate", "estimate.cluster-heuristic"] {
            let mut l = label("t:a");
            l.evidence = vec![EvidenceRef {
                kind: kind.into(),
                content_hash: keccak256(b"model output"),
            }];
            assert_eq!(
                set(vec![l]).validate(&r),
                Err(LabelError::EstimatedLabel("t:a".into())),
                "{kind}"
            );
        }
        // A lookalike kind is not the namespace.
        let mut l = label("t:a");
        l.evidence = vec![EvidenceRef {
            kind: "estimates-page".into(),
            content_hash: keccak256(b"doc"),
        }];
        assert_eq!(set(vec![l]).validate(&r), Ok(()));
    }

    /// Header, confidence and evidence bounds at their edges (mutation L4).
    #[test]
    fn validate_bounds_at_the_edges() {
        let r = reg();
        let mut s = set(vec![label("t:a")]);
        s.version = 0;
        assert_eq!(s.validate(&r), Err(LabelError::BadHeader));
        let mut s = set(vec![label("t:a")]);
        s.name = "Not A Name".into();
        assert_eq!(s.validate(&r), Err(LabelError::BadHeader));
        // Full confidence is in range; one ppm more is not.
        let mut l = label("t:a");
        l.confidence = Confidence(Confidence::ONE);
        assert_eq!(set(vec![l.clone()]).validate(&r), Ok(()));
        l.confidence = Confidence(Confidence::ONE + 1);
        assert!(matches!(
            set(vec![l]).validate(&r),
            Err(LabelError::ConfidenceOutOfRange(_))
        ));
        // Two attestation citations are allowed, three are not; other
        // evidence does not count against that cap.
        let ev = |kind: &str, n: u8| EvidenceRef {
            kind: kind.into(),
            content_hash: keccak256(&[n]),
        };
        let with = |e: Vec<EvidenceRef>| {
            let mut l = label("t:a");
            l.evidence = e;
            l.evidence.sort();
            set(vec![l]).validate(&r)
        };
        let att = |n| ev(ATTESTATION_EVIDENCE, n);
        assert_eq!(with(vec![att(1), att(2)]), Ok(()));
        assert!(matches!(
            with(vec![att(1), att(2), att(3)]),
            Err(LabelError::EvidenceNotCanonical(_))
        ));
        assert_eq!(with(vec![ev("doc", 1), ev("doc", 2), ev("doc", 3)]), Ok(()));
        // MAX_EVIDENCE_PER_LABEL itself is allowed.
        assert_eq!(
            with(
                (0..MAX_EVIDENCE_PER_LABEL as u8)
                    .map(|n| ev("doc", n))
                    .collect()
            ),
            Ok(())
        );
        assert_eq!(MAX_SUPPLIED_ATTESTATIONS, 2 * 65_536);
    }

    /// R-LBL-CANON-1: a case variant of an address cannot get a second label.
    #[test]
    fn case_variant_duplicate_rejected() {
        let mut other = label("t:ABC");
        other.entity = "entity:spammer".into();
        let s = set(vec![other, label("t:abc")]);
        assert_eq!(
            s.validate(&reg()),
            Err(LabelError::AddressNotCanonical("t:ABC".into()))
        );
        assert!(matches!(
            set(vec![label("zz:abc")]).validate(&reg()),
            Err(LabelError::AddressNotCanonical(_))
        ));
    }

    /// R-PRIV-1, R-LBL-CANON-1, R-ID-2: only registered entities can be labelled.
    #[test]
    fn entity_grammar_enforced() {
        for bad in [
            "Jane Doe (home wallet)",
            "person:jane",
            "entity:c\u{043E}inbase",
            "entity:c0inbase", // well formed, not registered
            "entity:jane-doe", // well formed, not registered
        ] {
            let mut l = label("t:a");
            l.entity = bad.into();
            assert!(
                matches!(set(vec![l]).validate(&reg()), Err(LabelError::BadEntity(_))),
                "{bad}"
            );
        }
    }

    /// R-ENC-2.
    #[test]
    fn evidence_order_enforced() {
        let e = |n: u8| EvidenceRef {
            kind: "tx".into(),
            content_hash: keccak256(&[n]),
        };
        let mut l = label("t:a");
        l.evidence = vec![e(2), e(1)];
        assert!(matches!(
            set(vec![l]).validate(&reg()),
            Err(LabelError::EvidenceNotCanonical(_))
        ));
    }

    #[test]
    fn version_changes_hash() {
        let a = LabelSet {
            name: "t".into(),
            version: 1,
            labels: vec![label("btc:a")],
        };
        let mut b = a.clone();
        b.version = 2;
        assert_ne!(a.content_hash(), b.content_hash());
    }
}

#[cfg(test)]
mod kind_tests {
    use super::*;
    use onyx_core::StaticRegistry;

    #[test]
    fn kind_must_match_registry() {
        struct AnyBody;
        impl onyx_core::AddressCanon for AnyBody {
            fn namespace(&self) -> &str {
                "t"
            }
            fn canon_id(&self) -> &str {
                "onyx.canon.test.v1"
            }
            fn vectors(&self) -> &'static [(&'static str, bool)] {
                &[]
            }
            fn is_canonical(&self, _: &str) -> bool {
                true
            }
        }
        let mut r = StaticRegistry::new();
        r.add_entity("entity:example", EntityKind::Exchange)
            .and_then(|r| r.add_canon(Box::new(AnyBody)))
            .expect("registry");
        let s = LabelSet {
            name: "onyx.test".into(),
            version: 1,
            labels: vec![Label {
                address: "t:a".into(),
                entity: "entity:example".into(),
                kind: EntityKind::Etf,
                confidence: Confidence(1),
                evidence: vec![],
            }],
        };
        assert!(matches!(s.validate(&r), Err(LabelError::BadEntity(_))));
    }
}

#[cfg(test)]
mod view_layout_tests {
    use super::*;

    /// `onyx_recipe_sdk::LabelSetView` must decode the exact bytes of a `LabelSet`.
    #[test]
    fn recipe_view_matches_label_set_layout() {
        let set = LabelSet {
            name: "onyx.entities.btc".into(),
            version: 3,
            labels: vec![Label {
                address: "btc:bc1qexample".into(),
                entity: "entity:okx".into(),
                kind: EntityKind::Etf,
                confidence: Confidence(990_000),
                evidence: vec![EvidenceRef {
                    kind: "por-signature".into(),
                    content_hash: keccak256(b"sig"),
                }],
            }],
        };
        let bytes = to_canonical_bytes(&set).expect("encode");
        let view: onyx_recipe_sdk::LabelSetView = borsh::from_slice(&bytes).expect("decode view");
        assert_eq!(view.name, set.name);
        assert_eq!(view.version, 3);
        let l = &view.labels[0];
        assert_eq!(l.address, "btc:bc1qexample");
        assert_eq!(l.entity, "entity:okx");
        assert_eq!(l.kind, EntityKind::Etf as u8);
        assert_eq!(l.confidence_ppm, 990_000);
        assert_eq!(l.evidence[0].0, "por-signature");
        assert_eq!(l.evidence[0].1, keccak256(b"sig").0);
        assert_eq!(borsh::to_vec(&view).expect("re-encode"), bytes);
    }
}

#[cfg(test)]
mod attestation_tests {
    use super::attest::test_support::*;
    use super::attest::{Statement, NONCE_LEN};
    use super::*;
    use onyx_core::{AddressCanon, StaticRegistry};

    struct AnyCanon;
    impl AddressCanon for AnyCanon {
        fn namespace(&self) -> &str {
            "t"
        }
        fn canon_id(&self) -> &str {
            "onyx.canon.test.v1"
        }
        fn vectors(&self) -> &'static [(&'static str, bool)] {
            &[]
        }
        fn is_canonical(&self, _: &str) -> bool {
            true
        }
    }

    fn reg() -> StaticRegistry {
        let mut r = StaticRegistry::new();
        r.set_domain("onyx.devnet")
            .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
            .and_then(|r| r.add_entity("entity:other", EntityKind::Exchange))
            .and_then(|r| r.add_canon(Box::new(AnyCanon)))
            .and_then(|r| r.add_proof(Box::new(StubProof)))
            .and_then(|r| r.add_entity_key("entity:example", key_address(&test_secret())))
            .and_then(|r| r.add_entity_key("entity:other", key_address(&test_secret())))
            .expect("registry");
        r
    }

    fn attestation(address: &str, entity: &str) -> Attestation {
        let text = Statement {
            domain: "onyx.devnet".into(),
            entity: entity.into(),
            address: address.into(),
            valid_from: 1,
            expires: 10,
            nonce: [1; NONCE_LEN],
        }
        .render();
        Attestation {
            address_sig: StubProof::sign(address.trim_start_matches("t:"), text.as_bytes()),
            entity_key: attest::test_support::key_address(&test_secret()),
            entity_sig: eip191_sign(&test_secret(), text.as_bytes()),
            statement: text,
        }
    }

    fn label(address: &str, entity: &str, ppm: u32, atts: &[&Attestation]) -> Label {
        let mut evidence: Vec<EvidenceRef> = atts
            .iter()
            .map(|a| EvidenceRef {
                kind: ATTESTATION_EVIDENCE.into(),
                content_hash: a.content_hash(),
            })
            .collect();
        evidence.sort();
        Label {
            address: address.into(),
            entity: entity.into(),
            kind: EntityKind::Exchange,
            confidence: Confidence(ppm),
            evidence,
        }
    }

    const CTX: AttestContext<'static> = AttestContext {
        ranges: &[("t", 5, 5)],
    };

    fn set(labels: Vec<Label>) -> LabelSet {
        LabelSet {
            name: "onyx.test".into(),
            version: 1,
            labels,
        }
    }

    #[test]
    fn full_confidence_needs_valid_matching_attestation() {
        let r = reg();
        let a = attestation("t:a", "entity:example");
        let s = set(vec![label("t:a", "entity:example", Confidence::ONE, &[&a])]);
        assert_eq!(s.validate(&r), Ok(()));
        assert_eq!(
            s.check_attestations(std::slice::from_ref(&a), &CTX, &r),
            Ok(())
        );
        // Not supplied.
        assert_eq!(
            s.check_attestations(&[], &CTX, &r),
            Err(LabelError::AttestationMissing("t:a".into()))
        );
        // Expired at the judging height.
        let late = AttestContext {
            ranges: &[("t", 9, 10)],
        };
        assert!(matches!(
            s.check_attestations(std::slice::from_ref(&a), &late, &r),
            Err(LabelError::AttestationInvalid(
                _,
                attest::AttestError::Expired
            ))
        ));
        // Revoked by the entity (recorded in the registry the shard pins).
        let mut revoked = reg();
        revoked
            .add_revocation(
                "entity:example",
                "t:a",
                5,
                key_address(&test_secret()),
                keccak256(b"rev"),
            )
            .expect("revocation");
        assert!(matches!(
            s.check_attestations(std::slice::from_ref(&a), &CTX, &revoked),
            Err(LabelError::AttestationInvalid(
                _,
                attest::AttestError::Revoked
            ))
        ));
        // No attestation at all.
        let bare = set(vec![label("t:a", "entity:example", Confidence::ONE, &[])]);
        assert_eq!(
            bare.check_attestations(&[], &CTX, &r),
            Err(LabelError::UnattestedFullConfidence("t:a".into()))
        );
        // Below full confidence, no attestation needed.
        let partial = set(vec![label("t:a", "entity:example", 990_000, &[])]);
        assert_eq!(partial.check_attestations(&[], &CTX, &r), Ok(()));
    }

    /// A valid attestation for another address or entity cannot be reused.
    #[test]
    fn attestation_cannot_be_moved() {
        let r = reg();
        let a = attestation("t:a", "entity:example");
        for l in [
            label("t:b", "entity:example", Confidence::ONE, &[&a]),
            label("t:a", "entity:other", Confidence::ONE, &[&a]),
        ] {
            let addr = l.address.clone();
            assert_eq!(
                set(vec![l]).check_attestations(std::slice::from_ref(&a), &CTX, &r),
                Err(LabelError::AttestationMismatch(addr))
            );
        }
    }

    /// R-LBL-FIX-1.
    #[test]
    fn test_sets_and_unevidenced_labels_not_servable() {
        let a = attestation("t:a", "entity:example");
        let good = set(vec![label("t:a", "entity:example", 990_000, &[&a])]);
        let store = |e: &EvidenceRef| e.content_hash == a.content_hash();
        assert_eq!(good.check_servable(&store), Ok(()));
        // Placeholder evidence that the store does not hold.
        let mut fake = good.clone();
        fake.labels[0].evidence = vec![EvidenceRef {
            kind: "x".into(),
            content_hash: keccak256(b"placeholder"),
        }];
        assert_eq!(
            fake.check_servable(&store),
            Err(LabelError::NotServable("t:a".into()))
        );
        let mut test_set = good.clone();
        test_set.name = "test.onyx.entities".into();
        assert!(test_set.is_test_only());
        assert!(matches!(
            test_set.check_servable(&store),
            Err(LabelError::NotServable(_))
        ));
        let bare = set(vec![label("t:a", "entity:example", 990_000, &[])]);
        assert_eq!(
            bare.check_servable(&store),
            Err(LabelError::NotServable("t:a".into()))
        );
    }

    /// L13-F4: a failing attestation drops only its label.
    #[test]
    fn attested_subset_drops_only_failing_labels() {
        let r = reg();
        let good = attestation("t:a", "entity:example");
        let expired = AttestContext {
            ranges: &[("t", 5, 10)],
        };
        let s = set(vec![
            label("t:a", "entity:example", Confidence::ONE, &[&good]),
            label("t:b", "entity:example", Confidence::ONE, &[]),
            label("t:c", "entity:example", 990_000, &[]),
            label("t:d", "entity:other", 990_000, &[&good]),
        ]);
        let (kept, dropped) = s
            .attested_subset(std::slice::from_ref(&good), &CTX, &r)
            .expect("subset");
        let addrs: Vec<&str> = kept.labels.iter().map(|l| l.address.as_str()).collect();
        assert_eq!(addrs, ["t:a", "t:c"]);
        assert_eq!(
            dropped,
            vec![
                ("t:b".to_string(), DropReason::UnattestedFullConfidence),
                ("t:d".to_string(), DropReason::Mismatch),
            ]
        );
        // Over a range past expiry, the attested label goes too; the rest stays.
        let (kept, dropped) = s
            .attested_subset(std::slice::from_ref(&good), &expired, &r)
            .expect("subset");
        assert_eq!(kept.labels.len(), 1);
        assert_eq!(
            dropped[0],
            (
                "t:a".to_string(),
                DropReason::Invalid(attest::AttestError::Expired)
            )
        );
    }

    /// L13-F5: caps on evidence per label and on attestations per call.
    #[test]
    fn evidence_and_attestation_caps() {
        let r = reg();
        let mut l = label("t:a", "entity:example", 990_000, &[]);
        l.evidence = (0..=MAX_EVIDENCE_PER_LABEL as u8)
            .map(|n| EvidenceRef {
                kind: "doc".into(),
                content_hash: keccak256(&[n]),
            })
            .collect();
        l.evidence.sort();
        assert!(matches!(
            set(vec![l]).validate(&r),
            Err(LabelError::EvidenceNotCanonical(_))
        ));
        // Cap on cited attestations: checked before any hashing or verifying.
        let many: Vec<Label> = (0..=MAX_ATTESTATIONS)
            .map(|n| {
                let mut l = label(&format!("t:a{n:06}"), "entity:example", 990_000, &[]);
                l.evidence = vec![EvidenceRef {
                    kind: ATTESTATION_EVIDENCE.into(),
                    content_hash: keccak256(&n.to_be_bytes()),
                }];
                l
            })
            .collect();
        assert_eq!(
            set(many).attested_subset(&[], &CTX, &r).map(|_| ()),
            Err(LabelError::TooManyAttestations)
        );
    }

    /// L14-F1: the set pins the cited attestation hashes, so the result does
    /// not depend on which extra attestations the caller holds; a missing cited
    /// one is an error (abstain), never a silent drop; uncited junk is ignored.
    #[test]
    fn result_does_not_depend_on_supplied_extras() {
        let r = reg();
        let a = attestation("t:a", "entity:example");
        let s = set(vec![
            label("t:a", "entity:example", Confidence::ONE, &[&a]),
            label("t:c", "entity:example", 990_000, &[]),
        ]);
        assert_eq!(
            s.attested_subset(&[], &CTX, &r).map(|_| ()),
            Err(LabelError::AttestationMissing("t:a".into()))
        );
        let only = s
            .attested_subset(std::slice::from_ref(&a), &CTX, &r)
            .expect("cited");
        let mut junk: Vec<Attestation> = (0..2000)
            .map(|n| {
                let mut j = attestation("t:b", "entity:example");
                j.address_sig = format!("junk {n}");
                j
            })
            .collect();
        junk.push(a.clone());
        let with_junk = s.attested_subset(&junk, &CTX, &r).expect("junk ignored");
        assert_eq!(only, with_junk);
        assert_eq!(only.0.labels.len(), 2);
    }

    /// L15-F3: a cited attestation that is supplied but oversized drops its
    /// label (invalid); it does not make every validator abstain.
    #[test]
    fn oversized_cited_attestation_is_invalid_not_missing() {
        let r = reg();
        let mut big = attestation("t:a", "entity:example");
        big.address_sig = big.address_sig.clone() + &" ".repeat(attest::MAX_ADDRESS_SIG_LEN);
        let s = set(vec![
            label("t:a", "entity:example", Confidence::ONE, &[&big]),
            label("t:c", "entity:example", 990_000, &[]),
        ]);
        let (kept, dropped) = s
            .attested_subset(std::slice::from_ref(&big), &CTX, &r)
            .expect("subset");
        assert_eq!(kept.labels.len(), 1);
        assert_eq!(
            dropped,
            vec![(
                "t:a".to_string(),
                DropReason::Invalid(AttestError::TooLarge)
            )]
        );
    }

    /// L15-F4: the signature checks run once per (attestation, registry root);
    /// range checks still run for every shard.
    #[test]
    fn cache_reuses_signature_verdicts_across_shards() {
        let r = reg();
        let a = attestation("t:a", "entity:example");
        let s = set(vec![label("t:a", "entity:example", Confidence::ONE, &[&a])]);
        let cache = attest::AttestCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        let shard1 = AttestContext {
            ranges: &[("t", 2, 3)],
        };
        let shard2 = AttestContext {
            ranges: &[("t", 4, 12)],
        };
        let (k1, d1) = s
            .attested_subset_cached(std::slice::from_ref(&a), &shard1, &r, Some(&cache))
            .expect("shard 1");
        assert_eq!((k1.labels.len(), d1.len()), (1, 0));
        let (k2, d2) = s
            .attested_subset_cached(std::slice::from_ref(&a), &shard2, &r, Some(&cache))
            .expect("shard 2");
        assert_eq!(k2.labels.len(), 0);
        assert_eq!(
            d2,
            vec![("t:a".to_string(), DropReason::Invalid(AttestError::Expired))]
        );
        assert_eq!(cache.len(), 1, "one static verdict for both shards");
        assert!(!cache.is_empty());
        // Same answers as without the cache.
        assert_eq!(
            s.attested_subset(std::slice::from_ref(&a), &shard2, &r)
                .expect("plain"),
            (k2, d2)
        );
    }
}
