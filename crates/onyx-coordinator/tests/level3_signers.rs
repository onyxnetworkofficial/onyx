//! One signer, one registration; recorded payloads are replays (ADR-0028
//! amendment 4, review L3R4-F1/F2).
use onyx_coordinator::signed::{
    signed_fact_key, FactStatus, KeyStatus, SignedError, SignedSources,
};
use onyx_core::signed::REPORT_EVIDENCE;
use onyx_core::{
    keccak256, Claim, DataRights, EntityKind, EvidenceRef, Hash32, KeyProvenance, MetricId,
    MetricInfo, ProofLevel, Redistribution, SignedPayload, SignedReport, Snapshot, SourceId,
    SourceKey, SourceRef, StaticRegistry, Subject, Value, ValueKind, VerificationMethod,
};
use onyx_labels::attest::eip191_digest;
use secp256k1::{Message, Secp256k1, SecretKey};

const NOW: u64 = 1_700_000_000;
fn sk(l: &str) -> SecretKey {
    SecretKey::from_slice(&keccak256(l.as_bytes()).0).expect("test setup")
}
fn address(sk: &SecretKey) -> [u8; 20] {
    let pk = secp256k1::PublicKey::from_secret_key(&Secp256k1::new(), sk);
    let h = keccak256(&pk.serialize_uncompressed()[1..]);
    let mut a = [0u8; 20];
    a.copy_from_slice(&h.0[12..]);
    a
}
fn sign(sk: &SecretKey, msg: &[u8]) -> [u8; 65] {
    let d = Message::from_digest(eip191_digest(msg).0);
    let (rid, b) = Secp256k1::new()
        .sign_ecdsa_recoverable(&d, sk)
        .serialize_compact();
    let mut o = [0u8; 65];
    o[..64].copy_from_slice(&b);
    o[64] = 27 + rid.to_i32() as u8;
    o
}
fn rights(id: &str) -> DataRights {
    DataRights {
        license: "CC0-1.0".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId(id.into()),
        attribution: None,
        terms: String::new(),
    }
}
fn reg() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    r.add_metric(
        "example.signed.reserve",
        MetricInfo {
            value_kind: ValueKind::UInt,
            needs_label_set: false,
        },
    )
    .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
    .expect("test setup");
    r.add_source(rights("example.signed")).expect("test setup");
    r.add_source(rights("example.other")).expect("test setup");
    r
}
fn key(src: &str, origin: &str, s: &SecretKey) -> SourceKey {
    SourceKey {
        source: SourceId(src.into()),
        signer: address(s),
        provenance: KeyProvenance::WellKnown {
            origin: origin.into(),
            document_hash: keccak256(b"doc"),
            fetched_unix: NOW - 10_000,
        },
    }
}
fn payload(src: &str, key: Hash32, seq: u64, v: u128, obs: u64) -> SignedPayload {
    SignedPayload {
        source: SourceId(src.into()),
        key,
        metric: MetricId("example.signed.reserve".into()),
        subject: Subject::Entity("entity:example".into()),
        value: Value::UInt(v),
        observed_unix: obs,
        sequence: seq,
    }
}
fn report(s: &SecretKey, p: SignedPayload) -> SignedReport {
    let signature = sign(s, p.message().as_bytes());
    SignedReport {
        payload: p,
        signature,
    }
}
fn claim_for(r: &SignedReport) -> Claim {
    let h = r.hash();
    Claim {
        metric: r.payload.metric.clone(),
        subject: r.payload.subject.clone(),
        value: r.payload.value.clone(),
        source: SourceRef {
            source: r.payload.source.clone(),
            snapshot: Snapshot::Document {
                published_unix: r.payload.observed_unix,
                content_hash: h,
            },
        },
        method: VerificationMethod::SignedSource {
            key: r.payload.key,
            report: h,
        },
        label_set: None,
        rights: rights(&r.payload.source.0),
        evidence: vec![EvidenceRef {
            kind: REPORT_EVIDENCE.into(),
            content_hash: h,
        }],
        confidence: None,
    }
}
fn accept(
    s: &mut SignedSources,
    reg: &StaticRegistry,
    r: &SignedReport,
    now: u64,
) -> Result<ProofLevel, SignedError> {
    s.accept_claim(&claim_for(r), r, reg, now).map(|i| i.level)
}

/// L3R4-F1: one private key can be registered once, under one source. Its
/// compromise can therefore never leave it active elsewhere, and a
/// compromised signer is never registered again under a new source.
#[test]
fn a_signer_registers_once_under_one_source() {
    let reg = reg();
    let (mut s, rv) = SignedSources::with_reviewer("r");
    let x = sk("shared-hsm-key");
    let a = s
        .register_key(
            &rv,
            key("example.signed", "a.example.org", &x),
            &reg,
            NOW,
            "a",
        )
        .expect("test setup");
    assert_eq!(
        s.register_key(
            &rv,
            key("example.other", "b.example.org", &x),
            &reg,
            NOW,
            "b"
        ),
        Err(SignedError::SignerTaken)
    );
    s.mark_compromised(&rv, &a, 0, NOW, "stolen")
        .expect("test setup");
    assert!(matches!(
        s.key_status(&a),
        Some(KeyStatus::Compromised { .. })
    ));
    // Still refused after the compromise, under any source or origin.
    assert_eq!(
        s.register_key(
            &rv,
            key("example.other", "c.example.org", &x),
            &reg,
            NOW,
            "again"
        ),
        Err(SignedError::SignerTaken)
    );
    // The thief's report for the other source has no key to sign under.
    let other_id = key("example.other", "b.example.org", &x).id();
    let lie = report(&x, payload("example.other", other_id, 1, 666, NOW - 10));
    assert_eq!(
        accept(&mut s, &reg, &lie, NOW),
        Err(SignedError::UnknownKey)
    );
    // A different signer for the other source registers fine.
    let y = sk("other-source-key");
    assert!(s
        .register_key(
            &rv,
            key("example.other", "b.example.org", &y),
            &reg,
            NOW,
            "b"
        )
        .is_ok());
}

/// L3R4-F2: the second report of a sequence equivocation is recorded, so a
/// relayer replaying it after the reviewer reinstated the key and resolved
/// the fact gets `Replay` and changes nothing.
#[test]
fn a_replayed_equivocation_report_changes_nothing() {
    let reg = reg();
    let (mut s, rv) = SignedSources::with_reviewer("r");
    let k = sk("k");
    let id = s
        .register_key(
            &rv,
            key("example.signed", "a.example.org", &k),
            &reg,
            NOW,
            "k",
        )
        .expect("test setup");
    let r1 = report(&k, payload("example.signed", id, 7, 10, NOW - 100));
    let fact = signed_fact_key(&r1.payload);
    assert_eq!(accept(&mut s, &reg, &r1, NOW), Ok(ProofLevel::SourceProven));
    let r2 = report(&k, payload("example.signed", id, 7, 10, NOW - 50)); // other fact, same seq
    assert!(matches!(
        accept(&mut s, &reg, &r2, NOW),
        Err(SignedError::Equivocation { .. })
    ));
    assert!(matches!(
        s.fact_status(&fact),
        Some(FactStatus::Disputed(_))
    ));
    s.reinstate_key(&rv, &id, NOW + 10, "benign")
        .expect("test setup");
    s.resolve_fact(&rv, &fact, Some(r1.hash()), NOW + 10, "keep")
        .expect("test setup");
    assert_eq!(s.fact_status(&fact), Some(&FactStatus::Accepted(r1.hash())));
    let n0 = s.equivocations().len();
    for r in [&r2, &r1] {
        assert_eq!(accept(&mut s, &reg, r, NOW + 20), Err(SignedError::Replay));
    }
    assert!(!s.is_suspended(&id));
    assert_eq!(s.fact_status(&fact), Some(&FactStatus::Accepted(r1.hash())));
    assert_eq!(s.equivocations().len(), n0);
}
