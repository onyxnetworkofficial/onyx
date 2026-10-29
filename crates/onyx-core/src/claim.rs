//! Claims and the two-lane model.

use crate::canonical::{from_canonical_bytes, CanonicalError};
use crate::hash::{keccak256, Hash32, RecipeId};
use crate::ids::{is_dotted_id, is_instrument_id};
use crate::registry::{is_registered_canonical_address, restrictiveness, Registry};
use crate::rights::{DataRights, Redistribution, RightsError};
use crate::source::{SourceError, SourceRef};
use crate::value::{Value, ValueError};
use borsh::{BorshDeserialize, BorshSerialize};

/// Which lane a claim belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Lane {
    /// Anyone can check it by rerunning open code or matching open sources.
    Objective,
    /// Cannot be proven. Versioned input with evidence and a scored confidence.
    Subjective,
}

/// Signature schemes accepted for first-party attestations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum AttestationScheme {
    /// Bitcoin BIP-322 generic signed message.
    Bip322,
    /// EVM personal_sign.
    Eip191,
    /// EVM typed data.
    Eip712,
}

/// How a claim is verified. New sectors add methods here only when no
/// existing method fits; adding a variant is a spec change with an ADR.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum VerificationMethod {
    /// Objective: rerun `recipe` on the claim's source snapshot; output must be byte-identical.
    Recompute {
        /// Content-addressed recipe.
        recipe: RecipeId,
    },
    /// Objective: value must equal what the official publisher released. The
    /// claim's own `source` is the pinned official document, so there is no
    /// second source field that could disagree with it (R-QUORUM-1).
    OfficialSourceMatch,
    /// Objective: at least `threshold` of `sources` must agree within
    /// `tolerance`. Each source carries its own pinned snapshot so the quorum can
    /// be rechecked in a dispute.
    CrossSourceQuorum {
        /// Independent sources, strictly ascending by source id; includes the claim's `source`.
        sources: Vec<SourceRef>,
        /// Minimum agreeing sources: at least 2 and a strict majority.
        threshold: u16,
        /// Allowed absolute difference in the value's own units (same scale for
        /// `Fixed`); 0 = exact. Capped per metric by the metric policy (M3).
        tolerance: u128,
    },
    /// Subjective, top confidence: the entity signed for itself.
    FirstPartyAttestation {
        /// Signature scheme.
        scheme: AttestationScheme,
    },
    /// Subjective: contributor judgment, scored later by a proper scoring rule.
    CalibratedJudgment,
    /// Objective, proof level 2 (ADR-0027): the value is the settled result of a
    /// level-2 observation round for the feed `spec` (an `ObservationSpec` id):
    /// a strict majority of bonded observers read the same public source in the
    /// same window. Appended last so earlier variants keep their encoding.
    ReObserved {
        /// `ObservationSpec::id` of the feed.
        spec: Hash32,
        /// `ObservationSettlement::result_hash` of the settled round the value
        /// comes from (binds the round and its outcome, L2R-F3). `validate`
        /// only checks the shape (non-zero) and `Claim::decode` refuses this
        /// variant (L2R3-F3). A level-2 claim is accepted only through
        /// `onyx_coordinator::observe::ObservationResults::accept_claim`
        /// (or `decode_claim`), which looks the result up among the rounds that
        /// coordinator settled itself, checks the value against it, and binds
        /// the claim's metric, subject, snapshot, source, rights, label set and
        /// evidence to the feed (L2R2-F1, L2R3-F1).
        result: Hash32,
    },
    /// Objective, proof level 3 (ADR-0028): the value is exactly what a
    /// registered key of the claim's source signed in one report. Appended
    /// last so earlier variants keep their encoding. `validate` only checks the
    /// shape (non-zero) and `Claim::decode` refuses this variant: a signed-source
    /// claim is accepted only through
    /// `onyx_coordinator::signed::SignedSources::accept_claim` (or
    /// `decode_claim`), which checks the signature against the key registry,
    /// refuses replays and equivocations, and binds every claim field to the
    /// signed payload.
    SignedSource {
        /// `SourceKey::id` of the signing key.
        key: Hash32,
        /// `SignedReport::hash` of the report (payload and signature).
        report: Hash32,
    },
}

impl VerificationMethod {
    /// The lane this method belongs to.
    pub fn lane(&self) -> Lane {
        match self {
            VerificationMethod::Recompute { .. }
            | VerificationMethod::OfficialSourceMatch
            | VerificationMethod::CrossSourceQuorum { .. }
            | VerificationMethod::ReObserved { .. }
            | VerificationMethod::SignedSource { .. } => Lane::Objective,
            VerificationMethod::FirstPartyAttestation { .. }
            | VerificationMethod::CalibratedJudgment => Lane::Subjective,
        }
    }
}

/// Probability in parts per million, 0..=1_000_000. Integer so it stays canonical.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, BorshSerialize, BorshDeserialize)]
pub struct Confidence(pub u32);

impl Confidence {
    /// One million parts per million.
    pub const ONE: u32 = 1_000_000;
}

/// A label set version pinned by content hash.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct LabelSetRef {
    /// Label set name (dotted id), e.g. `onyx.entities.btc`.
    pub name: String,
    /// Version, starting at 1, monotonic per name.
    pub version: u32,
    /// Keccak-256 of the canonical label set bytes.
    pub content_hash: Hash32,
}

impl LabelSetRef {
    /// Structural check.
    pub fn validate(&self) -> Result<(), ClaimError> {
        if !is_dotted_id(&self.name) {
            return Err(ClaimError::BadId("label set name"));
        }
        if self.version == 0 {
            return Err(ClaimError::BadId("label set version 0"));
        }
        if self.content_hash == Hash32::ZERO {
            return Err(ClaimError::BadId("label set hash"));
        }
        Ok(())
    }

    /// Keccak-256 of the canonical encoding (binds name, version and content).
    pub fn hash(&self) -> Hash32 {
        keccak256(&borsh::to_vec(self).unwrap_or_default())
    }
}

/// Namespaced metric id (dotted id), e.g. `crypto.btc.entity_balance`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, BorshSerialize, BorshDeserialize)]
pub struct MetricId(pub String);

/// What a claim is about. There is deliberately no variant for a natural
/// person, and every variant has a strict id grammar (see `ids`). The grammar
/// stops names and free text; the entity registry (M2) stops person slugs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, BorshSerialize, BorshDeserialize)]
pub enum Subject {
    /// A registered organisation: `entity:<slug>`.
    Entity(String),
    /// A chain address or account: `<ns>:<body>`. From M2, only addresses that a
    /// label set attributes to a registered entity may be subjects (R-PRIV-1).
    Address(String),
    /// A tradable instrument or series: `ticker:IBIT`, `series:FEDFUNDS`.
    Instrument(String),
    /// A whole network or aggregate (dotted id), e.g. `btc.mainnet`.
    Aggregate(String),
}

impl Subject {
    /// Grammar plus registry check (R-PRIV-1, R-SUBJ-1):
    /// - `Entity` must be a registered organisation;
    /// - `Address` must be in its namespace's canonical spelling;
    /// - `Instrument` namespace must be registered and its body uppercase;
    /// - `Aggregate` must be registered.
    pub fn is_valid(&self, reg: &dyn Registry) -> bool {
        match self {
            Subject::Entity(s) => reg.entity_kind(s).is_some(),
            Subject::Address(s) => is_registered_canonical_address(s, reg),
            Subject::Instrument(s) => {
                is_instrument_id(s)
                    && s.split_once(':').is_some_and(|(ns, body)| {
                        reg.instrument_namespace_known(ns)
                            && !body.bytes().any(|c| c.is_ascii_lowercase())
                    })
            }
            Subject::Aggregate(s) => is_dotted_id(s) && reg.aggregate_known(s),
        }
    }
}

/// Pointer to supporting evidence stored content-addressed offchain.
/// Ordered by `(kind, content_hash)`; evidence lists are strictly ascending.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, BorshSerialize, BorshDeserialize)]
pub struct EvidenceRef {
    /// Kind tag (dotted id), e.g. `bip322-signature`, `filing`, `tx`.
    pub kind: String,
    /// Keccak-256 of the evidence bytes in the content store.
    pub content_hash: Hash32,
}

/// Check an evidence list is canonical: well-formed kinds, strictly ascending, no duplicates.
pub fn evidence_is_canonical(evidence: &[EvidenceRef]) -> bool {
    evidence
        .iter()
        .all(|e| is_dotted_id(&e.kind) && e.content_hash != Hash32::ZERO)
        && evidence.windows(2).all(|w| w[0] < w[1])
}

/// One claim: a value about a subject, with how to check it and the right to serve it.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Claim {
    /// Metric.
    pub metric: MetricId,
    /// Subject.
    pub subject: Subject,
    /// Value.
    pub value: Value,
    /// Input state.
    pub source: SourceRef,
    /// Verification method; determines the lane.
    pub method: VerificationMethod,
    /// Label set used, required when the value depends on labels (the metric
    /// schema says which, from M1).
    pub label_set: Option<LabelSetRef>,
    /// Rights. Must equal the registry's reviewed rights for `source.source`.
    pub rights: DataRights,
    /// Evidence pointers, strictly ascending.
    pub evidence: Vec<EvidenceRef>,
    /// Confidence; required for the subjective lane, forbidden for the objective lane.
    pub confidence: Option<Confidence>,
}

/// Claim validation error.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClaimError {
    /// Bad value.
    #[error(transparent)]
    Value(#[from] ValueError),
    /// Rights forbid ingest or are malformed.
    #[error(transparent)]
    Rights(#[from] RightsError),
    /// Bad source or snapshot.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// Rights differ from the registry's reviewed rights for the source (R-RIGHTS-1).
    #[error("claim rights differ from the registered rights of its source")]
    RightsNotRegistered,
    /// Metric not in the registry.
    #[error("metric not registered: {0}")]
    UnknownMetric(String),
    /// Value variant or scale differs from the metric's registered kind (R-VAL-1).
    #[error("value kind does not match the metric's registered kind")]
    ValueKindMismatch,
    /// Label set present when the metric needs none, or missing when it needs one.
    #[error("label set requirement of the metric not met")]
    LabelSetRequirement,
    /// Source not in the registry.
    #[error("source not registered: {0}")]
    UnknownSource(String),
    /// Subject malformed, unregistered or not canonical.
    #[error("subject malformed, unregistered or not canonical")]
    BadSubject,
    /// Subjective claim without confidence.
    #[error("subjective claim needs a confidence")]
    MissingConfidence,
    /// Objective claim with a confidence (objective claims are right or wrong).
    #[error("objective claim must not carry a confidence")]
    UnexpectedConfidence,
    /// Confidence above one.
    #[error("confidence {0} ppm exceeds 1_000_000")]
    ConfidenceOutOfRange(u32),
    /// Quorum definition invalid.
    #[error("quorum invalid: {0}")]
    BadQuorum(&'static str),
    /// Attestation without evidence.
    #[error("first-party attestation needs signature evidence")]
    AttestationWithoutEvidence,
    /// Evidence not canonical.
    #[error("evidence must be well formed and strictly ascending")]
    EvidenceNotCanonical,
    /// Malformed identifier.
    #[error("malformed identifier: {0}")]
    BadId(&'static str),
}

/// Error from [`Claim::decode`].
#[derive(Debug, thiserror::Error)]
pub enum ClaimDecodeError {
    /// Bytes are not a canonical claim encoding.
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    /// Decoded, but breaks a claim rule.
    #[error(transparent)]
    Invalid(#[from] ClaimError),
    /// A level-2 claim (`ReObserved`): it is admitted only through
    /// `onyx_coordinator::observe::ObservationResults::{accept_claim,
    /// decode_claim}`, which check it against a settled result (L2R3-F3).
    #[error("level-2 claims enter only through the observation intake")]
    NeedsObservationIntake,
    /// A level-3 signed-source claim (`SignedSource`): it is admitted only
    /// through `onyx_coordinator::signed::SignedSources::{accept_claim,
    /// decode_claim}`, which verify the signed report (ADR-0028).
    #[error("signed-source claims enter only through the signed-source intake")]
    NeedsSignedSourceIntake,
    /// A first-party attestation claim (`FirstPartyAttestation`, level 3):
    /// `validate` checks only that some evidence is present, never the
    /// signature, so this decode refuses it (AUD-2-F3). Attestations are
    /// verified only by the label-attestation verifier
    /// (`onyx_labels::attest::Attestation::verify`); a claim intake that runs
    /// it does not exist yet.
    #[error("first-party attestation claims enter only through the attestation verifier")]
    NeedsAttestationIntake,
}

impl Claim {
    /// The lane, derived from the method so the two can never disagree.
    pub fn lane(&self) -> Lane {
        self.method.lane()
    }

    /// The ingest decode for every lane except the two intake-only methods:
    /// canonical bytes in, validated claim out (R-INGEST-1). A level-2 claim
    /// (`ReObserved`) is refused here (L2R3-F3): `validate` checks only its
    /// shape, so it is admitted only through the observation intake
    /// (`onyx_coordinator::observe::ObservationResults::decode_claim`), which
    /// checks it against a result that coordinator settled itself. A level-3
    /// signed-source claim (`SignedSource`) is refused for the same reason and
    /// enters only through `onyx_coordinator::signed::SignedSources`
    /// (ADR-0028). A first-party attestation claim is refused too: `validate`
    /// never checks its signature (AUD-2-F3).
    pub fn decode(bytes: &[u8], reg: &dyn Registry) -> Result<Claim, ClaimDecodeError> {
        let claim: Claim = from_canonical_bytes(bytes)?;
        match claim.method {
            VerificationMethod::ReObserved { .. } => {
                return Err(ClaimDecodeError::NeedsObservationIntake)
            }
            VerificationMethod::SignedSource { .. } => {
                return Err(ClaimDecodeError::NeedsSignedSourceIntake)
            }
            VerificationMethod::FirstPartyAttestation { .. } => {
                return Err(ClaimDecodeError::NeedsAttestationIntake)
            }
            _ => {}
        }
        claim.validate(reg)?;
        Ok(claim)
    }

    /// Validate against the rules and the reviewed registry. Called on every ingest path.
    pub fn validate(&self, reg: &dyn Registry) -> Result<(), ClaimError> {
        if !is_dotted_id(&self.metric.0) {
            return Err(ClaimError::BadId("metric"));
        }
        let metric = reg
            .metric(&self.metric.0)
            .ok_or_else(|| ClaimError::UnknownMetric(self.metric.0.clone()))?;
        if !self.subject.is_valid(reg) {
            return Err(ClaimError::BadSubject);
        }
        self.value.validate()?;
        if !self.value.is_kind(metric.value_kind) {
            return Err(ClaimError::ValueKindMismatch);
        }
        if self.label_set.is_some() != metric.needs_label_set {
            return Err(ClaimError::LabelSetRequirement);
        }
        self.source.validate()?;
        self.rights.check_ingest()?;
        let registered = reg
            .source_rights(&self.source.source)
            .ok_or_else(|| ClaimError::UnknownSource(self.source.source.0.clone()))?;
        if *registered != self.rights {
            return Err(ClaimError::RightsNotRegistered);
        }
        if let Some(l) = &self.label_set {
            l.validate()?;
        }
        if !evidence_is_canonical(&self.evidence) {
            return Err(ClaimError::EvidenceNotCanonical);
        }
        match (self.lane(), self.confidence) {
            (Lane::Subjective, None) => return Err(ClaimError::MissingConfidence),
            (Lane::Objective, Some(_)) => return Err(ClaimError::UnexpectedConfidence),
            (_, Some(Confidence(c))) if c > Confidence::ONE => {
                return Err(ClaimError::ConfidenceOutOfRange(c))
            }
            _ => {}
        }
        match &self.method {
            VerificationMethod::CrossSourceQuorum {
                sources,
                threshold,
                tolerance,
            } => self.validate_quorum(sources, *threshold, *tolerance, reg)?,
            VerificationMethod::FirstPartyAttestation { .. } if self.evidence.is_empty() => {
                return Err(ClaimError::AttestationWithoutEvidence)
            }
            VerificationMethod::ReObserved { spec, result }
                if *spec == Hash32::ZERO || *result == Hash32::ZERO =>
            {
                return Err(ClaimError::BadId("observation spec or result"))
            }
            VerificationMethod::SignedSource { key, report }
                if *key == Hash32::ZERO || *report == Hash32::ZERO =>
            {
                return Err(ClaimError::BadId("signed-source key or report"))
            }
            _ => {}
        }
        Ok(())
    }

    fn validate_quorum(
        &self,
        sources: &[SourceRef],
        threshold: u16,
        tolerance: u128,
        reg: &dyn Registry,
    ) -> Result<(), ClaimError> {
        if !self.value.is_numeric() {
            return Err(ClaimError::BadQuorum("quorum needs a numeric value"));
        }
        // Until per-metric tolerance policy exists (M3), quorum means exact agreement (R-QUORUM-2).
        if tolerance != 0 {
            return Err(ClaimError::BadQuorum(
                "tolerance must be 0 until the metric policy exists",
            ));
        }
        if sources.len() < 2 {
            return Err(ClaimError::BadQuorum("needs at least 2 sources"));
        }
        let t = usize::from(threshold);
        if t < 2 || t > sources.len() || 2 * t <= sources.len() {
            return Err(ClaimError::BadQuorum(
                "threshold must be at least 2 and a strict majority",
            ));
        }
        if !sources.windows(2).all(|w| w[0].source < w[1].source) {
            return Err(ClaimError::BadQuorum(
                "sources must be strictly ascending and distinct",
            ));
        }
        if !sources.contains(&self.source) {
            return Err(ClaimError::BadQuorum(
                "claim source must be one of the quorum sources",
            ));
        }
        // Every source must be registered (no invented sources), and the claim
        // must carry the most restrictive rights among them (R-RIGHTS-2).
        let mut most = 0u8;
        let mut restricted: Option<&DataRights> = None;
        for s in sources {
            s.validate()?;
            let r = reg
                .source_rights(&s.source)
                .ok_or_else(|| ClaimError::UnknownSource(s.source.0.clone()))?;
            most = most.max(restrictiveness(r.redistribution));
            // Any source with obligations (Restricted, or an attribution line) must
            // match every other such source on license, terms and attribution, so no
            // source's obligations are dropped (R-RIGHTS-4, R-RIGHTS-5).
            if r.redistribution == Redistribution::Restricted || r.attribution.is_some() {
                match restricted {
                    Some(p)
                        if (&p.license, &p.terms, &p.attribution)
                            != (&r.license, &r.terms, &r.attribution) =>
                    {
                        return Err(ClaimError::BadQuorum(
                            "sources with obligations must share license, terms and attribution",
                        ))
                    }
                    _ => restricted = Some(r),
                }
            }
        }
        if restrictiveness(self.rights.redistribution) < most {
            return Err(ClaimError::BadQuorum(
                "claim source must carry the most restrictive rights in the quorum",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::to_canonical_bytes;
    use crate::ids::AddressCanon;
    use crate::registry::{EntityKind, MetricInfo, StaticRegistry};
    use crate::source::{ChainId, Snapshot, SourceId};
    use crate::value::ValueKind;

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

    fn restricted(id: &str, terms: &str) -> DataRights {
        DataRights {
            license: "LicenseRef-vendor".into(),
            redistribution: Redistribution::Restricted,
            source: SourceId(id.into()),
            attribution: None,
            terms: terms.into(),
        }
    }

    fn reg() -> StaticRegistry {
        let mut r = StaticRegistry::new();
        let m = |value_kind, needs_label_set| MetricInfo {
            value_kind,
            needs_label_set,
        };
        r.add_metric("test.metric", m(ValueKind::UInt, false))
            .and_then(|r| r.add_metric("test.flag", m(ValueKind::Bool, false)))
            .and_then(|r| r.add_metric("test.labelled", m(ValueKind::UInt, true)))
            .expect("metrics");
        r.add_entity("entity:example-exchange", EntityKind::Exchange)
            .and_then(|r| r.add_aggregate("btc.mainnet"))
            .and_then(|r| r.add_instrument_namespace("ticker"))
            .and_then(|r| r.add_canon(Box::new(Lower)))
            .expect("registry");
        for id in [
            "chain.test",
            "chain.other",
            "vendor.b",
            "vendor.d",
            "vendor.e",
        ] {
            r.add_source(DataRights::public_chain(SourceId(id.into())))
                .expect("source");
        }
        let mut r3 = restricted("vendor.r3", "terms.vendor.nc-v1");
        r3.license = "LicenseRef-other".into();
        for (id, who) in [
            ("vendor.att1", "Data: Org One"),
            ("vendor.att2", "Data: Org Two"),
        ] {
            let mut d = DataRights::public_chain(SourceId(id.into()));
            d.license = "CC-BY-4.0".into();
            d.attribution = Some(who.into());
            r.add_source(d).expect("attributed source");
        }
        r.add_source(restricted("vendor.r1", "terms.vendor.nc-v1"))
            .and_then(|r| r.add_source(restricted("vendor.r2", "terms.vendor.other")))
            .and_then(|r| r.add_source(restricted("vendor.r4", "terms.vendor.nc-v1")))
            .and_then(|r| r.add_source(r3))
            .expect("restricted");
        r
    }

    fn src(id: &str, n: u8) -> SourceRef {
        SourceRef {
            source: SourceId(id.into()),
            snapshot: Snapshot::Block {
                chain: ChainId("test".into()),
                height: u64::from(n),
                block_hash: keccak256(&[n]),
            },
        }
    }

    fn ev(kind: &str, n: u8) -> EvidenceRef {
        EvidenceRef {
            kind: kind.into(),
            content_hash: keccak256(&[n]),
        }
    }

    fn base() -> Claim {
        Claim {
            metric: MetricId("test.metric".into()),
            subject: Subject::Entity("entity:example-exchange".into()),
            value: Value::UInt(42),
            source: src("chain.test", 1),
            method: VerificationMethod::Recompute {
                recipe: keccak256(b"wasm"),
            },
            label_set: None,
            rights: DataRights::public_chain(SourceId("chain.test".into())),
            evidence: vec![],
            confidence: None,
        }
    }

    #[test]
    fn objective_claim_valid() {
        assert_eq!(base().lane(), Lane::Objective);
        assert_eq!(base().validate(&reg()), Ok(()));
    }

    /// L2R-F3 / L2R2-F1: `validate` refuses a level-2 pointer with a zero
    /// feed or result; a non-zero pointer only passes the shape check (the
    /// result itself is checked at intake, `ObservationResults::accept_claim`).
    #[test]
    fn re_observed_needs_non_zero_feed_and_result() {
        let mut c = base();
        for (spec, result) in [
            (Hash32::ZERO, keccak256(b"r")),
            (keccak256(b"s"), Hash32::ZERO),
        ] {
            c.method = VerificationMethod::ReObserved { spec, result };
            assert_eq!(
                c.validate(&reg()),
                Err(ClaimError::BadId("observation spec or result"))
            );
        }
        c.method = VerificationMethod::ReObserved {
            spec: keccak256(b"s"),
            result: keccak256(b"r"),
        };
        assert_eq!(c.validate(&reg()), Ok(()));
    }

    #[test]
    fn objective_rejects_confidence() {
        let mut c = base();
        c.confidence = Some(Confidence(900_000));
        assert_eq!(c.validate(&reg()), Err(ClaimError::UnexpectedConfidence));
    }

    #[test]
    fn subjective_requires_confidence_in_range() {
        let mut c = base();
        c.method = VerificationMethod::CalibratedJudgment;
        assert_eq!(c.validate(&reg()), Err(ClaimError::MissingConfidence));
        c.confidence = Some(Confidence(1_000_001));
        assert_eq!(
            c.validate(&reg()),
            Err(ClaimError::ConfidenceOutOfRange(1_000_001))
        );
        c.confidence = Some(Confidence(700_000));
        assert_eq!(c.validate(&reg()), Ok(()));
    }

    #[test]
    fn attestation_requires_evidence() {
        let mut c = base();
        c.method = VerificationMethod::FirstPartyAttestation {
            scheme: AttestationScheme::Bip322,
        };
        c.confidence = Some(Confidence(Confidence::ONE));
        assert_eq!(
            c.validate(&reg()),
            Err(ClaimError::AttestationWithoutEvidence)
        );
        c.evidence.push(ev("bip322-signature", 1));
        assert_eq!(c.validate(&reg()), Ok(()));
    }

    fn q(sources: Vec<SourceRef>, threshold: u16, tolerance: u128) -> VerificationMethod {
        VerificationMethod::CrossSourceQuorum {
            sources,
            threshold,
            tolerance,
        }
    }

    /// R-QUORUM-1, R-QUORUM-2.
    #[test]
    fn quorum_rules() {
        let r = reg();
        let mut c = base();
        let a = src("chain.test", 1);
        let b = src("vendor.b", 2);
        let d = src("vendor.d", 3);
        let bad = |c: &Claim| matches!(c.validate(&r), Err(ClaimError::BadQuorum(_)));
        c.method = q(vec![a.clone()], 1, 0); // 1-of-1
        assert!(bad(&c));
        c.method = q(
            vec![a.clone(), b.clone(), d.clone(), src("vendor.e", 4)],
            2,
            0,
        ); // not majority
        assert!(bad(&c));
        c.method = q(vec![b.clone(), a.clone()], 2, 0); // unsorted
        assert!(bad(&c));
        c.method = q(vec![b.clone(), d.clone()], 2, 0); // claim source missing
        assert!(bad(&c));
        c.method = q(vec![a.clone(), b.clone(), d.clone()], 2, u128::MAX); // vacuous tolerance
        assert!(bad(&c));
        c.method = q(vec![a.clone(), b.clone(), d.clone()], 2, 0);
        c.metric = MetricId("test.flag".into());
        c.value = Value::Bool(true); // non-numeric
        assert!(bad(&c));
        c.metric = MetricId("test.metric".into());
        c.value = Value::UInt(42);
        assert_eq!(c.validate(&r), Ok(()));
        // Unregistered (invented or spelling-variant) source.
        c.method = q(vec![a.clone(), src("vendor.zz", 5)], 2, 0);
        assert!(matches!(c.validate(&r), Err(ClaimError::UnknownSource(_))));
    }

    /// Quorum bounds at their edges (mutation testing, L4): a threshold above
    /// the source count, a 3-of-5 majority, a duplicated source and two
    /// sources with identical obligations.
    #[test]
    fn quorum_bounds_at_the_edges() {
        let r = reg();
        let bad = |c: &Claim| matches!(c.validate(&r), Err(ClaimError::BadQuorum(_)));
        let mut c = base();
        let a = src("chain.test", 1);
        let five = vec![
            src("chain.other", 2),
            a.clone(),
            src("vendor.b", 3),
            src("vendor.d", 4),
            src("vendor.e", 5),
        ];
        // Threshold above the number of sources.
        c.method = q(vec![a.clone(), src("vendor.b", 2)], 3, 0);
        assert!(bad(&c));
        // 3 of 5 is a strict majority; 2 of 5 is not; 6 of 5 is impossible.
        c.method = q(five.clone(), 3, 0);
        assert_eq!(c.validate(&r), Ok(()));
        c.method = q(five.clone(), 5, 0);
        assert_eq!(c.validate(&r), Ok(()));
        c.method = q(five.clone(), 2, 0);
        assert!(bad(&c));
        c.method = q(five, 6, 0);
        assert!(bad(&c));
        // The same source twice (another snapshot) is not two sources.
        c.method = q(vec![a.clone(), src("chain.test", 2)], 2, 0);
        assert!(bad(&c));
        // Two restricted sources with identical license, terms and attribution
        // merge (R-RIGHTS-4 refuses only a difference).
        let mut c = base();
        c.source = src("vendor.r1", 2);
        c.rights = restricted("vendor.r1", "terms.vendor.nc-v1");
        c.method = q(vec![a, src("vendor.r1", 2), src("vendor.r4", 3)], 2, 0);
        assert_eq!(c.validate(&r), Ok(()));
    }

    /// R-RIGHTS-2: public claim source cannot launder restricted quorum sources.
    #[test]
    fn quorum_carries_most_restrictive_rights() {
        let r = reg();
        let mut c = base();
        let a = src("chain.test", 1);
        let r1 = src("vendor.r1", 2);
        let r2 = src("vendor.r2", 3);
        c.method = q(vec![a.clone(), r1.clone()], 2, 0);
        assert!(matches!(c.validate(&r), Err(ClaimError::BadQuorum(_))));
        // Restricted sources with different terms cannot be merged.
        c.source = r1.clone();
        c.rights = restricted("vendor.r1", "terms.vendor.nc-v1");
        c.method = q(vec![a.clone(), r1.clone(), r2], 2, 0);
        assert!(matches!(c.validate(&r), Err(ClaimError::BadQuorum(_))));
        // R-RIGHTS-4: same terms, different license: the second license would be dropped.
        let r3 = src("vendor.r3", 4);
        c.method = q(vec![a.clone(), r1.clone(), r3], 2, 0);
        assert!(matches!(c.validate(&r), Err(ClaimError::BadQuorum(_))));
        // R-RIGHTS-5: two Allowed sources whose attribution lines differ.
        let mut att = base();
        att.method = q(
            vec![a.clone(), src("vendor.att1", 5), src("vendor.att2", 6)],
            2,
            0,
        );
        assert!(matches!(att.validate(&r), Err(ClaimError::BadQuorum(_))));
        // Claim on the restricted source itself, carrying its rights: ok.
        c.method = q(vec![a, r1], 2, 0);
        assert_eq!(c.validate(&r), Ok(()));
    }

    /// R-RIGHTS-1: rights come from the registry, not the submitter.
    #[test]
    fn rights_must_match_registry() {
        let r = reg();
        let mut c = base();
        c.rights = DataRights::public_chain(SourceId("chain.other".into()));
        assert_eq!(c.validate(&r), Err(ClaimError::RightsNotRegistered));
        let mut c = base();
        c.source = src("vendor.r1", 2);
        c.rights = DataRights::public_chain(SourceId("vendor.r1".into())); // self-declared public
        assert_eq!(c.validate(&r), Err(ClaimError::RightsNotRegistered));
        let mut c = base();
        c.source = src("vendor.unknown", 2);
        c.rights = DataRights::public_chain(SourceId("vendor.unknown".into()));
        assert!(matches!(c.validate(&r), Err(ClaimError::UnknownSource(_))));
    }

    #[test]
    fn forbidden_rights_block_claim() {
        let mut c = base();
        c.rights.redistribution = Redistribution::Forbidden;
        assert!(matches!(c.validate(&reg()), Err(ClaimError::Rights(_))));
    }

    /// R-ENC-2.
    #[test]
    fn evidence_must_be_sorted_and_unique() {
        let r = reg();
        let mut c = base();
        c.evidence = vec![ev("tx", 2), ev("tx", 1)];
        assert_eq!(c.validate(&r), Err(ClaimError::EvidenceNotCanonical));
        c.evidence = vec![ev("tx", 1), ev("tx", 1)];
        assert_eq!(c.validate(&r), Err(ClaimError::EvidenceNotCanonical));
        c.evidence = vec![ev("tx", 1), ev("tx", 2)];
        assert_eq!(c.validate(&r), Ok(()));
        // A well-formed kind with a zero content hash, and a bad kind with a
        // real hash, are each refused (mutation L4).
        let zero = EvidenceRef {
            kind: "tx".into(),
            content_hash: Hash32::ZERO,
        };
        assert!(!evidence_is_canonical(&[zero]));
        assert!(!evidence_is_canonical(&[ev("Not A Kind", 1)]));
    }

    /// R-PRIV-1, R-LBL-CANON-1, R-SUBJ-1.
    #[test]
    fn subjects_must_be_registered_and_canonical() {
        let r = reg();
        for s in [
            Subject::Entity("person:jane-doe".into()),
            Subject::Entity("entity:jane-doe".into()), // well formed, not registered
            Subject::Entity("Jane Doe".into()),
            Subject::Aggregate("jane-doe.12-main-st.springfield".into()),
            Subject::Address("t:ABC".into()), // non-canonical case
            Subject::Address("zz:JaneDoeHomeWallet".into()), // unknown namespace
            Subject::Instrument("ticker:ibit".into()), // lowercase variant
            Subject::Instrument("isin:US123".into()), // unknown namespace
        ] {
            let mut c = base();
            c.subject = s.clone();
            assert_eq!(c.validate(&r), Err(ClaimError::BadSubject), "{s:?}");
        }
        for s in [
            Subject::Address("t:abc".into()),
            Subject::Aggregate("btc.mainnet".into()),
            Subject::Instrument("ticker:IBIT".into()),
        ] {
            let mut c = base();
            c.subject = s.clone();
            assert_eq!(c.validate(&r), Ok(()), "{s:?}");
        }
    }

    /// R-INGEST-1.
    #[test]
    fn decode_validates() {
        let r = reg();
        let mut c = base();
        c.rights.redistribution = Redistribution::Forbidden;
        let bytes = to_canonical_bytes(&c).expect("encode");
        assert!(matches!(
            Claim::decode(&bytes, &r),
            Err(ClaimDecodeError::Invalid(_))
        ));
        let good = to_canonical_bytes(&base()).expect("encode");
        assert_eq!(Claim::decode(&good, &r).expect("valid"), base());
    }

    /// L2R3-F3: `Claim::decode` never admits a level-2 claim, even one whose
    /// shape `validate` accepts: only the observation intake does.
    #[test]
    fn decode_refuses_level_two_claims() {
        let r = reg();
        let mut c = base();
        c.method = VerificationMethod::ReObserved {
            spec: keccak256(b"s"),
            result: keccak256(b"r"),
        };
        assert_eq!(c.validate(&r), Ok(()));
        let bytes = to_canonical_bytes(&c).expect("encode");
        assert!(matches!(
            Claim::decode(&bytes, &r),
            Err(ClaimDecodeError::NeedsObservationIntake)
        ));
    }

    /// ADR-0028 (L3-F1): `Claim::decode` never admits a level-3 signed-source
    /// claim either, and `validate` refuses a zero key or report hash.
    #[test]
    fn decode_refuses_signed_source_claims() {
        let r = reg();
        let mut c = base();
        c.method = VerificationMethod::SignedSource {
            key: keccak256(b"k"),
            report: keccak256(b"r"),
        };
        assert_eq!(c.validate(&r), Ok(()));
        let bytes = to_canonical_bytes(&c).expect("encode");
        assert!(matches!(
            Claim::decode(&bytes, &r),
            Err(ClaimDecodeError::NeedsSignedSourceIntake)
        ));
        for (key, report) in [
            (Hash32::ZERO, keccak256(b"r")),
            (keccak256(b"k"), Hash32::ZERO),
        ] {
            c.method = VerificationMethod::SignedSource { key, report };
            assert!(matches!(c.validate(&r), Err(ClaimError::BadId(_))));
        }
    }

    #[test]
    fn zero_publish_time_rejected() {
        let mut c = base();
        c.source.snapshot = Snapshot::Document {
            published_unix: 0,
            content_hash: keccak256(b"doc"),
        };
        assert_eq!(
            c.validate(&reg()),
            Err(ClaimError::Source(SourceError::MissingPublishTime))
        );
    }

    /// R-VAL-1, R-SUBJ-2 (metric ids are registered, not free text).
    #[test]
    fn metric_registered_and_kind_enforced() {
        let r = reg();
        let mut c = base();
        c.metric = MetricId("crypto.btc.jane-doe-net-worth".into());
        assert!(matches!(c.validate(&r), Err(ClaimError::UnknownMetric(_))));
        let mut c = base();
        c.value = Value::Int(42); // metric is UInt
        assert_eq!(c.validate(&r), Err(ClaimError::ValueKindMismatch));
        let mut c = base();
        c.metric = MetricId("test.labelled".into()); // needs a label set
        assert_eq!(c.validate(&r), Err(ClaimError::LabelSetRequirement));
        c.label_set = Some(LabelSetRef {
            name: "onyx.entities.btc".into(),
            version: 1,
            content_hash: keccak256(b"ls"),
        });
        assert_eq!(c.validate(&r), Ok(()));
    }

    #[test]
    fn label_set_ref_checked() {
        let mut c = base();
        c.metric = MetricId("test.labelled".into());
        c.label_set = Some(LabelSetRef {
            name: "onyx.entities.btc".into(),
            version: 0,
            content_hash: keccak256(b"ls"),
        });
        assert!(matches!(c.validate(&reg()), Err(ClaimError::BadId(_))));
    }

    #[test]
    fn claim_roundtrips_canonically() {
        let c = base();
        let bytes = to_canonical_bytes(&c).expect("encode");
        let back: Claim = from_canonical_bytes(&bytes).expect("decode");
        assert_eq!(back, c);
        assert_eq!(to_canonical_bytes(&back).expect("encode"), bytes);
    }

    /// L2R-F10: golden Borsh bytes of every verification method and of one
    /// claim, so the encoding (tags 0-6, field order) can never change
    /// unnoticed. Regenerate only with an ADR change: `ONYX_BLESS=1 cargo test
    /// -p onyx-core`.
    #[test]
    fn verification_method_and_claim_encodings_are_pinned() {
        let methods = [
            (
                "Recompute",
                VerificationMethod::Recompute {
                    recipe: keccak256(b"wasm"),
                },
            ),
            (
                "OfficialSourceMatch",
                VerificationMethod::OfficialSourceMatch,
            ),
            (
                "CrossSourceQuorum",
                VerificationMethod::CrossSourceQuorum {
                    sources: vec![src("chain.other", 2), src("chain.test", 1)],
                    threshold: 2,
                    tolerance: 0,
                },
            ),
            (
                "FirstPartyAttestation",
                VerificationMethod::FirstPartyAttestation {
                    scheme: AttestationScheme::Eip191,
                },
            ),
            ("CalibratedJudgment", VerificationMethod::CalibratedJudgment),
            (
                "ReObserved",
                VerificationMethod::ReObserved {
                    spec: keccak256(b"spec"),
                    result: keccak256(b"result"),
                },
            ),
            (
                "SignedSource",
                VerificationMethod::SignedSource {
                    key: keccak256(b"key"),
                    report: keccak256(b"report"),
                },
            ),
        ];
        let mut cases = Vec::new();
        for (tag, (name, m)) in methods.iter().enumerate() {
            let bytes = to_canonical_bytes(m).expect("encode");
            assert_eq!(usize::from(bytes[0]), tag, "{name} tag");
            cases.push(serde_json::json!({
                "method": name,
                "tag": tag,
                "borsh": format!("0x{}", hex::encode(&bytes)),
            }));
        }
        let claim = to_canonical_bytes(&base()).expect("encode claim");
        let doc = serde_json::json!({
            "methods": cases,
            "claim": format!("0x{}", hex::encode(&claim)),
            "claimKeccak": format!("0x{}", keccak256(&claim).to_hex()),
        });
        let text = serde_json::to_string_pretty(&doc).expect("json") + "\n";
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/claims/vectors.json"
        );
        if std::env::var_os("ONYX_BLESS").is_some() {
            std::fs::create_dir_all(std::path::Path::new(path).parent().expect("parent"))
                .expect("mkdir");
            std::fs::write(path, &text).expect("write vectors");
        }
        let on_disk = std::fs::read_to_string(path)
            .expect("fixtures/claims/vectors.json missing; run with ONYX_BLESS=1");
        assert_eq!(on_disk, text, "claim encodings drifted");
    }
}
