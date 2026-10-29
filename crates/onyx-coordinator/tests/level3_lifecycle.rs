//! Level-3 key life cycle and fact book (ADR-0028 amendments 2 and 3; reviews
//! L3R2-F1..F7, L3R3-F1..F4, F6). The life cycle only moves forward (Active -> Retired ->
//! Compromised); suspension is a separate flag. `every_state_and_event_pair`
//! walks every sequence of up to five events against a reference model.

use onyx_coordinator::signed::{
    signed_fact_key, DecisionAction, FactStatus, KeyStatus, Reviewer, SignedError, SignedSources,
    DISPUTE_WINDOW_SECS, MAX_REPORT_AGE_SECS, PRUNE_AFTER_SECS, RETIRED_GRACE_SECS,
};
use onyx_core::signed::REPORT_EVIDENCE;
use onyx_core::{
    keccak256, Claim, DataRights, EntityKind, EvidenceRef, Hash32, KeyProvenance, MetricId,
    MetricInfo, ProofLevel, Redistribution, SignedPayload, SignedReport, Snapshot, SourceId,
    SourceKey, SourceRef, StaticRegistry, Subject, Value, ValueKind, VerificationMethod,
};
use onyx_labels::attest::eip191_digest;
use secp256k1::{Message, Secp256k1, SecretKey};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};

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

/// Signatures already made, by (message, signer label).
type SigCache = HashMap<(Vec<u8>, String), [u8; 65]>;

thread_local! {
    static SIGS: RefCell<SigCache> = RefCell::new(HashMap::new());
}

/// EIP-191 signature, cached by (message, signer label): the exhaustive test
/// signs the same payloads many times.
fn sign(label: &str, msg: &[u8]) -> [u8; 65] {
    SIGS.with(|c| {
        *c.borrow_mut()
            .entry((msg.to_vec(), label.to_string()))
            .or_insert_with(|| {
                let digest = Message::from_digest(eip191_digest(msg).0);
                let (rid, bytes) = Secp256k1::new()
                    .sign_ecdsa_recoverable(&digest, &sk(label))
                    .serialize_compact();
                let mut out = [0u8; 65];
                out[..64].copy_from_slice(&bytes);
                out[64] = 27 + u8::try_from(rid.to_i32()).expect("recid");
                out
            })
    })
}

fn rights() -> DataRights {
    DataRights {
        license: "CC0-1.0".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId("example.signed".into()),
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
    r.add_source(rights()).expect("source");
    r
}

fn source_key(label: &str) -> SourceKey {
    SourceKey {
        source: SourceId("example.signed".into()),
        signer: address(&sk(label)),
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

fn report(label: &str, p: SignedPayload) -> SignedReport {
    let signature = sign(label, p.message().as_bytes());
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
        rights: rights(),
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

fn is_equivocation<T>(r: &Result<T, SignedError>) -> bool {
    matches!(r, Err(SignedError::Equivocation { .. }))
}

fn store() -> (SignedSources, Reviewer) {
    SignedSources::with_reviewer("lifecycle reviewer")
}

/// L3R2-F1: reinstating a suspended retired key clears the suspension only;
/// the key stays retired and cannot sign fresh facts later.
#[test]
fn reinstating_never_resurrects_a_retired_key() {
    let reg = reg();
    let (mut s, admin) = store();
    let io = s
        .register_key(&admin, source_key("old"), &reg, NOW, "test")
        .expect("reg");
    let inew = s
        .register_key(&admin, source_key("new"), &reg, NOW, "test")
        .expect("reg");
    let truth = report("new", payload(inew, 1, 10, NOW - 60));
    assert_eq!(
        accept(&mut s, &reg, &truth, NOW),
        Ok(ProofLevel::SourceProven)
    );
    s.retire_key(&admin, &io, NOW, NOW, "rotation")
        .expect("retire");
    // Within the grace, the (stolen) old key contradicts: both suspended.
    let lie = report("old", payload(io, 1, 11, NOW - 60));
    assert!(is_equivocation(&accept(&mut s, &reg, &lie, NOW + 10)));
    assert!(s.is_suspended(&io));
    s.reinstate_key(&admin, &io, NOW + 20, "cleared")
        .expect("reinstate");
    assert!(!s.is_suspended(&io));
    assert_eq!(s.key_status(&io), Some(KeyStatus::Retired { at: NOW }));
    // A day later the retired key signs a fresh fact: refused.
    let t = NOW + 86_400;
    let fresh = report("old", payload(io, 2, 999, t));
    assert_eq!(
        accept(&mut s, &reg, &fresh, t),
        Err(SignedError::KeyRetired)
    );
}

/// L3R2-F2: a compromise point after now is refused; `since = 0` is allowed
/// and recorded in the decision log.
#[test]
fn a_compromise_point_cannot_lie_in_the_future_and_every_call_is_logged() {
    let reg = reg();
    let (mut s, admin) = store();
    let ia = s
        .register_key(&admin, source_key("stolen"), &reg, NOW, "test")
        .expect("reg");
    let lie = report("stolen", payload(ia, 1, 1, NOW - 60));
    assert_eq!(
        accept(&mut s, &reg, &lie, NOW),
        Ok(ProofLevel::SourceProven)
    );
    assert_eq!(
        s.mark_compromised(&admin, &ia, NOW + 1_000_000, NOW + 10, "forward"),
        Err(SignedError::TimeInFuture)
    );
    assert_eq!(s.key_status(&ia), Some(KeyStatus::Active));
    // Only the registration is logged so far.
    assert_eq!(s.decisions().len(), 1);
    let review = s
        .mark_compromised(&admin, &ia, 0, NOW + 20, "whole history")
        .expect("mark");
    assert_eq!(review, vec![lie.hash()]);
    let d = s.decisions().last().expect("logged");
    assert_eq!(d.by, "lifecycle reviewer");
    assert_eq!(d.at, NOW + 20);
    assert_eq!(d.reason, "whole history");
    assert_eq!(
        d.action,
        DecisionAction::Compromise {
            key: ia,
            since: 0,
            withdrawn: vec![lie.hash()],
        }
    );
    // A later, later-dated mark cannot narrow what was withdrawn.
    s.mark_compromised(&admin, &ia, NOW, NOW + 30, "narrow")
        .expect("mark again");
    assert_eq!(
        s.key_status(&ia),
        Some(KeyStatus::Compromised {
            since: 0,
            recorded_at: NOW + 20,
        })
    );
}

/// L3R2-F3 as revised by amendment 3 (L3R3-F1, F3): after a theft the
/// thief's reports leave the dispute, but nothing is restored on its own:
/// the fact stays disputed and the honest key suspended until a reviewer
/// resolves the fact and reinstates the key. Every step is logged under the
/// reviewer's name; the store never decides on its own.
#[test]
fn recovery_after_a_theft_is_reviewer_only() {
    let reg = reg();
    let (mut s, admin) = store();
    let ia = s
        .register_key(&admin, source_key("stolen"), &reg, NOW, "test")
        .expect("reg");
    let ib = s
        .register_key(&admin, source_key("honest"), &reg, NOW, "test")
        .expect("reg");
    let truth = report("honest", payload(ib, 1, 10, NOW - 60));
    assert_eq!(
        accept(&mut s, &reg, &truth, NOW),
        Ok(ProofLevel::SourceProven)
    );
    let lie = report("stolen", payload(ia, 1, 11, NOW - 60));
    assert!(is_equivocation(&accept(&mut s, &reg, &lie, NOW + 1)));
    let fact = signed_fact_key(&truth.payload);
    s.mark_compromised(&admin, &ia, NOW, NOW + 2, "theft")
        .expect("mark");
    // The lie left the dispute; the honest report stays disputed.
    assert_eq!(
        s.fact_status(&fact),
        Some(&FactStatus::Disputed(vec![truth.hash()]))
    );
    assert!(s.is_suspended(&ib), "no automatic reinstatement");
    assert!(s
        .settleable(&fact, NOW + 10 * DISPUTE_WINDOW_SECS)
        .is_none());
    // The reviewer reinstates the honest key, then keeps its report.
    s.reinstate_key(&admin, &ib, NOW + 3, "honest key")
        .expect("reinstate");
    s.resolve_fact(&admin, &fact, Some(truth.hash()), NOW + 4, "keep truth")
        .expect("resolve");
    assert_eq!(
        s.fact_status(&fact),
        Some(&FactStatus::Accepted(truth.hash()))
    );
    assert!(s.decisions().iter().all(|d| d.by == "lifecycle reviewer"));
    let ic = s
        .register_key(&admin, source_key("new"), &reg, NOW + 5, "rotation")
        .expect("reg");
    let again = report("new", payload(ic, 1, 10, NOW - 60));
    assert_eq!(
        accept(&mut s, &reg, &again, NOW + 6),
        Err(SignedError::AlreadyStated)
    );
    let next = report("honest", payload(ib, 2, 12, NOW - 30));
    assert_eq!(
        accept(&mut s, &reg, &next, NOW + 7),
        Ok(ProofLevel::SourceProven)
    );
}

/// L3R3-F1: a key that joins a dispute and then contradicts itself stays
/// suspended when the other side is compromised; nothing frees it but a
/// reviewer, and until then it signs nothing.
#[test]
fn a_self_contradicting_key_is_never_freed_without_a_reviewer() {
    let reg = reg();
    let (mut s, admin) = store();
    let a = s
        .register_key(&admin, source_key("A"), &reg, NOW, "test")
        .expect("reg");
    let b = s
        .register_key(&admin, source_key("B"), &reg, NOW, "test")
        .expect("reg");
    let k = s
        .register_key(&admin, source_key("K"), &reg, NOW, "test")
        .expect("reg");
    let a1 = report("A", payload(a, 1, 10, NOW - 60));
    assert_eq!(accept(&mut s, &reg, &a1, NOW), Ok(ProofLevel::SourceProven));
    let b1 = report("B", payload(b, 1, 20, NOW - 60));
    assert!(is_equivocation(&accept(&mut s, &reg, &b1, NOW + 1)));
    // K joins agreeing with A, but A and B differ: K took part in a
    // contradiction and is suspended (L3R3-F2).
    let k1 = report("K", payload(k, 1, 10, NOW - 60));
    assert!(is_equivocation(&accept(&mut s, &reg, &k1, NOW + 2)));
    assert!(s.is_suspended(&k));
    let k2 = report("K", payload(k, 2, 30, NOW - 60));
    assert_eq!(
        accept(&mut s, &reg, &k2, NOW + 3),
        Err(SignedError::KeySuspended)
    );
    s.mark_compromised(&admin, &b, NOW - 1_000, NOW + 4, "B stolen")
        .expect("mark");
    let fact = signed_fact_key(&a1.payload);
    assert!(matches!(
        s.fact_status(&fact),
        Some(FactStatus::Disputed(_))
    ));
    assert!(s.is_suspended(&k) && s.is_suspended(&a));
    let fresh = report("K", payload(k, 3, 999, NOW + 100));
    assert_eq!(
        accept(&mut s, &reg, &fresh, NOW + 200),
        Err(SignedError::KeySuspended)
    );
}

/// L3R3-F2: a key joining an open dispute with a value that differs from any
/// report in it is suspended and recorded as equivocation evidence; a joiner
/// agreeing with every report is only recorded.
#[test]
fn a_contradicting_joiner_is_suspended() {
    let reg = reg();
    let (mut s, admin) = store();
    let k = s
        .register_key(&admin, source_key("K"), &reg, NOW, "test")
        .expect("reg");
    let j = s
        .register_key(&admin, source_key("J"), &reg, NOW, "test")
        .expect("reg");
    let m = s
        .register_key(&admin, source_key("M"), &reg, NOW, "test")
        .expect("reg");
    let k1 = report("K", payload(k, 1, 10, NOW - 60));
    assert_eq!(accept(&mut s, &reg, &k1, NOW), Ok(ProofLevel::SourceProven));
    // Same sequence, another fact: a sequence equivocation leaves K's fact
    // disputed with one report.
    let k1b = report("K", payload(k, 1, 11, NOW - 59));
    assert!(is_equivocation(&accept(&mut s, &reg, &k1b, NOW + 1)));
    let fact = signed_fact_key(&k1.payload);
    assert_eq!(
        s.fact_status(&fact),
        Some(&FactStatus::Disputed(vec![k1.hash()]))
    );
    // M agrees with the only report: recorded, not suspended.
    let m1 = report("M", payload(m, 1, 10, NOW - 60));
    assert_eq!(
        accept(&mut s, &reg, &m1, NOW + 2),
        Err(SignedError::FactDisputed)
    );
    assert!(!s.is_suspended(&m));
    // J differs: suspended, evidence kept.
    let before = s.equivocations().len();
    let j1 = report("J", payload(j, 1, 20, NOW - 60));
    assert!(is_equivocation(&accept(&mut s, &reg, &j1, NOW + 3)));
    assert!(s.is_suspended(&j));
    assert_eq!(s.equivocations().len(), before + 1);
    // Compromising J restores nothing: the reviewer still decides, and may
    // not keep K's report while K is suspended.
    s.mark_compromised(&admin, &j, NOW - 1_000, NOW + 4, "J stolen")
        .expect("mark");
    assert!(matches!(
        s.fact_status(&fact),
        Some(FactStatus::Disputed(_))
    ));
    assert_eq!(
        s.resolve_fact(&admin, &fact, Some(k1.hash()), NOW + 5, "keep k"),
        Err(SignedError::BadResolution)
    );
}

/// L3R3-F3: a retired key's in-grace report in a dispute is never
/// re-accepted after the grace by a compromise of the other side; a
/// reviewer resolution then refuses it too (the rule `resolve_fact` uses).
#[test]
fn a_retired_report_is_not_re_accepted_after_the_grace() {
    let reg = reg();
    let (mut s, admin) = store();
    let o = s
        .register_key(&admin, source_key("O"), &reg, NOW, "test")
        .expect("reg");
    let n = s
        .register_key(&admin, source_key("N"), &reg, NOW, "test")
        .expect("reg");
    s.retire_key(&admin, &o, NOW, NOW, "rotate")
        .expect("retire");
    let o1 = report("O", payload(o, 1, 10, NOW - 60));
    assert_eq!(
        accept(&mut s, &reg, &o1, NOW + 10),
        Ok(ProofLevel::SourceProven)
    );
    let n1 = report("N", payload(n, 1, 20, NOW - 60));
    assert!(is_equivocation(&accept(&mut s, &reg, &n1, NOW + 20)));
    let later = NOW + 2 * 86_400;
    s.mark_compromised(&admin, &n, NOW, later, "N stolen")
        .expect("mark");
    let fact = signed_fact_key(&o1.payload);
    assert_eq!(
        s.fact_status(&fact),
        Some(&FactStatus::Disputed(vec![o1.hash()]))
    );
    assert!(s.accepted(&o1.hash()).is_none());
    s.reinstate_key(&admin, &o, later, "cleared")
        .expect("reinstate");
    assert_eq!(
        s.resolve_fact(&admin, &fact, Some(o1.hash()), later, "keep o"),
        Err(SignedError::BadResolution)
    );
}

/// L3R3-F4: registering a key is a reviewer action: another store's
/// capability is refused, and every registration is logged.
#[test]
fn registering_a_key_needs_the_reviewer() {
    let reg = reg();
    let (mut s, admin) = store();
    let (_, other) = SignedSources::with_reviewer("someone else");
    assert_eq!(
        s.register_key(&other, source_key("evil"), &reg, NOW, "x"),
        Err(SignedError::NotReviewer)
    );
    assert!(s.decisions().is_empty());
    let id = s
        .register_key(&admin, source_key("honest"), &reg, NOW, "reviewed origin")
        .expect("reg");
    let d = s.decisions().last().expect("logged");
    assert_eq!(d.by, "lifecycle reviewer");
    assert_eq!(d.reason, "reviewed origin");
    assert!(matches!(d.action, DecisionAction::Register { key, .. } if key == id));
}

/// L3R3-F6: a report held past the window as dispute evidence is released
/// by the next sweep after the dispute ends; an exact replay of a pruned
/// report is refused as stale while the clock runs forward.
#[test]
fn held_evidence_is_released_after_its_dispute_ends() {
    let reg = reg();
    let (mut s, admin) = store();
    let a = s
        .register_key(&admin, source_key("a"), &reg, NOW, "test")
        .expect("reg");
    let b = s
        .register_key(&admin, source_key("b"), &reg, NOW, "test")
        .expect("reg");
    let ra = report("a", payload(a, 1, 1, NOW - 60));
    let rb = report("b", payload(b, 1, 2, NOW - 60));
    assert_eq!(accept(&mut s, &reg, &ra, NOW), Ok(ProofLevel::SourceProven));
    assert!(is_equivocation(&accept(&mut s, &reg, &rb, NOW)));
    let fact = signed_fact_key(&ra.payload);
    let late = NOW + PRUNE_AFTER_SECS + 1_000;
    s.prune(late);
    assert_eq!(s.book_sizes().1, 2, "evidence held while disputed");
    s.reinstate_key(&admin, &a, late, "a").expect("reinstate");
    s.resolve_fact(&admin, &fact, None, late, "clear")
        .expect("resolve");
    s.prune(late + 1);
    // `ra` is still on the re-review list (withdrawn); `rb` is released.
    assert_eq!(s.book_sizes().1, 1);
    s.reinstate_key(&admin, &b, late + 2, "b")
        .expect("reinstate");
    assert_eq!(accept(&mut s, &reg, &rb, late + 3), Err(SignedError::Stale));
}

/// L3R2-F4: inside the 1 h grace a retired key can sign only reports observed
/// within an hour before its retirement; `at` cannot lie in the future, and
/// a retirement is final.
#[test]
fn a_retired_key_cannot_backdate_a_week() {
    let reg = reg();
    let (mut s, admin) = store();
    let io = s
        .register_key(&admin, source_key("old"), &reg, NOW, "test")
        .expect("reg");
    assert_eq!(
        s.retire_key(&admin, &io, NOW + 1, NOW, "future"),
        Err(SignedError::TimeInFuture)
    );
    s.retire_key(&admin, &io, NOW, NOW, "rotation")
        .expect("retire");
    let forged = report("old", payload(io, 7, 5, NOW - 6 * 86_400));
    assert_eq!(
        accept(&mut s, &reg, &forged, NOW + 100),
        Err(SignedError::KeyRetired)
    );
    let edge = report("old", payload(io, 8, 5, NOW - RETIRED_GRACE_SECS - 1));
    assert_eq!(
        accept(&mut s, &reg, &edge, NOW + 100),
        Err(SignedError::KeyRetired)
    );
    let late = report("old", payload(io, 9, 5, NOW - RETIRED_GRACE_SECS));
    assert_eq!(
        accept(&mut s, &reg, &late, NOW + 100),
        Ok(ProofLevel::SourceProven)
    );
    assert_eq!(
        s.retire_key(&admin, &io, NOW - 10, NOW + 200, "again"),
        Err(SignedError::NotActive)
    );
}

/// L3R2-F5: restatements are not stored, and the sequence and report books
/// stay bounded by the 7-day window while time runs on.
#[test]
fn the_books_stay_bounded() {
    let reg = reg();
    let (mut s, reviewer) = SignedSources::with_reviewer("test reviewer");
    let id = s
        .register_key(&reviewer, source_key("k"), &reg, NOW, "test")
        .expect("reg");
    let first = report("k", payload(id, 0, 1, NOW - 60));
    assert_eq!(
        accept(&mut s, &reg, &first, NOW),
        Ok(ProofLevel::SourceProven)
    );
    let before = s.book_sizes();
    for i in 1..200 {
        let r = report("k", payload(id, i, 1, NOW - 60));
        assert_eq!(
            accept(&mut s, &reg, &r, NOW),
            Err(SignedError::AlreadyStated)
        );
    }
    assert_eq!(s.book_sizes(), before);
    assert_eq!(s.key_status(&id), Some(KeyStatus::Active));
    // Thirty days, one report every 30 minutes: the books hold about one
    // window's worth (336 reports), not 1,440.
    let (mut s, reviewer) = SignedSources::with_reviewer("test reviewer");
    let id = s
        .register_key(&reviewer, source_key("k"), &reg, NOW, "test")
        .expect("reg");
    let step = 1_800;
    let mut max = (0, 0);
    for i in 0..1_440u64 {
        let t = NOW + i * step;
        let r = report("k", payload(id, i, u128::from(i), t - 5));
        assert_eq!(accept(&mut s, &reg, &r, t), Ok(ProofLevel::SourceProven));
        let (a, b) = s.book_sizes();
        max = (max.0.max(a), max.1.max(b));
    }
    let window = (MAX_REPORT_AGE_SECS + 300) / step + 2;
    assert!(
        max.0 as u64 <= window && max.1 as u64 <= window,
        "{max:?} > {window}"
    );
}

/// L3R2-F6: reviewer actions need this store's capability; a resolution may
/// keep only a report whose key may sign it now.
#[test]
fn reviewer_actions_need_this_stores_capability() {
    let reg = reg();
    let (mut s, admin) = store();
    let (_, other) = SignedSources::with_reviewer("someone else");
    assert_eq!(other.name(), "someone else");
    let ia = s
        .register_key(&admin, source_key("a"), &reg, NOW, "test")
        .expect("reg");
    let ib = s
        .register_key(&admin, source_key("b"), &reg, NOW, "test")
        .expect("reg");
    assert_eq!(
        s.retire_key(&other, &ia, NOW, NOW, "x"),
        Err(SignedError::NotReviewer)
    );
    assert_eq!(
        s.mark_compromised(&other, &ia, NOW, NOW, "x"),
        Err(SignedError::NotReviewer)
    );
    let ra = report("a", payload(ia, 1, 1, NOW - 60));
    let rb = report("b", payload(ib, 1, 2, NOW - 60));
    assert_eq!(accept(&mut s, &reg, &ra, NOW), Ok(ProofLevel::SourceProven));
    assert!(is_equivocation(&accept(&mut s, &reg, &rb, NOW)));
    let fact = signed_fact_key(&ra.payload);
    assert_eq!(
        s.resolve_fact(&other, &fact, None, NOW, "x"),
        Err(SignedError::NotReviewer)
    );
    assert_eq!(
        s.reinstate_key(&other, &ib, NOW, "x"),
        Err(SignedError::NotReviewer)
    );
    // B is suspended: its report cannot be kept until B is reinstated.
    assert_eq!(
        s.resolve_fact(&admin, &fact, Some(rb.hash()), NOW + 1, "keep b"),
        Err(SignedError::BadResolution)
    );
    s.reinstate_key(&admin, &ib, NOW + 2, "b is the publisher's current key")
        .expect("reinstate");
    s.resolve_fact(&admin, &fact, Some(rb.hash()), NOW + 3, "keep b")
        .expect("resolve");
    assert_eq!(s.fact_status(&fact), Some(&FactStatus::Accepted(rb.hash())));
    let by: Vec<_> = s.decisions().iter().map(|d| d.by.as_str()).collect();
    // Two registrations, the reinstatement and the resolution.
    assert_eq!(by, vec!["lifecycle reviewer"; 4]);
}

/// L3R2-F7: the intake names the fact; an equivocation names what it
/// withdrew; a fact is settleable only after the dispute window.
#[test]
fn a_fact_is_settleable_only_after_the_dispute_window() {
    let reg = reg();
    let (mut s, reviewer) = SignedSources::with_reviewer("test reviewer");
    let ia = s
        .register_key(&reviewer, source_key("a"), &reg, NOW, "test")
        .expect("reg");
    let ib = s
        .register_key(&reviewer, source_key("b"), &reg, NOW, "test")
        .expect("reg");
    let ra = report("a", payload(ia, 1, 1, NOW - 60));
    let intake = s
        .accept_claim(&claim_for(&ra), &ra, &reg, NOW)
        .expect("accept");
    let fact = signed_fact_key(&ra.payload);
    assert_eq!((intake.fact, intake.report), (fact, ra.hash()));
    assert!(s.settleable(&fact, NOW).is_none());
    assert!(s.settleable(&fact, NOW + DISPUTE_WINDOW_SECS - 1).is_none());
    assert_eq!(
        s.settleable(&fact, NOW + DISPUTE_WINDOW_SECS)
            .map(|a| a.report.hash()),
        Some(ra.hash())
    );
    let rb = report("b", payload(ib, 1, 2, NOW - 60));
    assert_eq!(
        s.accept_claim(&claim_for(&rb), &rb, &reg, NOW + 10),
        Err(SignedError::Equivocation {
            fact,
            withdrawn: vec![ra.hash()],
        })
    );
    assert!(s
        .settleable(&fact, NOW + 10 * DISPUTE_WINDOW_SECS)
        .is_none());
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Ev {
    Fresh,
    Backdated,
    Retire,
    Compromise,
    Contradict,
    Reinstate,
    Advance,
}

const EVENTS: [Ev; 7] = [
    Ev::Fresh,
    Ev::Backdated,
    Ev::Retire,
    Ev::Compromise,
    Ev::Contradict,
    Ev::Reinstate,
    Ev::Advance,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Life {
    Active,
    Retired(u64),
    Compromised,
}

#[derive(Clone, Copy, Debug)]
struct Model {
    life: Life,
    suspended: bool,
    t: u64,
}

impl Model {
    /// The reference rule: may the key sign a report observed at `obs` now?
    fn permits(&self, obs: u64) -> Result<(), SignedError> {
        match self.life {
            Life::Active => {}
            Life::Retired(at)
                if obs < at
                    && obs >= at.saturating_sub(RETIRED_GRACE_SECS)
                    && self.t < at + RETIRED_GRACE_SECS => {}
            Life::Retired(_) => return Err(SignedError::KeyRetired),
            Life::Compromised => return Err(SignedError::KeyCompromised),
        }
        if self.suspended {
            return Err(SignedError::KeySuspended);
        }
        Ok(())
    }

    /// State class for the coverage table.
    fn class(&self) -> (u8, bool) {
        let c = match self.life {
            Life::Active => 0,
            Life::Retired(at) if self.t < at + RETIRED_GRACE_SECS => 1,
            Life::Retired(_) => 2,
            Life::Compromised => 3,
        };
        (c, self.suspended)
    }
}

const DEPTH: usize = 5;

/// One fixed setup: key K with one accepted fact per step (for the
/// contradictions).
fn setup(reg: &StaticRegistry) -> (SignedSources, Reviewer, Hash32) {
    let (mut s, admin) = SignedSources::with_reviewer("exhaustive");
    let k = s
        .register_key(&admin, source_key("K"), reg, NOW, "test")
        .expect("K");
    for j in 1..=DEPTH as u64 {
        let r = report("K", payload(k, j, 1, NOW - 100 - j));
        assert_eq!(accept(&mut s, reg, &r, NOW), Ok(ProofLevel::SourceProven));
    }
    (s, admin, k)
}

fn check(s: &SignedSources, k: &Hash32, m: &Model, path: &[Ev]) {
    let st = s.key_status(k).expect("K");
    let want = match m.life {
        Life::Active => KeyStatus::Active,
        Life::Retired(at) => KeyStatus::Retired { at },
        Life::Compromised => match st {
            KeyStatus::Compromised { .. } => st,
            _ => panic!("{path:?}: {st:?}, want compromised"),
        },
    };
    assert_eq!(st, want, "{path:?}");
    assert_eq!(s.is_suspended(k), m.suspended, "{path:?}");
}

/// Every sequence of up to `DEPTH` events from the seven below, against the
/// reference model: the store's life cycle and suspension match the model
/// after each event; a report is accepted exactly when the model's rule
/// permits it (life cycle at the report's observation time and at arrival,
/// no suspension); and no sequence of reviewer actions moves the life cycle
/// back (the model's life only moves forward). Every (state class, event)
/// pair is reached.
#[test]
fn every_state_and_event_pair() {
    let reg = reg();
    let mut covered = BTreeSet::new();
    let mut paths = 0usize;
    let mut path = Vec::with_capacity(DEPTH);
    fn walk(
        reg: &StaticRegistry,
        path: &mut Vec<Ev>,
        covered: &mut BTreeSet<((u8, bool), Ev)>,
        paths: &mut usize,
    ) {
        // Replay the path on a fresh store, checking each step.
        let (mut s, admin, k) = setup(reg);
        let mut m = Model {
            life: Life::Active,
            suspended: false,
            t: NOW,
        };
        let mut rank = 0u8;
        for (i, ev) in path.iter().enumerate() {
            let step = i as u64 + 1;
            covered.insert((m.class(), *ev));
            match ev {
                Ev::Fresh | Ev::Backdated => {
                    let (obs, seq) = if *ev == Ev::Fresh {
                        (m.t - 10 - step, 100 + step)
                    } else {
                        (m.t - 6 * 86_400 + step, 200 + step)
                    };
                    let r = report("K", payload(k, seq, 7, obs));
                    let got = accept(&mut s, reg, &r, m.t);
                    let want = m.permits(obs).map(|()| ProofLevel::SourceProven);
                    assert_eq!(got, want, "{path:?} step {step}");
                }
                Ev::Retire => {
                    let got = s.retire_key(&admin, &k, m.t, m.t, "rotate");
                    if m.life == Life::Active {
                        assert_eq!(got, Ok(()), "{path:?}");
                        m.life = Life::Retired(m.t);
                    } else {
                        assert_eq!(got, Err(SignedError::NotActive), "{path:?}");
                    }
                }
                Ev::Compromise => {
                    s.mark_compromised(&admin, &k, 0, m.t, "stolen")
                        .expect("compromise");
                    m.life = Life::Compromised;
                }
                Ev::Contradict => {
                    let label = format!("H{step}");
                    let h = s
                        .register_key(&admin, source_key(&label), reg, m.t, "test")
                        .expect("helper");
                    let r = report(&label, payload(h, 1, 2, NOW - 100 - step));
                    let got = accept(&mut s, reg, &r, m.t);
                    if m.life == Life::Compromised {
                        // K's fact was withdrawn: the helper's report stands.
                        assert_eq!(got, Ok(ProofLevel::SourceProven), "{path:?}");
                    } else {
                        assert!(is_equivocation(&got), "{path:?}: {got:?}");
                        m.suspended = true;
                    }
                }
                Ev::Reinstate => {
                    let got = s.reinstate_key(&admin, &k, m.t, "cleared");
                    if m.suspended {
                        assert_eq!(got, Ok(()), "{path:?}");
                        m.suspended = false;
                    } else {
                        assert_eq!(got, Err(SignedError::NotSuspended), "{path:?}");
                    }
                }
                Ev::Advance => m.t += RETIRED_GRACE_SECS + 1,
            }
            check(&s, &k, &m, path);
            let r = s.key_status(&k).expect("K").rank();
            assert!(r >= rank, "{path:?}: life cycle moved back");
            rank = r;
        }
        // Probe: a fresh fact now.
        let obs = m.t - 1;
        let probe = report("K", payload(k, 999, 3, obs));
        assert_eq!(
            accept(&mut s, reg, &probe, m.t),
            m.permits(obs).map(|()| ProofLevel::SourceProven),
            "{path:?} probe"
        );
        *paths += 1;
        if path.len() < DEPTH {
            for ev in EVENTS {
                path.push(ev);
                walk(reg, path, covered, paths);
                path.pop();
            }
        }
    }
    walk(&reg, &mut path, &mut covered, &mut paths);
    assert_eq!(
        paths,
        (0..=DEPTH as u32).map(|d| 7usize.pow(d)).sum::<usize>()
    );
    // 4 life-cycle classes x suspended or not x 7 events.
    assert_eq!(covered.len(), 4 * 2 * EVENTS.len(), "{covered:?}");
}
