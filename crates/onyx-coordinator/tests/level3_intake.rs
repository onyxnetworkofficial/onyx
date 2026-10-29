//! Level-3 intake under attack (ADR-0028 amendment 1; review L3R-F1..F8).
//! Each test replays a reviewer's attack and asserts it now fails.

use onyx_coordinator::signed::{
    signed_fact_key, FactStatus, KeyStatus, SignedError, SignedSources, RETIRED_GRACE_SECS,
};

fn admin_store() -> (SignedSources, onyx_coordinator::signed::Reviewer) {
    SignedSources::with_reviewer("level3 test reviewer")
}

fn is_equivocation<T>(r: &Result<T, SignedError>) -> bool {
    matches!(r, Err(SignedError::Equivocation { .. }))
}
use onyx_core::signed::REPORT_EVIDENCE;
use onyx_core::{
    keccak256, Claim, DataRights, EntityKind, EvidenceRef, Hash32, KeyProvenance, MetricId,
    MetricInfo, ProofLevel, Redistribution, SignedPayload, SignedReport, Snapshot, SourceId,
    SourceKey, SourceRef, StaticRegistry, Subject, Value, ValueKind, VerificationMethod,
};
use onyx_labels::attest::eip191_digest;
use secp256k1::{Message, Secp256k1, SecretKey};

const NOW: u64 = 1_700_000_000;

fn sk(label: &str) -> SecretKey {
    SecretKey::from_slice(&keccak256(label.as_bytes()).0).expect("test key")
}

fn address(sk: &SecretKey) -> [u8; 20] {
    let pk = secp256k1::PublicKey::from_secret_key(&Secp256k1::new(), sk);
    let h = keccak256(&pk.serialize_uncompressed()[1..]);
    let mut a = [0u8; 20];
    a.copy_from_slice(&h.0[12..]);
    a
}

fn sign(sk: &SecretKey, msg: &[u8]) -> [u8; 65] {
    let digest = Message::from_digest(eip191_digest(msg).0);
    let (rid, bytes) = Secp256k1::new()
        .sign_ecdsa_recoverable(&digest, sk)
        .serialize_compact();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&bytes);
    out[64] = 27 + u8::try_from(rid.to_i32()).expect("recid");
    out
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
    .expect("registry");
    r.add_source(rights("example.signed")).expect("source");
    r
}

fn source_key(signer: &SecretKey) -> SourceKey {
    SourceKey {
        source: SourceId("example.signed".into()),
        signer: address(signer),
        provenance: KeyProvenance::WellKnown {
            origin: "data.example.org".into(),
            document_hash: keccak256(b"keys document"),
            fetched_unix: NOW - 10_000,
        },
    }
}

fn payload(key: Hash32, seq: u64, value: u128, observed: u64) -> SignedPayload {
    SignedPayload {
        source: SourceId("example.signed".into()),
        key,
        metric: MetricId("example.signed.reserve".into()),
        subject: Subject::Entity("entity:example".into()),
        value: Value::UInt(value),
        observed_unix: observed,
        sequence: seq,
    }
}

fn report(sk: &SecretKey, p: SignedPayload) -> SignedReport {
    let signature = sign(sk, p.message().as_bytes());
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

fn suspended(s: &SignedSources, id: &Hash32) -> bool {
    s.is_suspended(id)
}

/// L3R-F1: a retired key stolen later cannot feed backdated forgeries once
/// its grace period is over; reports signed just before rotation still
/// arrive within it.
#[test]
fn a_retired_key_is_dead_after_its_grace_period() {
    let reg = reg();
    let (mut s, admin) = admin_store();
    let old = sk("old key");
    // Registered before its first report: the store's clock never steps back (L3R-F8).
    let k_old = s
        .register_key(&admin, source_key(&old), &reg, NOW - 3_000, "test")
        .expect("reg");
    let honest = report(&old, payload(k_old, 1, 100, NOW - 3_600));
    assert_eq!(
        accept(&mut s, &reg, &honest, NOW - 3_000),
        Ok(ProofLevel::SourceProven)
    );
    s.retire_key(&admin, &k_old, NOW, NOW, "rotation")
        .expect("retire");
    // Signed before rotation, delivered within the grace: accepted.
    let late = report(&old, payload(k_old, 2, 101, NOW - 10));
    assert_eq!(
        accept(&mut s, &reg, &late, NOW + 60),
        Ok(ProofLevel::SourceProven)
    );
    // Right at the end of the grace (the clock only moves forward, L3R-F8).
    let forged2 = report(&old, payload(k_old, 998, 1, NOW - 2));
    assert_eq!(
        accept(&mut s, &reg, &forged2, NOW + RETIRED_GRACE_SECS),
        Err(SignedError::KeyRetired)
    );
    // Six days later a thief signs a never-stated fact dated before rotation.
    let forged = report(&old, payload(k_old, 999, 1, NOW - 1));
    assert_eq!(
        accept(&mut s, &reg, &forged, NOW + 6 * 86_400),
        Err(SignedError::KeyRetired)
    );
}

/// L3R-F2: the same value stated again is a harmless restatement: no
/// suspension, the first report stays accepted.
#[test]
fn a_same_value_restatement_suspends_nobody() {
    let reg = reg();
    let (mut s, reviewer) = SignedSources::with_reviewer("test reviewer");
    let k = sk("honest");
    let id = s
        .register_key(&reviewer, source_key(&k), &reg, NOW, "test")
        .expect("reg");
    let a = report(&k, payload(id, 1, 42, NOW - 60));
    let b = report(&k, payload(id, 2, 42, NOW - 60));
    assert_eq!(accept(&mut s, &reg, &a, NOW), Ok(ProofLevel::SourceProven));
    assert_eq!(
        accept(&mut s, &reg, &b, NOW),
        Err(SignedError::AlreadyStated)
    );
    assert_eq!(s.key_status(&id), Some(KeyStatus::Active));
    assert!(s.accepted(&a.hash()).is_some());
    assert!(s.equivocations().is_empty());
    // Dual-signing the same value with the new key during a rotation too.
    let (mut s, reviewer) = SignedSources::with_reviewer("test reviewer");
    let k2 = sk("new");
    let id1 = s
        .register_key(&reviewer, source_key(&k), &reg, NOW, "test")
        .expect("reg");
    let id2 = s
        .register_key(&reviewer, source_key(&k2), &reg, NOW, "test")
        .expect("reg");
    let a = report(&k, payload(id1, 1, 42, NOW - 60));
    let b = report(&k2, payload(id2, 1, 42, NOW - 60));
    assert_eq!(accept(&mut s, &reg, &a, NOW), Ok(ProofLevel::SourceProven));
    assert_eq!(
        accept(&mut s, &reg, &b, NOW),
        Err(SignedError::AlreadyStated)
    );
    assert_eq!(s.key_status(&id1), Some(KeyStatus::Active));
    assert_eq!(s.key_status(&id2), Some(KeyStatus::Active));
}

/// L3R-F3: a thief holding key A states a false value first; when honest key
/// B states the truth, BOTH keys are suspended, the lie is withdrawn and the
/// fact is disputed. Once A is marked compromised the dispute resolves to B's
/// report on its own and B is reinstated (L3R2-F3).
#[test]
fn a_thief_first_lie_is_withdrawn_and_the_fact_disputed() {
    let reg = reg();
    let (mut s, admin) = admin_store();
    let a = sk("stolen A");
    let b = sk("honest B");
    let ia = s
        .register_key(&admin, source_key(&a), &reg, NOW, "test")
        .expect("reg");
    let ib = s
        .register_key(&admin, source_key(&b), &reg, NOW, "test")
        .expect("reg");
    let lie = report(&a, payload(ia, 1, 1, NOW + 299));
    assert_eq!(
        accept(&mut s, &reg, &lie, NOW),
        Ok(ProofLevel::SourceProven)
    );
    let truth = report(&b, payload(ib, 1, 1_000, NOW + 299));
    let fact = signed_fact_key(&lie.payload);
    assert_eq!(
        accept(&mut s, &reg, &truth, NOW + 299),
        Err(SignedError::Equivocation {
            fact,
            withdrawn: vec![lie.hash()],
        })
    );
    assert!(suspended(&s, &ia) && suspended(&s, &ib));
    assert!(s.accepted(&lie.hash()).is_none());
    assert!(s.review_list().contains(&lie.hash()));
    assert_eq!(
        s.fact_status(&fact),
        Some(&FactStatus::Disputed(vec![lie.hash(), truth.hash()]))
    );
    // A reviewer may not keep a report whose key is suspended.
    assert_eq!(
        s.resolve_fact(&admin, &fact, Some(truth.hash()), NOW + 300, "early"),
        Err(SignedError::BadResolution)
    );
    // Review: A is found stolen; its report leaves the dispute. Nothing is
    // restored on its own (amendment 3): the reviewer reinstates B and keeps
    // its report.
    s.mark_compromised(&admin, &ia, NOW - 1_000, NOW + 400, "theft reported")
        .expect("compromise");
    assert_eq!(
        s.fact_status(&fact),
        Some(&FactStatus::Disputed(vec![truth.hash()]))
    );
    assert!(suspended(&s, &ib));
    s.reinstate_key(&admin, &ib, NOW + 401, "B honest")
        .expect("reinstate");
    s.resolve_fact(&admin, &fact, Some(truth.hash()), NOW + 402, "keep B")
        .expect("resolve");
    assert_eq!(
        s.fact_status(&fact),
        Some(&FactStatus::Accepted(truth.hash()))
    );
    assert!(s.accepted(&truth.hash()).is_some());
    assert!(!suspended(&s, &ib));
    assert_eq!(s.key_status(&ib), Some(KeyStatus::Active));
    assert!(matches!(
        s.key_status(&ia),
        Some(KeyStatus::Compromised { .. })
    ));
    assert_eq!(
        s.resolve_fact(&admin, &fact, Some(lie.hash()), NOW + 500, "too late"),
        Err(SignedError::NotDisputed)
    );
}

/// L3R-F4: after key A is marked compromised, what it stated from the
/// compromise point is withdrawn, so the source's new key can state the true
/// value.
#[test]
fn a_new_key_restates_the_truth_after_a_theft() {
    let reg = reg();
    let (mut s, admin) = admin_store();
    let a = sk("stolen A");
    let ia = s
        .register_key(&admin, source_key(&a), &reg, NOW, "test")
        .expect("reg");
    let lie = report(&a, payload(ia, 1, 1, NOW - 60));
    assert_eq!(
        accept(&mut s, &reg, &lie, NOW),
        Ok(ProofLevel::SourceProven)
    );
    let review = s
        .mark_compromised(&admin, &ia, NOW - 100, NOW + 10, "stolen")
        .expect("mark");
    assert_eq!(review, vec![lie.hash()]);
    assert!(s.accepted(&lie.hash()).is_none());
    assert_eq!(s.fact_status(&signed_fact_key(&lie.payload)), None);
    let b = sk("new B");
    let ib = s
        .register_key(&admin, source_key(&b), &reg, NOW + 10, "test")
        .expect("reg");
    let truth = report(&b, payload(ib, 1, 1_000, NOW - 60));
    assert_eq!(
        accept(&mut s, &reg, &truth, NOW + 20),
        Ok(ProofLevel::SourceProven)
    );
    assert_eq!(s.key_status(&ib), Some(KeyStatus::Active));
}

/// L3R-F5: a later report submitted first does not block earlier ones.
#[test]
fn a_later_sequence_first_censors_nothing() {
    let reg = reg();
    let (mut s, reviewer) = SignedSources::with_reviewer("test reviewer");
    let k = sk("honest");
    let id = s
        .register_key(&reviewer, source_key(&k), &reg, NOW, "test")
        .expect("reg");
    let early: Vec<_> = (1..10)
        .map(|i| report(&k, payload(id, i, 40 + u128::from(i), NOW - 1_000 + i)))
        .collect();
    let last = report(&k, payload(id, 10, 50, NOW - 10));
    assert_eq!(
        accept(&mut s, &reg, &last, NOW),
        Ok(ProofLevel::SourceProven)
    );
    for r in &early {
        assert_eq!(accept(&mut s, &reg, r, NOW), Ok(ProofLevel::SourceProven));
    }
    // Every sequence is still single-use.
    assert_eq!(
        accept(&mut s, &reg, &early[0], NOW),
        Err(SignedError::Replay)
    );
}

/// L3R-F6: a contradicting value under an older, unused sequence is kept as
/// equivocation evidence and suspends the key.
#[test]
fn a_contradiction_under_an_older_sequence_is_evidence() {
    let reg = reg();
    let (mut s, reviewer) = SignedSources::with_reviewer("test reviewer");
    let k = sk("equivocator");
    let id = s
        .register_key(&reviewer, source_key(&k), &reg, NOW, "test")
        .expect("reg");
    let a = report(&k, payload(id, 5, 42, NOW - 60));
    let b = report(&k, payload(id, 3, 43, NOW - 60));
    assert_eq!(accept(&mut s, &reg, &a, NOW), Ok(ProofLevel::SourceProven));
    assert!(is_equivocation(&accept(&mut s, &reg, &b, NOW)));
    assert_eq!(s.equivocations().len(), 1);
    assert!(suspended(&s, &id));
}

/// L3R-F8: the top sequence is reserved, so no report can brick a key.
#[test]
fn the_top_sequence_is_reserved() {
    let reg = reg();
    let (mut s, reviewer) = SignedSources::with_reviewer("test reviewer");
    let k = sk("k");
    let id = s
        .register_key(&reviewer, source_key(&k), &reg, NOW, "test")
        .expect("reg");
    let a = report(&k, payload(id, u64::MAX, 1, NOW - 60));
    assert_eq!(
        accept(&mut s, &reg, &a, NOW),
        Err(SignedError::SequenceReserved)
    );
    let b = report(&k, payload(id, 0, 2, NOW - 30));
    assert_eq!(accept(&mut s, &reg, &b, NOW), Ok(ProofLevel::SourceProven));
}

/// L3R-F7: an origin publishes keys for one source only.
#[test]
fn another_source_cannot_use_a_taken_origin() {
    let mut r = reg();
    r.add_source(rights("example.other")).expect("source b");
    let (mut s, reviewer) = SignedSources::with_reviewer("test reviewer");
    let a = sk("a");
    s.register_key(&reviewer, source_key(&a), &r, NOW, "test")
        .expect("a");
    let b = sk("b");
    let mut kb = source_key(&b);
    kb.source = SourceId("example.other".into());
    assert_eq!(
        s.register_key(&reviewer, kb, &r, NOW, "test"),
        Err(SignedError::OriginTaken)
    );
}

/// A disputed fact records further reports as evidence and accepts none.
#[test]
fn a_disputed_fact_accepts_nothing_until_review() {
    let reg = reg();
    let (mut s, admin) = admin_store();
    let a = sk("a");
    let b = sk("b");
    let c = sk("c");
    let ia = s
        .register_key(&admin, source_key(&a), &reg, NOW, "test")
        .expect("a");
    let ib = s
        .register_key(&admin, source_key(&b), &reg, NOW, "test")
        .expect("b");
    let ic = s
        .register_key(&admin, source_key(&c), &reg, NOW, "test")
        .expect("c");
    let ra = report(&a, payload(ia, 1, 1, NOW - 60));
    let rb = report(&b, payload(ib, 1, 2, NOW - 60));
    let rc = report(&c, payload(ic, 1, 2, NOW - 60));
    assert_eq!(accept(&mut s, &reg, &ra, NOW), Ok(ProofLevel::SourceProven));
    assert!(is_equivocation(&accept(&mut s, &reg, &rb, NOW)));
    // C joins siding with B against A: kept as evidence, C suspended
    // (L3R3-F2), nothing accepted.
    assert!(is_equivocation(&accept(&mut s, &reg, &rc, NOW)));
    assert!(suspended(&s, &ic));
    let fact = signed_fact_key(&ra.payload);
    assert_eq!(
        s.fact_status(&fact),
        Some(&FactStatus::Disputed(vec![ra.hash(), rb.hash(), rc.hash()]))
    );
    assert!(s.accepted(&rc.hash()).is_none());
    // A reviewer clears the fact; it can then be stated again.
    s.resolve_fact(&admin, &fact, None, NOW + 10, "cleared on review")
        .expect("clear");
    assert_eq!(s.fact_status(&fact), None);
    assert_eq!(
        s.resolve_fact(&admin, &fact, None, NOW + 10, "again"),
        Err(SignedError::NotDisputed)
    );
}
