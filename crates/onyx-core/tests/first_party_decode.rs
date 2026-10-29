//! AUD-2-F3: `Claim::decode` must never admit a level-3 `FirstPartyAttestation`
//! claim, because `validate` checks only that evidence is present, not the
//! signature. Before the fix a plain data claim with an arbitrary evidence hash
//! decoded and reported `SourceProven`.
use onyx_core::{
    keccak256, to_canonical_bytes, AttestationScheme, ChainId, Claim, ClaimDecodeError, Confidence,
    DataRights, EvidenceRef, MetricId, MetricInfo, Snapshot, SourceId, SourceRef, StaticRegistry,
    Subject, Value, ValueKind, VerificationMethod,
};

#[test]
fn decode_refuses_an_unverified_first_party_attestation() {
    let mut r = StaticRegistry::new();
    r.add_metric(
        "test.metric",
        MetricInfo {
            value_kind: ValueKind::UInt,
            needs_label_set: false,
        },
    )
    .expect("metric");
    r.add_aggregate("btc.mainnet").expect("aggregate");
    r.add_source(DataRights::public_chain(SourceId("chain.test".into())))
        .expect("source");
    let c = Claim {
        metric: MetricId("test.metric".into()),
        subject: Subject::Aggregate("btc.mainnet".into()),
        value: Value::UInt(123_456_789),
        source: SourceRef {
            source: SourceId("chain.test".into()),
            snapshot: Snapshot::Block {
                chain: ChainId("test".into()),
                height: 1,
                block_hash: keccak256(b"b"),
            },
        },
        method: VerificationMethod::FirstPartyAttestation {
            scheme: AttestationScheme::Eip191,
        },
        label_set: None,
        rights: DataRights::public_chain(SourceId("chain.test".into())),
        evidence: vec![EvidenceRef {
            kind: "anything".into(),
            content_hash: keccak256(b"not a signature"),
        }],
        confidence: Some(Confidence(Confidence::ONE)),
    };
    let bytes = to_canonical_bytes(&c).expect("encode");
    assert!(matches!(
        Claim::decode(&bytes, &r),
        Err(ClaimDecodeError::NeedsAttestationIntake)
    ));
}
