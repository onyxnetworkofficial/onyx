//! Review L3R5: retirement and the sequence-equivocation path, both as
//! R-SIG-3 and R-SIG-6 state them (ADR-0028 amendment 5).

use onyx_coordinator::signed::{
    signed_fact_key, DecisionAction, FactStatus, KeyStatus, SignedError, SignedSources,
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

const S: &str = "example.signed";
const O: &str = "data.example.org";

/// L3R5-F1: a report observed at or after a key's retirement point stops
/// counting when the key retires, even though it was accepted while the key
/// was active (future-dated by up to the clock skew, or under a retroactive
/// `at`): it goes to the re-review list and its fact is cleared, so the new
/// key can state it. A report observed before `at` keeps counting.
#[test]
fn retirement_withdraws_reports_observed_at_or_after_at() {
    let reg = reg();
    let (mut s, rv) = SignedSources::with_reviewer("r");
    let a = sk("A");
    let ka = s
        .register_key(&rv, key(S, O, &a), &reg, NOW, "x")
        .expect("register");
    // Future-dated by 200 s, accepted while active.
    let r1 = report(&a, payload(S, ka, 1, 10, NOW + 200));
    assert_eq!(accept(&mut s, &reg, &r1, NOW), Ok(ProofLevel::SourceProven));
    // Observed at NOW - 50, after the retroactive point below.
    let r2 = report(&a, payload(S, ka, 2, 11, NOW - 50));
    assert_eq!(accept(&mut s, &reg, &r2, NOW), Ok(ProofLevel::SourceProven));
    // Observed before the point: keeps counting.
    let r3 = report(&a, payload(S, ka, 3, 12, NOW - 500));
    assert_eq!(accept(&mut s, &reg, &r3, NOW), Ok(ProofLevel::SourceProven));
    s.retire_key(&rv, &ka, NOW - 100, NOW, "rotated at NOW-100")
        .expect("retire");
    assert_eq!(
        s.key_status(&ka),
        Some(KeyStatus::Retired { at: NOW - 100 })
    );
    let later = NOW + 4_000;
    for r in [&r1, &r2] {
        let f = signed_fact_key(&r.payload);
        assert!(s.settleable(&f, later).is_none());
        assert_eq!(s.fact_status(&f), None, "fact cleared for the new key");
        assert!(s.accepted(&r.hash()).is_none());
        assert!(s.review_list().contains(&r.hash()));
    }
    let f3 = signed_fact_key(&r3.payload);
    assert!(s.settleable(&f3, later).is_some());
    assert!(!s.review_list().contains(&r3.hash()));
    let mut want = vec![r1.hash(), r2.hash()];
    want.sort();
    match &s.decisions().last().expect("logged").action {
        DecisionAction::Retire { at, withdrawn, .. } => {
            assert_eq!(*at, NOW - 100);
            let mut got = withdrawn.clone();
            got.sort();
            assert_eq!(got, want);
        }
        other => panic!("{other:?}"),
    }
    // The new key states the cleared fact.
    let b = sk("B");
    let kb = s
        .register_key(&rv, key(S, O, &b), &reg, NOW, "x")
        .expect("register");
    let rb = report(&b, payload(S, kb, 1, 11, NOW - 50));
    assert_eq!(accept(&mut s, &reg, &rb, NOW), Ok(ProofLevel::SourceProven));
}

/// L3R5-F2: a sequence equivocation whose second payload contradicts
/// another key's accepted value for a fact disputes that fact too, and every
/// key involved is suspended: the first signer of the fact as well.
#[test]
fn sequence_equivocation_also_meets_the_fact_book() {
    let reg = reg();
    let (mut s, rv) = SignedSources::with_reviewer("r");
    let (a, k) = (sk("A"), sk("K"));
    let ka = s
        .register_key(&rv, key(S, O, &a), &reg, NOW, "x")
        .expect("register");
    let kk = s
        .register_key(&rv, key(S, O, &k), &reg, NOW, "x")
        .expect("register");
    let ra = report(&a, payload(S, ka, 1, 10, NOW - 60));
    let f = signed_fact_key(&ra.payload);
    assert!(accept(&mut s, &reg, &ra, NOW).is_ok());
    let rk = report(&k, payload(S, kk, 1, 5, NOW - 120));
    let fk = signed_fact_key(&rk.payload);
    assert!(accept(&mut s, &reg, &rk, NOW).is_ok());
    // K contradicts A on fact f, under its used sequence 1.
    let rk2 = report(&k, payload(S, kk, 1, 11, NOW - 60));
    assert_eq!(signed_fact_key(&rk2.payload), f);
    match accept(&mut s, &reg, &rk2, NOW) {
        Err(SignedError::Equivocation { .. }) => {}
        other => panic!("{other:?}"),
    }
    assert!(s.is_suspended(&kk));
    assert!(s.is_suspended(&ka), "every key involved is suspended");
    assert_eq!(
        s.fact_status(&f),
        Some(&FactStatus::Disputed(vec![ra.hash(), rk2.hash()]))
    );
    assert!(s.settleable(&f, NOW + 3_600).is_none());
    // K's own earlier report (its sequence 1) is withdrawn, its fact disputed.
    assert_eq!(
        s.fact_status(&fk),
        Some(&FactStatus::Disputed(vec![rk.hash()]))
    );
    assert!(s.review_list().contains(&ra.hash()));
    assert!(s.review_list().contains(&rk.hash()));
}

/// L3R5-F2: the same value as another key's accepted report is a harmless
/// restatement on the fact book side: only the equivocating key is
/// suspended.
#[test]
fn sequence_equivocation_agreeing_with_a_fact_suspends_only_its_key() {
    let reg = reg();
    let (mut s, rv) = SignedSources::with_reviewer("r");
    let (a, k) = (sk("A"), sk("K"));
    let ka = s
        .register_key(&rv, key(S, O, &a), &reg, NOW, "x")
        .expect("register");
    let kk = s
        .register_key(&rv, key(S, O, &k), &reg, NOW, "x")
        .expect("register");
    let ra = report(&a, payload(S, ka, 1, 10, NOW - 60));
    let f = signed_fact_key(&ra.payload);
    assert!(accept(&mut s, &reg, &ra, NOW).is_ok());
    let rk = report(&k, payload(S, kk, 1, 5, NOW - 120));
    assert!(accept(&mut s, &reg, &rk, NOW).is_ok());
    let rk2 = report(&k, payload(S, kk, 1, 10, NOW - 60));
    assert!(matches!(
        accept(&mut s, &reg, &rk2, NOW),
        Err(SignedError::Equivocation { .. })
    ));
    assert!(s.is_suspended(&kk));
    assert!(!s.is_suspended(&ka));
    assert_eq!(s.fact_status(&f), Some(&FactStatus::Accepted(ra.hash())));
}
